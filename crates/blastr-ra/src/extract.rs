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

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use blastr_model::{
    CancelToken, Certainty, CrateId, Edge, EdgeDetail, EdgeKind, ImplInfo, Location, Progress,
    RevisionGraph, Symbol, SymbolFlags, SymbolId, SymbolKind, SymbolPath, TargetKind,
    Visibility as ModelVisibility,
};
use ra_ap_hir::{
    Adt, AssocItem, Crate, EnumVariant, Field, FieldSource, Function, HasSource, HasVisibility,
    HirFileId, Impl, InFile, MacroKind, Module, ModuleDef, Semantics, Trait,
    Visibility as HirVisibility, attach_db, db::HirDatabase,
};
use ra_ap_ide_db::base_db::SourceDatabase;
use ra_ap_syntax::ast;
use ra_ap_syntax::{AstNode, SyntaxNode, TextRange, TextSize};
use ra_ap_vfs::FileId;

use crate::edges::{self, Container, Input as EdgeInput, ResolvedCrate};
use crate::hashing;
use crate::load::MemberCrate;
use ra_ap_ide_db::base_db::Crate as BaseCrate;

use crate::locate::Locator;
use crate::pipeline::{self, Crates, Threads};
use crate::stats::{ExtractReport, Kind, Profile};
use crate::symbols::{self, DefKey, Hashes, Raw, SymbolTable};
use crate::{Error, ExtractOptions, Workspace, check_cancel, paths};

impl Workspace {
    /// Extracts the graph of the project as currently loaded.
    pub fn extract(
        &self,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<RevisionGraph, Error> {
        let (graph, _report) = self.extract_with(&ExtractOptions::default(), progress, cancel)?;
        Ok(graph)
    }

    /// Extracts the graph, and reports how long the extraction took.
    ///
    /// The report is empty unless [`ExtractOptions::measure`] is set; the graph
    /// is the same either way.
    pub fn extract_with(
        &self,
        options: &ExtractOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<(RevisionGraph, ExtractReport), Error> {
        let database = self.host.raw_database();
        let db: &dyn HirDatabase = database;
        let source_db: &dyn SourceDatabase = database;
        let locator = Locator::new(source_db, &self.vfs, &self.root);
        let threads = Threads::new(database, options.threads);
        attach_db(db, || {
            let members = self.members(db);
            let phases = Crates::new(
                db,
                &members
                    .iter()
                    .enumerate()
                    .map(|(index, member)| (member.resolved.krate, index))
                    .collect::<Vec<_>>(),
                members.len(),
            );
            let mut walked: Vec<Option<Extractor<'_>>> = (0..members.len()).map(|_| None).collect();
            let started = Instant::now();
            let inactive = pipeline::run(
                pipeline::Input {
                    db,
                    crates: &phases,
                    threads: &threads,
                    paths: locator.paths(),
                    cancel,
                    progress,
                },
                &mut walked,
                |index| {
                    let extractor = Extractor::new(
                        db,
                        &locator,
                        cancel,
                        self,
                        &members[index],
                        options.measure,
                    );
                    let extractor = extractor.run()?;
                    let modules = extractor.modules.clone();
                    Ok((extractor, modules))
                },
            )?;
            let pass = started.elapsed();
            let mut walks = Vec::with_capacity(walked.len());
            for walk in walked {
                walks.push(
                    walk.ok_or_else(|| {
                        Error::Internal("a member crate was not walked".to_owned())
                    })?,
                );
            }
            let workspace_crates = members
                .iter()
                .map(|member| member.resolved.krate.base())
                .collect();
            let assembler =
                Assembler::merge(walks, db, &locator, self, workspace_crates, options.measure);
            Ok(assembler.finish(inactive, pass))
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
    /// Records the time spent in each kind of work.
    profile: Profile,
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
        measure: bool,
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
            profile: Profile::new(measure),
        }
    }

    /// Times one rust-analyzer query.
    fn timed<T>(&self, kind: Kind, query: impl FnOnce() -> Option<T>) -> Option<T> {
        let start = self.profile.start();
        let result = self.guarded(query);
        self.profile.finish(kind, start);
        result
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
        let start = self.profile.start();
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
        let walked = self.walk_module(root, crate_name, Some(index), Scope::default());
        self.profile.finish(Kind::CrateRoot, start);
        walked
    }

    /// Adds the items of one module, and of its impl blocks.
    fn walk_module(
        &mut self,
        module: Module,
        path: &str,
        parent: Option<usize>,
        scope: Scope,
    ) -> Result<(), Error> {
        let start = self.profile.start();
        check_cancel(self.cancel)?;
        self.note_module(module);
        let declarations = self
            .timed(Kind::Declarations, || Some(module.declarations(self.db)))
            .unwrap_or_default();
        for def in declarations {
            self.add_def(def, path, parent, scope)?;
        }
        // A `macro_rules!` macro without `#[macro_export]` is textually scoped
        // and not among the declarations. The legacy scope also holds the
        // macros inherited from enclosing modules and from `#[macro_use]`
        // crates, so only the ones defined in this module are taken.
        let macros = self
            .timed(Kind::Declarations, || Some(module.legacy_macros(self.db)))
            .unwrap_or_default();
        for item in macros {
            if item.module(self.db) == module {
                self.add_macro(item, path, parent, scope)?;
            }
        }
        let impls = self
            .timed(Kind::Declarations, || Some(module.impl_defs(self.db)))
            .unwrap_or_default();
        for block in impls {
            self.add_impl(block, path, parent)?;
        }
        self.profile.finish(Kind::Module, start);
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
        let start = self.profile.start();
        let result = self.add_def_inner(def, path, parent, scope);
        self.profile.finish(Kind::Symbol, start);
        result
    }

    /// Adds one definition that can appear in a module's declarations.
    fn add_def_inner(
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
            .timed(Kind::Source, || function.source_with_range(self.db))
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let name = function.name(self.db);
        if name.is_missing() {
            return Ok(());
        }
        let start = self.profile.start();
        let has_self = function.has_self_param(self.db);
        self.profile.finish(Kind::ItemQuery, start);
        let kind = if has_self {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        let hashes = source
            .ast()
            .map(|node| {
                let start = self.profile.start();
                let hashes = symbols::function_hashes(node);
                self.profile.finish(Kind::Hashes, start);
                hashes
            })
            .unwrap_or_else(Hashes::none);
        let is_test = hashes.is_test;
        let has_body = hashes.has_body;
        let path = paths::child(path, name.as_str());
        let start = self.profile.start();
        let visibility = function.visibility(self.db);
        let module = function.module(self.db);
        let visibility = self.model_visibility(visibility, module);
        let pub_api = self.is_pub_api(function.visibility(self.db), module);
        self.profile.finish(Kind::ItemQuery, start);
        let mut raw = Raw::new(
            kind,
            path.clone(),
            self.krate,
            source.location.clone(),
            visibility,
            hashes,
            parent,
        );
        raw.flags = SymbolFlags {
            is_test,
            is_main: self.is_main(name.as_str(), kind, parent),
            is_pub_api: pub_api,
            has_body,
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            from_macro: source.from_macro,
            sole_parameter_use: false,
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
            .timed(Kind::Source, || adt.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .syntax()
            .map(|node| {
                let start = self.profile.start();
                let hashes = symbols::adt_hashes(kind, node);
                self.profile.finish(Kind::Hashes, start);
                hashes
            })
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
                let fields = self
                    .timed(Kind::Declarations, || Some(it.fields(self.db)))
                    .unwrap_or_default();
                for field in fields {
                    self.add_field(field, module, &path, Some(index), scope)?;
                }
            }
            Adt::Union(it) => {
                let fields = self
                    .timed(Kind::Declarations, || Some(it.fields(self.db)))
                    .unwrap_or_default();
                for field in fields {
                    self.add_field(field, module, &path, Some(index), scope)?;
                }
            }
            Adt::Enum(it) => {
                let variants = self
                    .timed(Kind::Declarations, || Some(it.variants(self.db)))
                    .unwrap_or_default();
                for variant in variants {
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
            .timed(Kind::Source, || variant.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(|node| {
                let start = self.profile.start();
                let hashes = symbols::variant_hashes(node);
                self.profile.finish(Kind::Hashes, start);
                hashes
            })
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
        let fields = self
            .timed(Kind::Declarations, || Some(variant.fields(self.db)))
            .unwrap_or_default();
        for field in fields {
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
            .timed(Kind::Source, || field.source(self.db))
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
        let ty = source.ast().and_then(field_type);
        let sole_parameter_use = match (source.ast(), ty.as_ref()) {
            (Some(source), Some(ty)) => {
                let node = match source {
                    FieldSource::Named(field) => field.syntax(),
                    FieldSource::Pos(field) => field.syntax(),
                };
                symbols::sole_parameter_use(node, ty)
            }
            _ => false,
        };
        raw.flags = SymbolFlags {
            is_pub_api: self.is_pub_api(visibility, module),
            from_macro: source.from_macro,
            in_trait: scope.in_trait,
            in_trait_impl: scope.in_trait_impl,
            sole_parameter_use,
            ..SymbolFlags::default()
        };
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
            .timed(Kind::Source, || item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(|node| {
                let start = self.profile.start();
                let hashes = symbols::const_hashes(node);
                self.profile.finish(Kind::Hashes, start);
                hashes
            })
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
            .timed(Kind::Source, || item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(|node| {
                let start = self.profile.start();
                let hashes = symbols::static_hashes(node);
                self.profile.finish(Kind::Hashes, start);
                hashes
            })
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
            .timed(Kind::Source, || item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(|node| {
                let start = self.profile.start();
                let hashes = symbols::type_alias_hashes(node);
                self.profile.finish(Kind::Hashes, start);
                hashes
            })
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
            .timed(Kind::Source, || item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = source
            .ast()
            .map(|node| {
                let start = self.profile.start();
                let hashes = symbols::trait_hashes(node);
                self.profile.finish(Kind::Hashes, start);
                hashes
            })
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
        let items = self
            .timed(Kind::Declarations, || Some(item.items(self.db)))
            .unwrap_or_default();
        for item in items {
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
            .timed(Kind::Source, || item.source(self.db))
            .map(with_range)
            .and_then(|source| self.resolve_source(source))
        else {
            return Ok(());
        };
        let hashes = macro_source_hashes(&source, &self.profile);
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
        // A procedural macro is implemented by a function of the crate, which
        // is walked as a function of its own; the two are one item.
        if let Some(function) = self.implementing_function(&source) {
            self.aliases.push(MacroAlias {
                key,
                function: DefKey::Function(function),
                fallback: raw,
            });
            return Ok(());
        }
        self.push(raw, Some(key));
        Ok(())
    }

    /// The function that implements a procedural macro, if `source` is one.
    ///
    /// The source of a `macro_rules!` or of a `macro` 2.0 item is the item
    /// itself; the source of a procedural macro is the `fn` that the compiler
    /// calls, which is a definition of the same module.
    fn implementing_function<A: AstNode>(&self, source: &Source<A>) -> Option<Function> {
        let node = source.syntax()?;
        let function = ast::Fn::cast(node.clone())?;
        self.timed(Kind::ResolveDef, || {
            self.sema.to_def2(InFile::new(source.file, &function))
        })
    }

    /// Adds an impl block and the items it defines.
    fn add_impl(&mut self, block: Impl, path: &str, parent: Option<usize>) -> Result<(), Error> {
        check_cancel(self.cancel)?;
        let key = DefKey::Impl(block);
        if self.defs.contains_key(&key) {
            return Ok(());
        }
        let Some(source) = self
            .timed(Kind::Source, || block.source_with_range(self.db))
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
            .map(|node| {
                let start = self.profile.start();
                let hashes = symbols::impl_hashes(node);
                self.profile.finish(Kind::Hashes, start);
                hashes
            })
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
        let items = self
            .timed(Kind::Declarations, || Some(block.items(self.db)))
            .unwrap_or_default();
        for item in items {
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
        let start = self.profile.start();
        let mut nodes = Vec::new();
        item_nodes(body, &mut nodes);
        self.profile.finish(Kind::Nested, start);
        for node in nodes {
            check_cancel(self.cancel)?;
            let Some(item) = ast::Item::cast(node) else {
                continue;
            };
            let block = match &item {
                ast::Item::Impl(it) => self.timed(Kind::ResolveDef, || {
                    self.sema.to_def2(InFile::new(file, it))
                }),
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
        let start = self.profile.start();
        let def = match item {
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
        };
        self.profile.finish(Kind::ResolveDef, start);
        def
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

    /// Records the ranges of the code of `module` that is inactive under the
    /// analysed configuration.
    ///
    /// The ranges are read after the whole walk, because their query infers the
    /// types of every body of the module and is best run next to the other
    /// crates.
    fn note_module(&mut self, module: Module) {
        self.modules.push(module);
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
        let start = self.profile.start();
        let result = self.resolve_source_inner(source);
        self.profile.finish(Kind::Site, start);
        result
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
    db: &'a dyn HirDatabase,
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
    /// Records the time spent in each kind of work.
    profile: Profile,
}

impl<'a> Assembler<'a> {
    /// Joins the walks of the member crates, in the order of the crates.
    fn merge(
        walks: Vec<Extractor<'a>>,
        db: &'a dyn HirDatabase,
        locator: &'a Locator<'a>,
        workspace: &'a Workspace,
        workspace_crates: HashSet<BaseCrate>,
        measure: bool,
    ) -> Assembler<'a> {
        let mut assembler = Assembler {
            db,
            workspace_crates,
            locator,
            workspace,
            raws: Vec::new(),
            defs: HashMap::new(),
            containers: Vec::new(),
            profile: Profile::new(measure),
        };
        for walk in walks {
            let offset = assembler.raws.len();
            assembler.profile.merge(&walk.profile);
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
    fn finish(self, mut inactive: Vec<Location>, pass: Duration) -> (RevisionGraph, ExtractReport) {
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
        };

        let sema = Semantics::new_dyn(self.db);
        let edge_started = Instant::now();
        let (output, stats) = edges::collect(
            EdgeInput {
                db: self.db,
                sema: &sema,
                symbols: &table,
                locate: self.locator,
                graph: &graph,
                workspace_crates: &self.workspace_crates,
                containers: &containers,
                inactive: &graph.inactive,
            },
            self.profile.enabled(),
        );
        let edge_pass = edge_started.elapsed();
        let merge_started = Instant::now();
        graph.edges = edges;
        graph.edges.extend(output.edges);
        edges::sort_edges(&mut graph.edges);
        graph.unresolved = output.unresolved;
        let merge = merge_started.elapsed();
        self.profile.merge(&stats.profile);
        let report = ExtractReport {
            pipeline_pass: pass.as_secs_f64(),
            edge_pass: edge_pass.as_secs_f64(),
            merge: merge.as_secs_f64(),
            files: stats.files,
            phases: self.profile.phases(),
        };
        (graph, report)
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
fn macro_source_hashes<A: AstNode>(source: &Source<A>, profile: &Profile) -> Hashes {
    let Some(node) = source.syntax() else {
        return Hashes::none();
    };
    let start = profile.start();
    let hashes = if let Some(item) = ast::Macro::cast(node.clone()) {
        symbols::macro_hashes(&item)
    } else if let Some(item) = ast::Fn::cast(node.clone()) {
        symbols::function_hashes(&item)
    } else {
        Hashes::none()
    };
    profile.finish(Kind::Hashes, start);
    hashes
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
