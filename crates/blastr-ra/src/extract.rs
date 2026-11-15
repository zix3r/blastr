//! Walking the loaded crates and assembling the revision graph.
//!
//! The walk starts at every workspace-member crate root and follows the module
//! tree: the declarations of a module, the items of every trait and impl block,
//! the fields and variants of every type, and the items declared inside
//! function bodies. Items that a macro or a derive produced are reached through
//! the module's definitions as well; their location is the range of the macro
//! call or derive attribute in the real file, and their hashes are taken over
//! the expanded syntax.
//!
//! Symbol ids are assigned after the whole walk, in a deterministic order, so
//! the same project always produces the same ids.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use blastr_model::{
    CancelToken, Certainty, CrateId, Edge, EdgeDetail, EdgeKind, ImplInfo, Location, Progress,
    RevisionGraph, Symbol, SymbolFlags, SymbolId, SymbolKind, SymbolPath, TargetKind,
    Visibility as ModelVisibility,
};
use ra_ap_hir::{
    Adt, AnyDiagnostic, AssocItem, Crate, EnumVariant, Field, FieldSource, Function, HasSource,
    HasVisibility, HirFileId, Impl, InFile, Module, ModuleDef, Semantics, Trait,
    Visibility as HirVisibility, attach_db, db::HirDatabase,
};
use ra_ap_ide_db::base_db::SourceDatabase;
use ra_ap_syntax::ast;
use ra_ap_syntax::{AstNode, SyntaxNode, TextRange, TextSize};
use ra_ap_vfs::FileId;

use crate::edges::{self, Container, Input as EdgeInput, ResolvedCrate};
use crate::hashing;
use crate::load::MemberCrate;
use crate::locate::Locator;
use crate::symbols::{self, DefKey, Hashes, Raw, SymbolTable};
use crate::{Error, Workspace, check_cancel, paths};

impl Workspace {
    /// Extracts the graph of the project as currently loaded.
    pub fn extract(
        &self,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<RevisionGraph, Error> {
        let database = self.host.raw_database();
        let db: &dyn HirDatabase = database;
        let source_db: &dyn SourceDatabase = database;
        let locator = Locator::new(source_db, &self.vfs, &self.root);
        attach_db(db, || {
            let mut extractor = Extractor::new(db, &locator, cancel, progress, self);
            extractor.run()?;
            Ok(extractor.finish())
        })
    }
}

/// What the block a definition is declared in looks like.
#[derive(Debug, Clone, Copy, Default)]
struct Scope {
    /// The definition is declared inside a trait block.
    in_trait: bool,
    /// The definition is declared inside an impl block for a trait.
    in_trait_impl: bool,
}

/// A definition together with the syntax of its body in the file it came from.
struct Source<A> {
    /// The file the syntax nodes belong to, which may be a macro expansion.
    file: HirFileId,
    /// The syntax of the item, absent when only a range is known (derive impls).
    node: Option<InFile<A>>,
    /// Where the item is written, in a real file.
    location: Location,
    /// Whether a macro or a derive produced the item.
    from_macro: bool,
}

impl<A: AstNode> Source<A> {
    /// The syntax of the item.
    fn ast(&self) -> Option<&A> {
        self.node.as_ref().map(|node| &node.value)
    }

    /// The syntax node of the item.
    fn syntax(&self) -> Option<&SyntaxNode> {
        self.ast().map(|node| node.syntax())
    }
}

/// One region of code whose references belong to one symbol, before the ids
/// are assigned.
struct PendingContainer {
    /// Index into [`Extractor::raws`] of the symbol the region belongs to.
    raw: usize,
    /// The definition the region belongs to, when it is a definition.
    def: Option<DefKey>,
    /// The file of `node`.
    file: HirFileId,
    /// The syntax that holds the references of the symbol.
    node: SyntaxNode,
}

struct Extractor<'a> {
    /// The database, attached to the current thread.
    db: &'a dyn HirDatabase,
    /// Maps the syntax of nested items to their definitions.
    sema: Semantics<'a, dyn HirDatabase>,
    /// Maps file ids and ranges to locations.
    locator: &'a Locator<'a>,
    /// Asks the caller to stop.
    cancel: &'a CancelToken,
    /// Tells the caller how far the extraction is.
    progress: &'a dyn Progress,
    /// The workspace, for its member crates and its execution mode.
    workspace: &'a Workspace,
    /// The crates of the graph that were found in the database.
    crates: Vec<ResolvedCrate>,
    /// The symbols, before ids are assigned.
    raws: Vec<Raw>,
    /// Where each definition's symbol is in [`Extractor::raws`].
    defs: HashMap<DefKey, usize>,
    /// Inactive `cfg` ranges, in no particular order.
    inactive: Vec<Location>,
    /// The regions the dependency pass walks.
    containers: Vec<PendingContainer>,
    /// Index into `Workspace::crates` of the crate being walked.
    krate: usize,
}

impl<'a> Extractor<'a> {
    fn new(
        db: &'a dyn HirDatabase,
        locator: &'a Locator<'a>,
        cancel: &'a CancelToken,
        progress: &'a dyn Progress,
        workspace: &'a Workspace,
    ) -> Extractor<'a> {
        Extractor {
            db,
            sema: Semantics::new_dyn(db),
            locator,
            cancel,
            progress,
            workspace,
            crates: Vec::new(),
            raws: Vec::new(),
            defs: HashMap::new(),
            inactive: Vec::new(),
            containers: Vec::new(),
            krate: 0,
        }
    }

    /// Walks every workspace-member crate.
    fn run(&mut self) -> Result<(), Error> {
        let workspace: &'a Workspace = self.workspace;
        let members = &workspace.crates;
        let candidates = candidates_by_root(self.db, members);
        let total = members.len() as u64;
        for (index, member) in members.iter().enumerate() {
            check_cancel(self.cancel)?;
            self.progress.report("extract", index as u64, Some(total));
            let Some(krate) = find_crate(&candidates, member, self.db) else {
                continue;
            };
            let resolved = ResolvedCrate {
                index,
                krate,
                is_bin: member.info.target_kind == TargetKind::Bin,
            };
            self.crates.push(resolved);
            self.krate = resolved.index;
            let name = member.info.name.clone();
            self.walk_crate(resolved.krate, &name)?;
        }
        self.progress.report("extract", total, Some(total));
        Ok(())
    }

    /// Adds the crate root module and everything below it.
    fn walk_crate(&mut self, krate: Crate, crate_name: &str) -> Result<(), Error> {
        let root = krate.root_module(self.db);
        let Some(location) = self.root_location(krate) else {
            return Ok(());
        };
        let raw = Raw::new(
            SymbolKind::Module,
            crate_name.to_owned(),
            self.krate,
            location,
            ModelVisibility::Public,
            symbols::module_hashes(),
            None,
        );
        let index = self.push(raw, Some(DefKey::Module(root)));
        self.walk_module(root, crate_name, Some(index), Scope::default())
    }

    /// Adds the items of one module, and of its impl blocks.
    fn walk_module(
        &mut self,
        module: Module,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        check_cancel(self.cancel)?;
        self.collect_inactive(module);
        for def in module.declarations(self.db) {
            self.add_def(def, path, parent, scope)?;
        }
        // A `macro_rules!` macro without `#[macro_export]` is textually scoped
        // and not among the declarations. The legacy scope also holds the
        // macros inherited from enclosing modules and from `#[macro_use]`
        // crates, so only the ones defined in this module are taken.
        for item in module.legacy_macros(self.db) {
            if item.module(self.db) == module {
                self.add_macro(item, path, parent, scope)?;
            }
        }
        for block in module.impl_defs(self.db) {
            self.add_impl(block, path, parent)?;
        }
        Ok(())
    }

    /// Adds one definition that can appear in a module's declarations.
    fn add_def(
        &mut self,
        def: ModuleDef,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        match def {
            ModuleDef::Module(module) => self.add_module(module, path, parent, scope),
            ModuleDef::Function(function) => self.add_function(function, path, parent, scope),
            ModuleDef::Adt(adt) => self.add_adt(adt, path, parent, scope),
            ModuleDef::Const(item) => self.add_const(item, path, parent, scope),
            ModuleDef::Static(item) => self.add_static(item, path, parent, scope),
            ModuleDef::Trait(item) => self.add_trait(item, path, parent, scope),
            ModuleDef::TypeAlias(item) => self.add_type_alias(item, path, parent, scope),
            ModuleDef::Macro(item) => self.add_macro(item, path, parent, scope),
            // Enum variants are reached through their enum, and builtin types
            // are not written anywhere.
            ModuleDef::EnumVariant(_) | ModuleDef::BuiltinType(_) => Ok(()),
        }
    }

    /// Adds an item declared inside a trait or an impl block.
    fn add_assoc_item(
        &mut self,
        item: AssocItem,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        match item {
            AssocItem::Function(function) => self.add_function(function, path, parent, scope),
            AssocItem::Const(item) => self.add_const(item, path, parent, scope),
            AssocItem::TypeAlias(item) => self.add_type_alias(item, path, parent, scope),
        }
    }

    fn add_module(
        &mut self,
        module: Module,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::Module(module);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let Some(name) = module.name(self.db) else {
            return Ok(());
        };
        if name.is_missing() {
            return Ok(());
        }
        let Some(location) = self.module_location(module) else {
            return Ok(());
        };
        let path = paths::child(path, name.as_str());
        let raw = Raw::new(
            SymbolKind::Module,
            path.clone(),
            self.krate,
            location,
            self.model_visibility(module.visibility(self.db), module),
            symbols::module_hashes(),
            parent,
        );
        let index = self.push(raw, Some(key));
        self.walk_module(module, &path, Some(index), scope)
    }

    fn add_function(
        &mut self,
        function: Function,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::Function(function);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let Some(source) = self
            .guarded(|| function.source_with_range(self.db))
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let name = function.name(self.db);
        if name.is_missing() {
            return Ok(());
        }
        let kind = if function.has_self_param(self.db) {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        let hashes = source
            .ast()
            .map(symbols::function_hashes)
            .unwrap_or_else(Hashes::none);
        let is_test = hashes.is_test;
        let has_body = hashes.has_body;
        let path = paths::child(path, name.as_str());
        let visibility = function.visibility(self.db);
        let module = function.module(self.db);
        let mut raw = Raw::new(
            kind,
            path.clone(),
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_test,
            is_main: self.is_main(name.as_str(), kind, parent),
            is_pub_api: self.is_pub_api(visibility, module),
            has_body,
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            from_macro: source.from_macro,
        };
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, source.syntax());
        if let Some(body) = source.ast().and_then(|item| item.body()) {
            self.walk_nested(source.file, body.syntax(), &path, index, scope)?;
        }
        Ok(())
    }

    fn add_adt(
        &mut self,
        adt: Adt,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::Adt(adt);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let (kind, name, module, visibility) = match adt {
            Adt::Struct(it) => (
                SymbolKind::Struct,
                it.name(self.db),
                it.module(self.db),
                it.visibility(self.db),
            ),
            Adt::Enum(it) => (
                SymbolKind::Enum,
                it.name(self.db),
                it.module(self.db),
                it.visibility(self.db),
            ),
            Adt::Union(it) => (
                SymbolKind::Union,
                it.name(self.db),
                it.module(self.db),
                it.visibility(self.db),
            ),
        };
        if name.is_missing() {
            return Ok(());
        }
        let Some(source) = self
            .guarded(|| adt.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .syntax()
            .map(|node| symbols::adt_hashes(kind, node))
            .unwrap_or_else(Hashes::none);
        let path = paths::child(path, name.as_str());
        let mut raw = Raw::new(
            kind,
            path.clone(),
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            from_macro: source.from_macro,
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            ..SymbolFlags::default()
        };
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, source.syntax());
        match adt {
            Adt::Struct(it) => {
                for field in it.fields(self.db) {
                    self.add_field(field, module, &path, Some(index), scope)?;
                }
            }
            Adt::Union(it) => {
                for field in it.fields(self.db) {
                    self.add_field(field, module, &path, Some(index), scope)?;
                }
            }
            Adt::Enum(it) => {
                for variant in it.variants(self.db) {
                    self.add_variant(variant, &path, Some(index), scope)?;
                }
            }
        }
        Ok(())
    }

    fn add_variant(
        &mut self,
        variant: EnumVariant,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::Variant(variant);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let name = variant.name(self.db);
        if name.is_missing() {
            return Ok(());
        }
        let module = variant.module(self.db);
        let visibility = variant.visibility(self.db);
        let Some(source) = self
            .guarded(|| variant.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(symbols::variant_hashes)
            .unwrap_or_else(Hashes::none);
        let path = paths::child(path, name.as_str());
        let mut raw = Raw::new(
            SymbolKind::Variant,
            path.clone(),
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            from_macro: source.from_macro,
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            ..SymbolFlags::default()
        };
        let index = self.push(raw, Some(key));
        for field in variant.fields(self.db) {
            self.add_field(field, module, &path, Some(index), scope)?;
        }
        Ok(())
    }

    fn add_field(
        &mut self,
        field: Field,
        module: Module,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::Field(field);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let Some(source) = self
            .guarded(|| field.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source.ast().map(source_hashes).unwrap_or_else(Hashes::none);
        let name = field.name(self.db);
        let name = if name.is_missing() {
            field.index().to_string()
        } else {
            name.as_str().to_owned()
        };
        let path = paths::child(path, &name);
        let visibility = field.visibility(self.db);
        let mut raw = Raw::new(
            SymbolKind::Field,
            path,
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            from_macro: source.from_macro,
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            ..SymbolFlags::default()
        };
        let ty = source.ast().and_then(field_type);
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, ty.as_ref());
        Ok(())
    }

    fn add_const(
        &mut self,
        item: ra_ap_hir::Const,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::Const(item);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let Some(name) = item.name(self.db) else {
            return Ok(());
        };
        if name.is_missing() {
            return Ok(());
        }
        let module = item.module(self.db);
        let visibility = item.visibility(self.db);
        let Some(source) = self
            .guarded(|| item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(symbols::const_hashes)
            .unwrap_or_else(Hashes::none);
        let has_body = hashes.has_body;
        let path = paths::child(path, name.as_str());
        let mut raw = Raw::new(
            SymbolKind::Const,
            path,
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            has_body,
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            from_macro: source.from_macro,
            ..SymbolFlags::default()
        };
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, source.syntax());
        Ok(())
    }

    fn add_static(
        &mut self,
        item: ra_ap_hir::Static,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::Static(item);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let name = item.name(self.db);
        if name.is_missing() {
            return Ok(());
        }
        let module = item.module(self.db);
        let visibility = item.visibility(self.db);
        let Some(source) = self
            .guarded(|| item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(symbols::static_hashes)
            .unwrap_or_else(Hashes::none);
        let has_body = hashes.has_body;
        let path = paths::child(path, name.as_str());
        let mut raw = Raw::new(
            SymbolKind::Static,
            path,
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            has_body,
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            from_macro: source.from_macro,
            ..SymbolFlags::default()
        };
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, source.syntax());
        Ok(())
    }

    fn add_type_alias(
        &mut self,
        item: ra_ap_hir::TypeAlias,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::TypeAlias(item);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let name = item.name(self.db);
        if name.is_missing() {
            return Ok(());
        }
        let module = item.module(self.db);
        let visibility = item.visibility(self.db);
        let Some(source) = self
            .guarded(|| item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(symbols::type_alias_hashes)
            .unwrap_or_else(Hashes::none);
        let path = paths::child(path, name.as_str());
        let mut raw = Raw::new(
            SymbolKind::TypeAlias,
            path,
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            from_macro: source.from_macro,
            ..SymbolFlags::default()
        };
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, source.syntax());
        Ok(())
    }

    fn add_trait(
        &mut self,
        item: Trait,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::Trait(item);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let name = item.name(self.db);
        if name.is_missing() {
            return Ok(());
        }
        let module = item.module(self.db);
        let visibility = item.visibility(self.db);
        let Some(source) = self
            .guarded(|| item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(symbols::trait_hashes)
            .unwrap_or_else(Hashes::none);
        let path = paths::child(path, name.as_str());
        let mut raw = Raw::new(
            SymbolKind::Trait,
            path.clone(),
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            from_macro: source.from_macro,
            ..SymbolFlags::default()
        };
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, source.syntax());
        let inner = Scope {
            in_trait: true,
            in_trait_impl: scope.in_trait_impl,
        };
        for item in item.items(self.db) {
            self.add_assoc_item(item, &path, Some(index), inner)?;
        }
        Ok(())
    }

    fn add_macro(
        &mut self,
        item: ra_ap_hir::Macro,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let key = DefKey::Macro(item);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let name = item.name(self.db);
        if name.is_missing() {
            return Ok(());
        }
        let module = item.module(self.db);
        let visibility = item.visibility(self.db);
        let Some(source) = self
            .guarded(|| item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = macro_source_hashes(&source);
        let path = paths::child(path, name.as_str());
        let mut raw = Raw::new(
            SymbolKind::Macro,
            path,
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            from_macro: source.from_macro,
            ..SymbolFlags::default()
        };
        self.push(raw, Some(key));
        Ok(())
    }

    /// Adds an impl block and the items it defines.
    fn add_impl(&mut self, block: Impl, path: &str, parent: Option<usize>) -> Result<(), Error> {
        check_cancel(self.cancel)?;
        let key = DefKey::Impl(block);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let Some(source) = self
            .guarded(|| block.source_with_range(self.db))
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let trait_def = block.trait_(self.db);
        let (trait_text, self_text) = match source.ast() {
            Some(node) => (
                node.trait_()
                    .map(|it| hashing::text_without_ws(it.syntax())),
                node.self_ty()
                    .map(|it| hashing::text_without_ws(it.syntax())),
            ),
            None => (
                trait_def.map(|it| it.name(self.db).as_str().to_owned()),
                self.type_name(block.self_ty(self.db)),
            ),
        };
        let Some(self_text) = self_text else {
            return Ok(());
        };
        let segment = paths::impl_segment(trait_text.as_deref(), &self_text);
        let path = paths::child(path, &segment);
        let info = ImplInfo {
            trait_path: trait_def.map(|it| self.trait_path(it)),
            self_path: self.adt_path(block.self_ty(self.db).as_adt()),
        };
        let hashes = source
            .ast()
            .map(symbols::impl_hashes)
            .unwrap_or_else(Hashes::none);
        let mut raw = Raw::new(
            SymbolKind::Impl,
            path.clone(),
            self.krate,
            source.location.clone(),
            ModelVisibility::Public,
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            in_trait_impl: trait_def.is_some(),
            from_macro: source.from_macro,
            ..SymbolFlags::default()
        };
        raw.impl_info = Some(info.clone());
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, source.syntax());
        let item_path = if trait_def.is_some() {
            path
        } else {
            info.self_path.unwrap_or(path)
        };
        let inner = Scope {
            in_trait: false,
            in_trait_impl: trait_def.is_some(),
        };
        for item in block.items(self.db) {
            self.add_assoc_item(item, &item_path, Some(index), inner)?;
        }
        Ok(())
    }

    /// Adds the items declared inside a function body.
    ///
    /// The items of a block are part of no module's declarations, so they are
    /// found through the syntax of the body and resolved to definitions one by
    /// one. Items that a macro call inside the body produced are not seen.
    fn walk_nested(
        &mut self,
        file: HirFileId,
        body: &SyntaxNode,
        path: &str,
        parent: usize,
        scope: Scope,
    ) -> Result<(), Error> {
        let mut nodes = Vec::new();
        item_nodes(body, &mut nodes);
        for node in nodes {
            check_cancel(self.cancel)?;
            let Some(item) = ast::Item::cast(node) else {
                continue;
            };
            let block = match &item {
                ast::Item::Impl(it) => self.guarded(|| self.sema.to_def2(InFile::new(file, it))),
                _ => None,
            };
            if let Some(block) = block {
                self.add_impl(block, path, Some(parent))?;
                continue;
            }
            if let Some(def) = self.nested_def(file, &item) {
                self.add_def(def, path, Some(parent), scope)?;
            }
        }
        Ok(())
    }

    /// Resolves the syntax of an item in a block to its definition.
    fn nested_def(&self, file: HirFileId, item: &ast::Item) -> Option<ModuleDef> {
        match item {
            ast::Item::Fn(it) => self
                .guarded(|| self.sema.to_def2(InFile::new(file, it)))
                .map(ModuleDef::Function),
            ast::Item::Struct(it) => self
                .guarded(|| self.sema.to_def2(InFile::new(file, it)))
                .map(|it| ModuleDef::Adt(Adt::Struct(it))),
            ast::Item::Enum(it) => self
                .guarded(|| self.sema.to_def2(InFile::new(file, it)))
                .map(|it| ModuleDef::Adt(Adt::Enum(it))),
            ast::Item::Union(it) => self
                .guarded(|| self.sema.to_def2(InFile::new(file, it)))
                .map(|it| ModuleDef::Adt(Adt::Union(it))),
            ast::Item::Trait(it) => self
                .guarded(|| self.sema.to_def2(InFile::new(file, it)))
                .map(ModuleDef::Trait),
            ast::Item::Const(it) => self
                .guarded(|| self.sema.to_def2(InFile::new(file, it)))
                .map(ModuleDef::Const),
            ast::Item::Static(it) => self
                .guarded(|| self.sema.to_def2(InFile::new(file, it)))
                .map(ModuleDef::Static),
            ast::Item::TypeAlias(it) => self
                .guarded(|| self.sema.to_def2(InFile::new(file, it)))
                .map(ModuleDef::TypeAlias),
            ast::Item::Module(it) => self
                .guarded(|| self.sema.to_def2(InFile::new(file, it)))
                .map(ModuleDef::Module),
            ast::Item::MacroRules(it) => {
                let item = ast::Macro::MacroRules(it.clone());
                self.guarded(|| self.sema.to_def2(InFile::new(file, &item)))
                    .map(ModuleDef::Macro)
            }
            ast::Item::MacroDef(it) => {
                let item = ast::Macro::MacroDef(it.clone());
                self.guarded(|| self.sema.to_def2(InFile::new(file, &item)))
                    .map(ModuleDef::Macro)
            }
            _ => None,
        }
    }

    /// The canonical path of a trait, in a member crate or outside the project.
    fn trait_path(&self, item: Trait) -> String {
        let module = item.module(self.db);
        let name = item.name(self.db);
        self.definition_path(module.krate(self.db), module, name.as_str())
    }

    /// The canonical path of a type, when it is a struct, enum or union.
    fn adt_path(&self, adt: Option<Adt>) -> Option<String> {
        let adt = adt?;
        let (module, name) = match adt {
            Adt::Struct(it) => (it.module(self.db), it.name(self.db)),
            Adt::Enum(it) => (it.module(self.db), it.name(self.db)),
            Adt::Union(it) => (it.module(self.db), it.name(self.db)),
        };
        Some(self.definition_path(module.krate(self.db), module, name.as_str()))
    }

    /// The name of a type, used when a derive-generated impl has no source text
    /// to copy its self type from.
    fn type_name(&self, ty: ra_ap_hir::Type<'_>) -> Option<String> {
        let adt = ty.as_adt()?;
        let name = match adt {
            Adt::Struct(it) => it.name(self.db),
            Adt::Enum(it) => it.name(self.db),
            Adt::Union(it) => it.name(self.db),
        };
        Some(name.as_str().to_owned())
    }

    /// The canonical path of a definition, built from its crate and module
    /// instead of from the walk, so that it does not depend on the walk order.
    fn definition_path(&self, krate: Crate, module: Module, name: &str) -> String {
        let mut path = self.crate_name(krate);
        for segment in module.path_segments(self.db) {
            path = paths::child(&path, segment.as_str());
        }
        paths::child(&path, name)
    }

    /// The name a crate goes by in paths.
    fn crate_name(&self, krate: Crate) -> String {
        let root_file = krate.root_file(self.db);
        if let Some(member) = self
            .workspace
            .crates
            .iter()
            .find(|member| member.root_file == root_file)
        {
            return member.info.name.clone();
        }
        krate
            .display_name(self.db)
            .map(|name| name.canonical_name().as_str().replace('-', "_"))
            .unwrap_or_default()
    }

    /// Whether the item is the `main` of a binary target.
    fn is_main(&self, name: &str, kind: SymbolKind, parent: Option<usize>) -> bool {
        let Some(resolved) = self.resolved(self.krate) else {
            return false;
        };
        if !resolved.is_bin || kind != SymbolKind::Function || name != "main" {
            return false;
        }
        parent.is_some_and(|parent| {
            self.raws[parent].kind == SymbolKind::Module && self.raws[parent].parent.is_none()
        })
    }

    /// The crate of the graph whose `CrateInfo` is at `index`.
    fn resolved(&self, index: usize) -> Option<&ResolvedCrate> {
        self.crates.iter().find(|crate_| crate_.index == index)
    }

    /// Whether the item is public API of a library target: `pub`, in a library,
    /// with every enclosing module `pub`.
    fn is_pub_api(&self, visibility: HirVisibility, module: Module) -> bool {
        if !matches!(
            self.current_target_kind(),
            TargetKind::Lib | TargetKind::ProcMacro
        ) || visibility != HirVisibility::Public
        {
            return false;
        }
        let mut module = module;
        while let Some(parent) = module.parent(self.db) {
            if module.visibility(self.db) != HirVisibility::Public {
                return false;
            }
            module = parent;
        }
        true
    }

    fn current_target_kind(&self) -> TargetKind {
        // `krate` is set to an index of `Workspace::crates` before anything is
        // walked for that crate, so it cannot be out of range here.
        self.workspace.crates[self.krate].info.target_kind
    }

    /// The declared visibility, as the graph reports it.
    fn model_visibility(&self, visibility: HirVisibility, module: Module) -> ModelVisibility {
        match visibility {
            HirVisibility::Public => ModelVisibility::Public,
            HirVisibility::PubCrate(_) => ModelVisibility::Crate,
            HirVisibility::Module(owner, _) => {
                if Module::from(owner) == module {
                    ModelVisibility::Private
                } else {
                    ModelVisibility::Restricted
                }
            }
        }
    }

    /// The location of a module: the `mod` declaration if it has one, and the
    /// start of the crate root file for the crate root.
    fn module_location(&self, module: Module) -> Option<Location> {
        if let Some(location) = self
            .guarded(|| module.declaration_source_range(self.db))
            .and_then(|range| self.locate(range.file_id, range.value))
        {
            return Some(location);
        }
        self.root_location(module.krate(self.db))
    }

    /// The first position of the crate root file, for the crate root module.
    fn root_location(&self, krate: Crate) -> Option<Location> {
        let start = TextRange::empty(TextSize::from(0u32));
        self.locator
            .location_in_file(krate.root_file(self.db), start)
    }

    /// Records the ranges of the code of `module` that is inactive under the
    /// analysed configuration.
    fn collect_inactive(&mut self, module: Module) {
        let mut diagnostics = Vec::new();
        let done = catch_unwind(AssertUnwindSafe(|| {
            module.diagnostics(self.db, &mut diagnostics, false);
        }));
        if done.is_err() {
            return;
        }
        for diagnostic in diagnostics {
            let AnyDiagnostic::InactiveCode(code) = diagnostic else {
                continue;
            };
            let range = code.node.map(|pointer| pointer.text_range());
            if let Some(location) = self.locate(range.file_id, range.value) {
                self.inactive.push(location);
            }
        }
    }

    /// Maps a range that may live in a macro expansion back to the real file.
    fn locate(&self, file: HirFileId, range: TextRange) -> Option<Location> {
        self.guarded(|| self.locator.location(file, range))
    }

    /// Runs a rust-analyzer query, treating a panic as "unknown".
    ///
    /// rust-analyzer uses unwinding to cancel queries and can panic on syntax it
    /// cannot map, for example inside a broken macro expansion. One such item
    /// must not take the whole extraction down, and every caller can do without
    /// the answer, so a panic becomes `None`.
    fn guarded<T>(&self, query: impl FnOnce() -> Option<T>) -> Option<T> {
        catch_unwind(AssertUnwindSafe(query)).ok().flatten()
    }

    /// Turns the syntax of a definition into its source, with a location.
    fn resolve_source<A: AstNode>(
        &self,
        source: InFile<(TextRange, Option<A>)>,
    ) -> Option<Source<A>> {
        let file = source.file_id;
        let (range, node) = source.value;
        let location = self.locate(file, range)?;
        let from_macro = file.macro_file().is_some() || node.is_none();
        Some(Source {
            file,
            node: node.map(|node| InFile::new(file, node)),
            location,
            from_macro,
        })
    }

    fn push(&mut self, raw: Raw, key: Option<DefKey>) -> usize {
        let index = self.raws.len();
        self.raws.push(raw);
        if let Some(key) = key {
            self.defs.insert(key, index);
        }
        index
    }

    /// Records the region of code whose references belong to one symbol.
    ///
    /// A definition without source of its own (an impl block a derive produced)
    /// has nothing to walk.
    fn region(
        &mut self,
        raw: usize,
        def: Option<DefKey>,
        file: HirFileId,
        node: Option<&SyntaxNode>,
    ) {
        let Some(node) = node else {
            return;
        };
        self.containers.push(PendingContainer {
            raw,
            def,
            file,
            node: node.clone(),
        });
    }

    /// Assigns the ids, builds the symbols and the `Contains` edges, and hands
    /// the result to the dependency-edge pass.
    fn finish(mut self) -> RevisionGraph {
        let mut order: Vec<usize> = (0..self.raws.len()).collect();
        order.sort_by(|&left, &right| raw_key(&self.raws[left]).cmp(&raw_key(&self.raws[right])));

        let mut id_of_raw = vec![SymbolId(0); self.raws.len()];
        for (id, &index) in order.iter().enumerate() {
            id_of_raw[index] = SymbolId(id as u32);
        }

        let mut counters: HashMap<(String, SymbolKind), u32> = HashMap::new();
        let mut symbols = Vec::with_capacity(order.len());
        for &index in &order {
            let raw = &self.raws[index];
            let counter = counters.entry((raw.path.clone(), raw.kind)).or_insert(0);
            let disambiguator = *counter;
            *counter += 1;
            symbols.push(Symbol {
                path: SymbolPath {
                    path: raw.path.clone(),
                    kind: raw.kind,
                    disambiguator,
                },
                krate: CrateId(raw.krate as u32),
                location: raw.location.clone(),
                visibility: raw.visibility,
                signature_hash: raw.signature_hash,
                body_hash: raw.body_hash,
                attr_hash: raw.attr_hash,
                body_sketch: raw.body_sketch.clone(),
                parent: raw.parent.map(|parent| id_of_raw[parent]),
                flags: raw.flags,
                impl_info: raw.impl_info.clone(),
            });
        }

        let mut edges = Vec::new();
        for (index, raw) in self.raws.iter().enumerate() {
            let Some(parent) = raw.parent else {
                continue;
            };
            edges.push(Edge {
                from: id_of_raw[parent],
                to: id_of_raw[index],
                kind: EdgeKind::Contains,
                certainty: Certainty::Resolved,
                site: raw.location.clone(),
                detail: EdgeDetail::None,
            });
        }

        let mut table = SymbolTable::default();
        for (key, index) in &self.defs {
            table.insert(*key, id_of_raw[*index]);
        }

        let containers: Vec<Container> = self
            .containers
            .iter()
            .map(|pending| Container {
                id: id_of_raw[pending.raw],
                def: pending.def,
                file: pending.file,
                node: pending.node.clone(),
            })
            .collect();

        let mut inactive = std::mem::take(&mut self.inactive);
        sort_locations(&mut inactive);

        let mut graph = RevisionGraph {
            crates: self
                .workspace
                .crates
                .iter()
                .map(|member| member.info.clone())
                .collect(),
            symbols,
            edges: Vec::new(),
            unresolved: Vec::new(),
            inactive,
            restricted: self.workspace.restricted,
        };

        let output = edges::collect(EdgeInput {
            db: self.db,
            sema: &self.sema,
            symbols: &table,
            locate: self.locator,
            graph: &graph,
            containers: &containers,
            inactive: &graph.inactive,
        });
        graph.edges = edges;
        graph.edges.extend(output.edges);
        edges::sort_edges(&mut graph.edges);
        graph.unresolved = output.unresolved;
        graph
    }
}

/// The sort key of a symbol: file, start offset, path text, kind, and the end
/// offset and crate as tie breakers, so that the order is total.
type RawKey<'a> = (&'a str, u32, &'a str, SymbolKind, u32, usize);

fn raw_key(raw: &Raw) -> RawKey<'_> {
    (
        raw.location.file.as_str(),
        raw.location.span.start,
        raw.path.as_str(),
        raw.kind,
        raw.location.span.end,
        raw.krate,
    )
}

/// Sorts locations by `(file, start, end)` and drops duplicates.
fn sort_locations(locations: &mut Vec<Location>) {
    locations.sort_by(|left, right| {
        let left_key = (
            left.file.as_str(),
            left.span.start,
            left.span.end,
            left.span.start_line,
        );
        let right_key = (
            right.file.as_str(),
            right.span.start,
            right.span.end,
            right.span.start_line,
        );
        left_key.cmp(&right_key)
    });
    locations.dedup_by(|left, right| left == right);
}

/// The range a definition reports, together with the syntax it may have.
fn with_range<A: AstNode>(source: InFile<A>) -> InFile<(TextRange, Option<A>)> {
    source.map(|node| (node.syntax().text_range(), Some(node)))
}

/// The hashes of a struct, union or variant field.
fn source_hashes(source: &FieldSource) -> Hashes {
    let (node, ty) = match source {
        FieldSource::Named(field) => (field.syntax(), field.ty()),
        FieldSource::Pos(field) => (field.syntax(), field.ty()),
    };
    symbols::field_hashes(node, ty)
}

/// The type of a field, whose references belong to the field.
fn field_type(source: &FieldSource) -> Option<SyntaxNode> {
    let ty = match source {
        FieldSource::Named(field) => field.ty(),
        FieldSource::Pos(field) => field.ty(),
    };
    ty.map(|ty| ty.syntax().clone())
}

/// The hashes of a macro definition, which is either the macro itself or, for a
/// procedural macro, the function that implements it.
fn macro_source_hashes<A: AstNode>(source: &Source<A>) -> Hashes {
    let Some(node) = source.syntax() else {
        return Hashes::none();
    };
    if let Some(item) = ast::Macro::cast(node.clone()) {
        return symbols::macro_hashes(&item);
    }
    if let Some(item) = ast::Fn::cast(node.clone()) {
        return symbols::function_hashes(&item);
    }
    Hashes::none()
}

/// Collects the items declared directly in `node`, without descending into the
/// items themselves: their own bodies are walked when they are added.
fn item_nodes(node: &SyntaxNode, out: &mut Vec<SyntaxNode>) {
    for child in node.children() {
        if ast::Item::can_cast(child.kind()) {
            out.push(child);
        } else {
            item_nodes(&child, out);
        }
    }
}

/// The crates of the database that can belong to a member target, by root file.
fn candidates_by_root(
    db: &dyn HirDatabase,
    members: &[MemberCrate],
) -> HashMap<FileId, Vec<Crate>> {
    let mut candidates: HashMap<FileId, Vec<Crate>> = HashMap::new();
    for krate in Crate::all(db) {
        let root = krate.root_file(db);
        if members.iter().any(|member| member.root_file == root) {
            candidates.entry(root).or_default().push(krate);
        }
    }
    for crates in candidates.values_mut() {
        crates.sort_by_key(|krate| display_name(*krate, db));
    }
    candidates
}

/// The database crate of a member target.
///
/// Several crates can share a root file (for example a binary that is declared
/// in the library root), so the crate name decides which one is meant.
fn find_crate(
    candidates: &HashMap<FileId, Vec<Crate>>,
    member: &MemberCrate,
    db: &dyn HirDatabase,
) -> Option<Crate> {
    let crates = candidates.get(&member.root_file)?;
    crates
        .iter()
        .copied()
        .find(|krate| display_name(*krate, db) == member.info.name)
        .or_else(|| crates.first().copied())
}

/// The name of a crate with dashes replaced by underscores, so that crate names
/// from the database and from Cargo compare equal.
fn display_name(krate: Crate, db: &dyn HirDatabase) -> String {
    krate
        .display_name(db)
        .map(|name| name.canonical_name().as_str().replace('-', "_"))
        .unwrap_or_default()
}
