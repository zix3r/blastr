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

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use blastr_model::{
    CancelToken, Certainty, CrateId, Edge, EdgeDetail, EdgeKind, ImplInfo, Location, Progress,
    RevisionGraph, Symbol, SymbolFlags, SymbolId, SymbolKind, SymbolPath, TargetKind,
    Visibility as ModelVisibility,
};
use ra_ap_hir::{
    Adt, AssocItem, Crate, EnumVariant, Field, FieldSource, Function, HasSource, HasVisibility,
    HirFileId, Impl, InFile, Macro, MacroKind, Module, ModuleDef, Name, Semantics, Trait,
    Visibility as HirVisibility, attach_db, db::HirDatabase,
};
use ra_ap_ide_db::RootDatabase;
use ra_ap_syntax::ast;
use ra_ap_syntax::{AstNode, SyntaxNode, TextRange, TextSize};
use ra_ap_vfs::FileId;

use crate::edges::{self, Container, Input as EdgeInput, ResolvedCrate};
use crate::hashing;
use crate::load::MemberCrate;
use ra_ap_ide_db::base_db::Crate as BaseCrate;

use crate::locate::{self, Locator, Root};
use crate::pipeline::Pipeline;
use crate::symbols::{self, DefKey, Hashes, Raw, SymbolTable};
use crate::{Error, ExtractOptions, Workspace, check_cancel, paths};

impl Workspace {
    /// Extracts the graph of the project as currently loaded.
    pub fn extract(
        &self,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<RevisionGraph, Error> {
        self.extract_with(&ExtractOptions::default(), progress, cancel)
    }

    /// Extracts the graph with the given options.
    pub fn extract_with(
        &self,
        options: &ExtractOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<RevisionGraph, Error> {
        let _span = tracing::info_span!("extract").entered();
        let database = self.host.raw_database();
        let db: &dyn HirDatabase = database;
        let paths = locate::paths(&self.vfs, &Root::new(&self.root));
        let locator = Locator::new(database, &paths);
        let pipeline = Pipeline {
            database,
            threads: options.threads,
            paths: &paths,
            cancel,
            progress,
        };
        attach_db(db, || {
            let members = self.members(db);
            let (walks, inactive, failed) = pipeline.run(members.len(), |index| {
                let extractor = Extractor::new(db, &locator, cancel, self, &members[index]);
                let extractor = extractor.run()?;
                let modules = extractor.modules.clone();
                Ok((extractor, modules))
            })?;
            let workspace_crates = members
                .iter()
                .map(|member| member.resolved.krate.base())
                .collect();
            let mut assembler = Assembler::merge(walks, database, &locator, self, workspace_crates);
            assembler.failed += failed;
            Ok(assembler.finish(inactive))
        })
    }

    /// The member crates of the project, with the crates of the database they
    /// were found as.
    fn members(&self, db: &dyn HirDatabase) -> Vec<Member> {
        let candidates = candidates_by_root(db, &self.crates);
        let mut members = Vec::with_capacity(self.crates.len());
        for (index, member) in self.crates.iter().enumerate() {
            let Some(krate) = find_crate(&candidates, member, db) else {
                continue;
            };
            members.push(Member {
                resolved: ResolvedCrate {
                    index,
                    krate,
                    is_bin: member.info.target_kind == TargetKind::Bin,
                },
                name: member.info.name.clone(),
            });
        }
        members
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

/// Where a definition is declared.
#[derive(Debug, Clone, Copy)]
struct Place<'p> {
    /// The path of the enclosing symbol, which the definition's path extends.
    path: &'p str,
    /// The enclosing symbol, as an index into [`Extractor::raws`].
    parent: Option<usize>,
    /// The block around the definition.
    scope: Scope,
}

impl Place<'_> {
    /// The place of the definitions inside the symbol at `index`, whose path
    /// is `path`.
    fn child(self, path: &str, index: usize) -> Place<'_> {
        Place {
            path,
            parent: Some(index),
            scope: self.scope,
        }
    }
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

/// A workspace-member crate of the graph, resolved in the database.
#[derive(Debug, Clone)]
struct Member {
    /// The crate as the graph reports it.
    resolved: ResolvedCrate,
    /// The crate name as it appears in paths.
    name: String,
}

/// The walk of one member crate.
///
/// A walk builds the symbols of one crate and the regions the dependency pass
/// walks, with indices that count from zero; [`Assembler::merge`] joins the
/// walks of all crates into one graph.
struct Extractor<'a> {
    /// The database, attached to the current thread.
    db: &'a dyn HirDatabase,
    /// Maps the syntax of nested items to their definitions.
    sema: Semantics<'a, dyn HirDatabase>,
    /// Maps file ids and ranges to locations.
    locator: &'a Locator<'a>,
    /// Asks the caller to stop.
    cancel: &'a CancelToken,
    /// The workspace, for its member crates and its execution mode.
    workspace: &'a Workspace,
    /// The crate of the graph that is walked.
    member: Member,
    /// Index into `Workspace::crates` of the crate that is walked.
    krate: usize,
    /// The symbols, before ids are assigned.
    raws: Vec<Raw>,
    /// Where each definition's symbol is in [`Extractor::raws`].
    defs: HashMap<DefKey, usize>,
    /// The modules whose inactive `cfg` ranges are needed.
    modules: Vec<Module>,
    /// The regions the dependency pass walks.
    containers: Vec<PendingContainer>,
    /// The procedural macros of the crate, waiting for the symbol of the
    /// function that implements them.
    aliases: Vec<MacroAlias>,
    /// The queries that panicked, whose answers are missing.
    failed: Cell<u32>,
}

/// A procedural macro, which has no item of its own.
///
/// Its definition is the function that implements it, which is written as one
/// item and walked as a function: the macro shares the symbol of that function,
/// so that a change to either reaches the same code and an invocation of the
/// macro names the one item the definition is written as.
struct MacroAlias {
    /// The key the macro itself is resolved by.
    key: DefKey,
    /// The key of the function that implements it.
    function: DefKey,
    /// The symbol to add when the function produced none.
    fallback: Raw,
}

impl<'a> Extractor<'a> {
    fn new(
        db: &'a dyn HirDatabase,
        locator: &'a Locator<'a>,
        cancel: &'a CancelToken,
        workspace: &'a Workspace,
        member: &Member,
    ) -> Extractor<'a> {
        Extractor {
            db,
            sema: Semantics::new_dyn(db),
            locator,
            cancel,
            workspace,
            member: Member {
                resolved: member.resolved,
                name: member.name.clone(),
            },
            krate: member.resolved.index,
            raws: Vec::new(),
            defs: HashMap::new(),
            modules: Vec::new(),
            containers: Vec::new(),
            aliases: Vec::new(),
            failed: Cell::new(0),
        }
    }

    /// Walks the crate of this worker.
    fn run(mut self) -> Result<Extractor<'a>, Error> {
        let name = self.member.name.clone();
        let krate = self.member.resolved.krate;
        self.walk_crate(krate, &name)?;
        self.resolve_aliases();
        Ok(self)
    }

    /// Points every procedural macro at the symbol of the function it is
    /// implemented by.
    ///
    /// That function is walked on its own, and the walk may reach it before or
    /// after the macro that it implements. A macro whose function produced no
    /// symbol keeps a symbol of its own, so that invocations of it stay in the
    /// graph.
    fn resolve_aliases(&mut self) {
        for alias in std::mem::take(&mut self.aliases) {
            match self.defs.get(&alias.function).copied() {
                Some(index) => {
                    self.defs.insert(alias.key, index);
                }
                None => {
                    self.push(alias.fallback, Some(alias.key));
                }
            }
        }
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
            Hashes::none(),
            None,
        );
        let index = self.push(raw, Some(DefKey::Module(root)));
        let place = Place {
            path: crate_name,
            parent: Some(index),
            scope: Scope::default(),
        };
        self.walk_module(root, place)
    }

    /// Adds the items of one module, and of its impl blocks.
    fn walk_module(&mut self, module: Module, place: Place<'_>) -> Result<(), Error> {
        check_cancel(self.cancel)?;
        // The inactive ranges of the module are read after the walk.
        self.modules.push(module);
        for def in self.all(|| module.declarations(self.db)) {
            self.add_def(def, place)?;
        }
        // A `macro_rules!` macro without `#[macro_export]` is textually scoped
        // and not among the declarations. The legacy scope also holds the
        // macros inherited from enclosing modules and from `#[macro_use]`
        // crates, so only the ones defined in this module are taken.
        for item in self.all(|| module.legacy_macros(self.db)) {
            if item.module(self.db) == module {
                self.add_macro(item, place);
            }
        }
        for block in self.all(|| module.impl_defs(self.db)) {
            self.add_impl(block, place)?;
        }
        Ok(())
    }

    /// Adds one definition that can appear in a module's declarations.
    fn add_def(&mut self, def: ModuleDef, place: Place<'_>) -> Result<(), Error> {
        match def {
            ModuleDef::Module(module) => self.add_module(module, place),
            ModuleDef::Function(function) => self.add_function(function, place),
            ModuleDef::Adt(adt) => self.add_adt(adt, place),
            ModuleDef::Trait(item) => self.add_trait(item, place),
            ModuleDef::Macro(item) => {
                self.add_macro(item, place);
                Ok(())
            }
            ModuleDef::Const(item) => {
                self.add_item(item, SymbolKind::Const, symbols::const_hashes, place);
                Ok(())
            }
            ModuleDef::Static(item) => {
                self.add_item(item, SymbolKind::Static, symbols::static_hashes, place);
                Ok(())
            }
            ModuleDef::TypeAlias(item) => {
                let hashes = symbols::type_alias_hashes;
                self.add_item(item, SymbolKind::TypeAlias, hashes, place);
                Ok(())
            }
            // Enum variants are reached through their enum, and builtin types
            // are not written anywhere.
            ModuleDef::EnumVariant(_) | ModuleDef::BuiltinType(_) => Ok(()),
        }
    }

    /// Adds a definition whose region is its whole source, and nothing below
    /// it, and returns its index and path.
    fn add_item<D>(
        &mut self,
        def: D,
        kind: SymbolKind,
        hashes: fn(&D::Ast) -> Hashes,
        place: Place<'_>,
    ) -> Option<(usize, String, Source<D::Ast>)>
    where
        D: HasSource + Into<ModuleDef> + Copy,
    {
        let (key, raw, source) = self.symbol(def, kind, hashes, place)?;
        let path = raw.path.clone();
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, source.syntax());
        Some((index, path, source))
    }

    /// Adds an item declared inside a trait or an impl block.
    fn add_assoc_item(&mut self, item: AssocItem, place: Place<'_>) -> Result<(), Error> {
        let def = match item {
            AssocItem::Function(it) => ModuleDef::Function(it),
            AssocItem::Const(it) => ModuleDef::Const(it),
            AssocItem::TypeAlias(it) => ModuleDef::TypeAlias(it),
        };
        self.add_def(def, place)
    }

    /// Builds the symbol of a definition, which the caller pushes.
    ///
    /// `None` when the definition was added already, has no name, or has no
    /// source in the workspace. `hashes` hashes the source of the kind.
    fn symbol<D>(
        &self,
        def: D,
        kind: SymbolKind,
        hashes: fn(&D::Ast) -> Hashes,
        place: Place<'_>,
    ) -> Option<(DefKey, Raw, Source<D::Ast>)>
    where
        D: HasSource + Into<ModuleDef> + Copy,
    {
        let db = self.db;
        let module_def: ModuleDef = def.into();
        let key = DefKey::from_module_def(module_def)?;
        if self.defs.contains_key(&key) {
            return None;
        }
        let name = module_def.name(db).filter(|name| !name.is_missing())?;
        let module = module_def.module(db)?;
        let visibility = module_def.visibility(db);
        let source = self.source(def)?;
        let hashes = source.ast().map(hashes).unwrap_or_else(Hashes::none);
        let function = matches!(kind, SymbolKind::Function | SymbolKind::Method);
        let flags = SymbolFlags {
            is_test: function && hashes.is_test,
            has_body: (function || matches!(kind, SymbolKind::Const | SymbolKind::Static))
                && hashes.has_body,
            is_pub_api: self.is_pub_api(visibility, module),
            in_trait: place.scope.in_trait,
            in_trait_impl: place.scope.in_trait_impl,
            from_macro: source.from_macro,
            ..SymbolFlags::default()
        };
        let mut raw = Raw::new(
            kind,
            paths::child(place.path, name.as_str()),
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            place.parent,
        );
        raw.flags = flags;
        Some((key, raw, source))
    }

    /// The source of a definition, with its location.
    fn source<D: HasSource>(&self, def: D) -> Option<Source<D::Ast>> {
        let source = self.guarded(|| def.source_with_range(self.db))?;
        self.resolve_source(source)
    }

    fn add_module(&mut self, module: Module, place: Place<'_>) -> Result<(), Error> {
        let key = DefKey::Module(module);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let Some(name) = module.name(self.db).filter(|name| !name.is_missing()) else {
            return Ok(());
        };
        let Some(location) = self.module_location(module) else {
            return Ok(());
        };
        let path = paths::child(place.path, name.as_str());
        let raw = Raw::new(
            SymbolKind::Module,
            path.clone(),
            self.krate,
            location,
            self.model_visibility(module.visibility(self.db), module),
            Hashes::none(),
            place.parent,
        );
        let index = self.push(raw, Some(key));
        self.walk_module(module, place.child(&path, index))
    }

    fn add_function(&mut self, function: Function, place: Place<'_>) -> Result<(), Error> {
        let kind = if function.has_self_param(self.db) {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        let Some((index, path, source)) =
            self.add_item(function, kind, symbols::function_hashes, place)
        else {
            return Ok(());
        };
        let name = function.name(self.db);
        self.raws[index].flags.is_main = self.is_main(name.as_str(), kind, place.parent);
        if let Some(body) = source.ast().and_then(|item| item.body()) {
            self.walk_nested(source.file, body.syntax(), place.child(&path, index))?;
        }
        Ok(())
    }

    fn add_adt(&mut self, adt: Adt, place: Place<'_>) -> Result<(), Error> {
        let kind = match adt {
            Adt::Struct(_) => SymbolKind::Struct,
            Adt::Enum(_) => SymbolKind::Enum,
            Adt::Union(_) => SymbolKind::Union,
        };
        let Some((index, path, _)) = self.add_item(adt, kind, symbols::adt_hashes, place) else {
            return Ok(());
        };
        let inner = place.child(&path, index);
        let module = adt.module(self.db);
        match adt {
            Adt::Struct(it) => self.add_fields(self.all(|| it.fields(self.db)), module, inner),
            Adt::Union(it) => self.add_fields(self.all(|| it.fields(self.db)), module, inner),
            Adt::Enum(it) => {
                for variant in self.all(|| it.variants(self.db)) {
                    self.add_variant(variant, inner)?;
                }
                Ok(())
            }
        }
    }

    fn add_variant(&mut self, variant: EnumVariant, place: Place<'_>) -> Result<(), Error> {
        let hashes = symbols::variant_hashes;
        let Some((key, raw, _)) = self.symbol(variant, SymbolKind::Variant, hashes, place) else {
            return Ok(());
        };
        let path = raw.path.clone();
        let index = self.push(raw, Some(key));
        let module = variant.module(self.db);
        self.add_fields(
            self.all(|| variant.fields(self.db)),
            module,
            place.child(&path, index),
        )
    }

    /// Adds the fields of a struct, a union or a variant declared in `module`.
    fn add_fields(
        &mut self,
        fields: Vec<Field>,
        module: Module,
        place: Place<'_>,
    ) -> Result<(), Error> {
        for field in fields {
            self.add_field(field, module, place);
        }
        Ok(())
    }

    fn add_field(&mut self, field: Field, module: Module, place: Place<'_>) {
        let key = DefKey::Field(field);
        if self.defs.contains_key(&key) {
            return;
        }
        let Some(source) = self.source(field) else {
            return;
        };
        let name = field.name(self.db);
        let name = if name.is_missing() {
            field.index().to_string()
        } else {
            name.as_str().to_owned()
        };
        let (node, ty) = match source.ast() {
            Some(FieldSource::Named(field)) => (Some(field.syntax()), field.ty()),
            Some(FieldSource::Pos(field)) => (Some(field.syntax()), field.ty()),
            None => (None, None),
        };
        let hashes = node.map_or_else(Hashes::none, |node| symbols::field_hashes(node, ty.clone()));
        let ty = ty.map(|ty| ty.syntax().clone());
        let visibility = field.visibility(self.db);
        let mut raw = Raw::new(
            SymbolKind::Field,
            paths::child(place.path, &name),
            self.krate,
            source.location.clone(),
            self.model_visibility(visibility, module),
            hashes,
            place.parent,
        );
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            from_macro: source.from_macro,
            in_trait: place.scope.in_trait,
            in_trait_impl: place.scope.in_trait_impl,
            sole_parameter_use: node
                .zip(ty.as_ref())
                .is_some_and(|(node, ty)| symbols::sole_parameter_use(node, ty)),
            ..SymbolFlags::default()
        };
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, ty.as_ref());
    }

    fn add_trait(&mut self, item: Trait, place: Place<'_>) -> Result<(), Error> {
        let hashes = symbols::trait_hashes;
        let Some((index, path, _)) = self.add_item(item, SymbolKind::Trait, hashes, place) else {
            return Ok(());
        };
        let mut inner = place.child(&path, index);
        inner.scope.in_trait = true;
        for item in self.all(|| item.items(self.db)) {
            self.add_assoc_item(item, inner)?;
        }
        Ok(())
    }

    fn add_macro(&mut self, item: Macro, place: Place<'_>) {
        let hashes = symbols::macro_source_hashes;
        let Some((key, raw, source)) = self.symbol(item, SymbolKind::Macro, hashes, place) else {
            return;
        };
        // A procedural macro is implemented by a function of the crate, which
        // is walked as a function of its own; the two are one item.
        if let Some(function) = source.syntax().cloned().and_then(ast::Fn::cast)
            && let Some(function) =
                self.guarded(|| self.sema.to_def2(InFile::new(source.file, &function)))
        {
            self.aliases.push(MacroAlias {
                key,
                function: DefKey::Function(function),
                fallback: raw,
            });
            return;
        }
        self.push(raw, Some(key));
    }

    /// Adds an impl block and the items it defines.
    fn add_impl(&mut self, block: Impl, place: Place<'_>) -> Result<(), Error> {
        check_cancel(self.cancel)?;
        let key = DefKey::Impl(block);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let Some(source) = self.source(block) else {
            return Ok(());
        };
        let trait_def = block.trait_(self.db);
        let self_ty = block.self_ty(self.db).as_adt();
        let (trait_text, self_text) = match source.ast() {
            Some(node) => (
                node.trait_()
                    .map(|it| hashing::text_without_ws(it.syntax())),
                node.self_ty()
                    .map(|it| hashing::text_without_ws(it.syntax())),
            ),
            // A derive-generated impl has no text to copy its self type from.
            None => (
                trait_def.map(|it| it.name(self.db).as_str().to_owned()),
                self_ty.map(|adt| adt.name(self.db).as_str().to_owned()),
            ),
        };
        let Some(self_text) = self_text else {
            return Ok(());
        };
        let segment = paths::impl_segment(trait_text.as_deref(), &self_text);
        let path = paths::child(place.path, &segment);
        let info = ImplInfo {
            trait_path: trait_def
                .map(|it| self.definition_path(it.module(self.db), it.name(self.db))),
            self_path: self_ty
                .map(|adt| self.definition_path(adt.module(self.db), adt.name(self.db))),
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
            place.parent,
        );
        raw.flags = SymbolFlags {
            in_trait_impl: trait_def.is_some(),
            from_macro: source.from_macro,
            ..SymbolFlags::default()
        };
        raw.impl_info = Some(info.clone());
        let index = self.push(raw, Some(key));
        self.region(index, Some(key), source.file, source.syntax());
        let item_path = match (trait_def, info.self_path) {
            (None, Some(self_path)) => self_path,
            _ => path,
        };
        let mut inner = place.child(&item_path, index);
        inner.scope = Scope {
            in_trait: false,
            in_trait_impl: trait_def.is_some(),
        };
        for item in self.all(|| block.items(self.db)) {
            self.add_assoc_item(item, inner)?;
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
        place: Place<'_>,
    ) -> Result<(), Error> {
        let mut nodes = Vec::new();
        item_nodes(body, &mut nodes);
        for node in nodes {
            check_cancel(self.cancel)?;
            let Some(item) = ast::Item::cast(node) else {
                continue;
            };
            if let ast::Item::Impl(it) = &item {
                if let Some(block) = self.guarded(|| self.sema.to_def2(InFile::new(file, it))) {
                    self.add_impl(block, place)?;
                }
                continue;
            }
            if let Some(def) = self.nested_def(file, &item) {
                self.add_def(def, place)?;
            }
        }
        Ok(())
    }

    /// Resolves the syntax of an item in a block to its definition.
    fn nested_def(&self, file: HirFileId, item: &ast::Item) -> Option<ModuleDef> {
        let sema = &self.sema;
        let syntax = item.syntax().clone();
        self.guarded(|| match item {
            ast::Item::Fn(it) => sema.to_def2(InFile::new(file, it)).map(ModuleDef::from),
            ast::Item::Trait(it) => sema.to_def2(InFile::new(file, it)).map(ModuleDef::from),
            ast::Item::Const(it) => sema.to_def2(InFile::new(file, it)).map(ModuleDef::from),
            ast::Item::Static(it) => sema.to_def2(InFile::new(file, it)).map(ModuleDef::from),
            ast::Item::TypeAlias(it) => sema.to_def2(InFile::new(file, it)).map(ModuleDef::from),
            ast::Item::Module(it) => sema.to_def2(InFile::new(file, it)).map(ModuleDef::from),
            ast::Item::Struct(_) | ast::Item::Enum(_) | ast::Item::Union(_) => {
                let adt = ast::Adt::cast(syntax)?;
                sema.to_def2(InFile::new(file, &adt)).map(ModuleDef::from)
            }
            ast::Item::MacroRules(_) | ast::Item::MacroDef(_) => {
                let item = ast::Macro::cast(syntax)?;
                sema.to_def2(InFile::new(file, &item)).map(ModuleDef::from)
            }
            _ => None,
        })
    }

    /// The canonical path of a definition, built from its crate and module
    /// instead of from the walk, so that it does not depend on the walk order.
    fn definition_path(&self, module: Module, name: Name) -> String {
        let mut path = self.crate_name(module.krate(self.db));
        for segment in module.path_segments(self.db) {
            path = paths::child(&path, segment.as_str());
        }
        paths::child(&path, name.as_str())
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
        (self.member.resolved.index == index).then_some(&self.member.resolved)
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

    /// Maps a range that may live in a macro expansion back to the real file.
    fn locate(&self, file: HirFileId, range: TextRange) -> Option<Location> {
        self.guarded(|| self.locator.location(file, range))
    }

    /// Runs a rust-analyzer query, treating a panic as "unknown".
    ///
    /// rust-analyzer uses unwinding to cancel queries and can panic on syntax it
    /// cannot map, for example inside a broken macro expansion. One such item
    /// must not take the whole extraction down, and every caller can do without
    /// the answer, so a panic becomes `None`. It is counted, so that the graph
    /// can say that something is missing.
    fn guarded<T>(&self, query: impl FnOnce() -> Option<T>) -> Option<T> {
        catch_unwind(AssertUnwindSafe(query)).unwrap_or_else(|_| {
            self.failed.set(self.failed.get() + 1);
            None
        })
    }

    /// The definitions a listing query returns, or none when it panics.
    fn all<T>(&self, query: impl FnOnce() -> Vec<T>) -> Vec<T> {
        self.guarded(|| Some(query())).unwrap_or_default()
    }

    /// Turns the syntax of a definition into its source, with a location.
    fn resolve_source<A: AstNode>(
        &self,
        source: InFile<(TextRange, Option<A>)>,
    ) -> Option<Source<A>> {
        self.resolve_source_inner(source)
    }

    /// Turns the syntax of a definition into its source, with a location.
    fn resolve_source_inner<A: AstNode>(
        &self,
        source: InFile<(TextRange, Option<A>)>,
    ) -> Option<Source<A>> {
        let file = source.file_id;
        let (range, node) = source.value;
        let location = self.locate(file, range)?;
        let from_macro = node.is_none() || self.written_by_macro(file, &location);
        Some(Source {
            file,
            node: node.map(|node| InFile::new(file, node)),
            location,
            from_macro,
        })
    }

    /// Whether the item is only written into the file by the macro, with no
    /// text of its own.
    ///
    /// A derive and a function-like macro write everything they produce: every
    /// item of their expansion is located at the call and has no text of its
    /// own.
    ///
    /// An attribute macro is written *on* an item: its call node is that whole
    /// item, attributes and doc comments included. The item itself, which the
    /// macro rewrites, keeps the range of the whole item — it is written out,
    /// and only its body was replaced — while the items the macro generates are
    /// located at the attribute alone.
    fn written_by_macro(&self, file: HirFileId, location: &Location) -> bool {
        let Some(call) = file.macro_file() else {
            return false;
        };
        if !matches!(call.kind(self.db), MacroKind::Attr | MacroKind::AttrBuiltIn) {
            return true;
        }
        let Some(node) = self.guarded(|| Some(call.call_node(self.db))) else {
            return true;
        };
        let Some(site) = self.locate(node.file_id, node.value.text_range()) else {
            return true;
        };
        !(site.file == location.file && site.span == location.span)
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
}

/// The walks of all member crates, joined into one graph.
struct Assembler<'a> {
    /// The database, attached to the current thread.
    db: &'a RootDatabase,
    /// The crates of the workspace members, as the database names them.
    workspace_crates: HashSet<BaseCrate>,
    /// Maps file ids and ranges to locations.
    locator: &'a Locator<'a>,
    /// The workspace, for its member crates and its execution mode.
    workspace: &'a Workspace,
    /// The symbols of every crate, before ids are assigned.
    raws: Vec<Raw>,
    /// Where each definition's symbol is in [`Assembler::raws`].
    defs: HashMap<DefKey, usize>,
    /// The regions the dependency pass walks.
    containers: Vec<PendingContainer>,
    /// The queries of the walks that panicked.
    failed: u32,
}

impl<'a> Assembler<'a> {
    /// Joins the walks of the member crates, in the order of the crates.
    fn merge(
        walks: Vec<Extractor<'a>>,
        db: &'a RootDatabase,
        locator: &'a Locator<'a>,
        workspace: &'a Workspace,
        workspace_crates: HashSet<BaseCrate>,
    ) -> Assembler<'a> {
        let mut assembler = Assembler {
            db,
            workspace_crates,
            locator,
            workspace,
            raws: Vec::new(),
            defs: HashMap::new(),
            containers: Vec::new(),
            failed: 0,
        };
        for walk in walks {
            assembler.failed += walk.failed.get();
            let offset = assembler.raws.len();
            for mut raw in walk.raws {
                if let Some(parent) = raw.parent {
                    raw.parent = Some(parent + offset);
                }
                assembler.raws.push(raw);
            }
            for (key, index) in walk.defs {
                assembler.defs.insert(key, index + offset);
            }
            for mut pending in walk.containers {
                pending.raw += offset;
                assembler.containers.push(pending);
            }
        }
        assembler
    }

    /// Assigns the ids, builds the symbols and the `Contains` edges, and hands
    /// the result to the dependency-edge pass.
    fn finish(self, mut inactive: Vec<Location>) -> RevisionGraph {
        sort_locations(&mut inactive);
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
            failed_queries: 0,
        };

        let sema = Semantics::new(self.db);
        let _span = tracing::info_span!("edges").entered();
        let output = edges::collect(EdgeInput {
            db: self.db,
            sema: &sema,
            symbols: &table,
            locate: self.locator,
            graph: &graph,
            workspace_crates: &self.workspace_crates,
            containers: &containers,
            inactive: &graph.inactive,
        });
        graph.edges = edges;
        graph.edges.extend(output.edges);
        edges::sort_edges(&mut graph.edges);
        graph.unresolved = output.unresolved;
        graph.failed_queries = self.failed + output.failed;
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
