//! Dependency edges between the extracted symbols.
//!
//! The `Contains` edges are produced by [`crate::extract`] itself, because they
//! follow directly from the symbol tree. Every other edge kind (calls, type
//! uses, field accesses, matches, ...) needs a pass over the code of every
//! function, constant and static, and lives here: [`collect`] is called once per
//! extraction, after the symbols and their ids are fixed, and its result is
//! merged into the graph.
//!
//! A dependency pass needs three things, all handed over in [`Input`]:
//!
//! - the attached database ([`Input::db`]) and the [`Semantics`] built on it to
//!   resolve names with `hir`,
//! - the [`SymbolTable`] to go from a rust-analyzer definition to the
//!   [`SymbolId`] this crate assigned to it,
//! - the [`Locator`] to turn a file id and a text range into a [`Location`]
//!   relative to the repository root. References inside macro expansions must be
//!   located with [`Locator::location`], which falls back to the range of the
//!   macro call in the real file.
//!
//! The symbols themselves are in [`Input::graph`], indexed by [`SymbolId`], so a
//! pass that needs the kind or the location of the symbol it is inside can look
//! it up there.
//!
//! Every reference is attributed to the innermost symbol whose region contains
//! it ([`Container`]): the region of a function or method is the whole item, the
//! region of a field is its type, and the regions of the types, traits and impl
//! blocks are their headers. A reference in a macro argument or in the body of a
//! macro-defined item is found by expanding the macro call and walking the
//! expansion; its location is always mapped back to the real file.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use blastr_model::{
    Certainty, Edge, EdgeDetail, EdgeKind, Location, RevisionGraph, SymbolId, SymbolKind,
    UnresolvedRef,
};
use ra_ap_hir::{
    Adt, AsAssocItem, AssocItem, AssocItemContainer, Crate, Field, Function, HirFileId, Impl,
    InFile, ModuleDef, PathResolution, Semantics, Trait, Variant, db::HirDatabase,
};
use ra_ap_ide_db::base_db::Crate as BaseCrate;
use ra_ap_syntax::ast::HasArgList;
use ra_ap_syntax::{AstNode, AstToken, SyntaxNode, ast};

use crate::hashing;
use crate::locate::Locator;
use crate::stats::{FileTiming, Kind, Profile};
use crate::symbols::{DefKey, SymbolTable};

/// The deepest nesting of macro expansions that is walked.
///
/// The limit only exists to guarantee termination: a recursive macro that
/// expands to itself would otherwise be walked forever.
const MAX_EXPANSION_DEPTH: u32 = 16;

/// A workspace-member crate, resolved in the database.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedCrate {
    /// Index into [`RevisionGraph::crates`].
    pub(crate) index: usize,
    /// The rust-analyzer handle of the crate.
    pub(crate) krate: Crate,
    /// Whether the crate is a binary target; only binaries have a `main`.
    pub(crate) is_bin: bool,
}

/// One region of source code whose references belong to one symbol.
///
/// The regions nest, and a reference always belongs to the innermost one that
/// contains it, which is why the walk skips a nested region: the nested symbol
/// is walked on its own and produces the edges with the right `from`.
#[derive(Debug, Clone)]
pub(crate) struct Container {
    /// The symbol the references inside the region belong to.
    pub(crate) id: SymbolId,
    /// The definition the region belongs to, when it is a definition.
    pub(crate) def: Option<DefKey>,
    /// The file of [`Container::node`]; a macro expansion for generated code.
    pub(crate) file: HirFileId,
    /// The syntax whose references belong to the symbol.
    pub(crate) node: SyntaxNode,
}

/// Everything [`collect`] may use.
#[derive(Clone, Copy)]
pub(crate) struct Input<'a, 'db> {
    /// The database, attached to the current thread for the duration of the call.
    pub(crate) db: &'db dyn HirDatabase,
    /// The name resolution of the same database.
    pub(crate) sema: &'a Semantics<'db, dyn HirDatabase>,
    /// The definition-to-symbol lookup.
    pub(crate) symbols: &'a SymbolTable,
    /// The file and range to location conversion.
    pub(crate) locate: &'a Locator<'db>,
    /// The extracted symbols, by [`SymbolId`](blastr_model::SymbolId).
    pub(crate) graph: &'a RevisionGraph,
    /// The crates of the workspace members, as the database names them.
    pub(crate) workspace_crates: &'a HashSet<BaseCrate>,
    /// The regions to walk, in no particular order.
    pub(crate) containers: &'a [Container],
    /// The code ranges that are inactive under the analysed configuration.
    pub(crate) inactive: &'a [Location],
}

/// What the dependency pass found.
#[derive(Debug, Default)]
pub(crate) struct Output {
    /// The edges, in any order.
    pub(crate) edges: Vec<Edge>,
    /// The references that did not resolve.
    pub(crate) unresolved: Vec<UnresolvedRef>,
}

/// Collects every edge kind except `Contains`.
pub(crate) fn collect(input: Input<'_, '_>, measure: bool) -> (Output, CollectorStats) {
    let mut collector = Collector {
        input,
        edges: Vec::new(),
        unresolved: Vec::new(),
        regions: input
            .containers
            .iter()
            .map(|container| container.node.clone())
            .collect(),
        impls: Vec::new(),
        quiet: false,
        expansion_site: None,
        profile: Profile::new(measure),
        file_times: HashMap::new(),
    };
    collector.run();
    let files = file_timings(&collector.file_times, collector.input.locate);
    (
        Output {
            edges: collector.edges,
            unresolved: collector.unresolved,
        },
        CollectorStats {
            profile: collector.profile,
            files,
        },
    )
}

/// What one dependency pass measured.
pub(crate) struct CollectorStats {
    /// The times of each kind of work.
    pub(crate) profile: Profile,
    /// The time each file took.
    pub(crate) files: Vec<FileTiming>,
}

/// The time one file took, accumulated over its regions.
#[derive(Debug, Default, Clone, Copy)]
struct FileCounters {
    /// The time the regions of the file took.
    duration: Duration,
    /// The number of regions the file holds.
    containers: u64,
    /// The number of edges and unresolved references the file produced.
    references: u64,
}

/// The per-file timings, in a deterministic order.
fn file_timings(times: &HashMap<HirFileId, FileCounters>, locate: &Locator<'_>) -> Vec<FileTiming> {
    let mut files: Vec<FileTiming> = times
        .iter()
        .map(|(&file, counters)| FileTiming {
            file: locate.file_path(file),
            containers: counters.containers,
            references: counters.references,
            seconds: counters.duration.as_secs_f64(),
        })
        .collect();
    files.sort_by(|left, right| left.file.cmp(&right.file));
    files
}

/// What one method call resolved to.
#[derive(Debug, Clone, Copy)]
enum MethodTarget {
    /// A method or an associated function.
    Function(Function),
    /// A field holding a function, which the call goes through.
    Field(Field),
}

/// The state of one dependency pass.
struct Collector<'a, 'db> {
    /// Everything the pass may read.
    input: Input<'a, 'db>,
    /// The edges found so far.
    edges: Vec<Edge>,
    /// The references that did not resolve.
    unresolved: Vec<UnresolvedRef>,
    /// The region nodes, so that a nested region is not walked twice.
    regions: HashSet<SyntaxNode>,
    /// The impl blocks of the workspace, for trait dispatch and `Implements`.
    impls: Vec<(SymbolId, Impl)>,
    /// True while walking code that a macro produced.
    ///
    /// rust-analyzer does not resolve the copies of an item that a `#[cfg]`
    /// disables, and a macro that generates an item usually generates such
    /// copies (`#[cfg(not(feature = "x"))] fn f() { .. }` next to
    /// `#[cfg(feature = "x")] fn f() { .. }`). A failure there says nothing
    /// about the reference, so nothing is reported from those regions.
    quiet: bool,
    /// The location of the outermost macro call whose expansion is being
    /// walked, if any.
    ///
    /// Everything a nested invocation of a workspace macro produces is located at
    /// that call, because the tokens of the inner call are part of the outer call
    /// in the real file.
    expansion_site: Option<Location>,
    /// Records the time spent in each kind of work.
    profile: Profile,
    /// The time each file took, by the file of its regions.
    file_times: HashMap<HirFileId, FileCounters>,
}

impl Collector<'_, '_> {
    /// Walks every region and adds the edges that do not come from a region.
    fn run(&mut self) {
        for container in self.input.containers {
            if let Some(DefKey::Impl(block)) = container.def {
                self.impls.push((container.id, block));
            }
        }
        for index in 0..self.impls.len() {
            let (id, block) = self.impls[index];
            self.implements(id, block);
        }
        for container in self.input.containers {
            let edges_before = self.edges.len();
            let unresolved_before = self.unresolved.len();
            let start = self.profile.start();
            // The semantics can only resolve nodes of trees it has parsed.
            self.timed(Kind::Parse, || {
                self.input.sema.parse_or_expand(container.file);
                Some(())
            });
            let previous = self.quiet;
            self.quiet = container.file.macro_file().is_some();
            self.visit(&container.node, container.id, 0, true);
            self.quiet = previous;
            if let Some(start) = start {
                let duration = start.elapsed();
                self.profile.add(Kind::Visit, duration, 1);
                let counters = self.file_times.entry(container.file).or_default();
                counters.duration += duration;
                counters.containers += 1;
                counters.references += ((self.edges.len() - edges_before)
                    + (self.unresolved.len() - unresolved_before))
                    as u64;
            }
        }
        self.sort_unresolved();
    }

    /// Times one rust-analyzer query.
    fn timed<T>(&self, kind: Kind, query: impl FnOnce() -> Option<T>) -> Option<T> {
        let start = self.profile.start();
        let result = self.guard(query);
        self.profile.finish(kind, start);
        result
    }

    /// Adds the `Implements` edges of one trait impl block.
    fn implements(&mut self, id: SymbolId, block: Impl) {
        let db = self.input.db;
        let Some(trait_) = self.timed(Kind::Implements, || block.trait_(db)) else {
            return;
        };
        let site = self.symbol_location(id);
        if let Some(trait_id) = self.input.symbols.get(DefKey::Trait(trait_)) {
            self.push(
                id,
                trait_id,
                EdgeKind::Implements,
                Certainty::Resolved,
                site,
                EdgeDetail::None,
            );
        }
        let Some(items) = self.timed(Kind::Implements, || Some(block.items(db))) else {
            return;
        };
        for item in items {
            let Some(item_id) = self.input.symbols.assoc_item(item) else {
                continue;
            };
            let Some(trait_item) = self.trait_item(trait_, item) else {
                continue;
            };
            let Some(trait_item_id) = self.input.symbols.assoc_item(trait_item) else {
                continue;
            };
            let site = self.symbol_location(item_id);
            self.push(
                item_id,
                trait_item_id,
                EdgeKind::Implements,
                Certainty::Resolved,
                site,
                EdgeDetail::None,
            );
        }
    }

    /// The item of `trait_` that `item` of an impl block implements.
    fn trait_item(&self, trait_: Trait, item: AssocItem) -> Option<AssocItem> {
        let db = self.input.db;
        let name = self.timed(Kind::Implements, || item.name(db))?;
        let items = self.timed(Kind::Implements, || Some(trait_.items(db)))?;
        items.into_iter().find(|candidate| {
            same_kind(*candidate, item)
                && self.timed(Kind::Implements, || candidate.name(db)) == Some(name.clone())
        })
    }

    /// Visits one node of a region.
    ///
    /// `root` is true for the node the region was made from, which is in
    /// [`Collector::regions`] like every nested region and must still be walked.
    fn visit(&mut self, node: &SyntaxNode, from: SymbolId, depth: u32, root: bool) {
        if !root && self.regions.contains(node) {
            return;
        }
        // Attributes are not part of the signature or the body of anything.
        if ast::Attr::can_cast(node.kind()) {
            return;
        }
        if let Some(call) = ast::MacroCall::cast(node.clone()) {
            self.macro_call(&call, from, depth);
            return;
        }
        if let Some(call) = ast::CallExpr::cast(node.clone()) {
            self.call_expr(&call, from, depth);
            return;
        }
        if let Some(call) = ast::MethodCallExpr::cast(node.clone()) {
            self.method_call(&call, from, depth);
            return;
        }
        if let Some(field) = ast::FieldExpr::cast(node.clone()) {
            self.field_expr(&field, from, depth);
            return;
        }
        if let Some(expr) = ast::RecordExpr::cast(node.clone()) {
            self.record_expr(&expr, from, depth);
            return;
        }
        if let Some(expr) = ast::MatchExpr::cast(node.clone()) {
            self.match_expr(&expr, from, depth);
            return;
        }
        if let Some(pat) = ast::RecordPat::cast(node.clone()) {
            self.record_pat(&pat, from, depth);
            return;
        }
        if let Some(pat) = ast::TupleStructPat::cast(node.clone()) {
            self.pattern_path(pat.path(), pat.syntax(), from, tuple_struct_has_rest(&pat));
            for field in pat.fields() {
                self.visit(field.syntax(), from, depth, false);
            }
            return;
        }
        if let Some(pat) = ast::PathPat::cast(node.clone()) {
            self.pattern_path(pat.path(), pat.syntax(), from, false);
            return;
        }
        if let Some(expr) = ast::FormatArgsExpr::cast(node.clone()) {
            self.format_args(&expr, from, depth);
            return;
        }
        if let Some(expr) = ast::PathExpr::cast(node.clone()) {
            self.path_expr(&expr, from, depth);
            return;
        }
        if let Some(ty) = ast::PathType::cast(node.clone()) {
            self.path_type(&ty, from, depth);
            return;
        }
        for child in node.children() {
            self.visit(&child, from, depth, false);
        }
    }

    /// Adds the `Expands` edge of a macro call and walks its expansion.
    ///
    /// A reference found in the expansion is located at the macro call in the
    /// real file by [`Collector::site`], and the calls another workspace macro's
    /// expansion makes are located at the outermost call.
    fn macro_call(&mut self, call: &ast::MacroCall, from: SymbolId, depth: u32) {
        self.expands(call, from);
        if depth >= MAX_EXPANSION_DEPTH {
            return;
        }
        let Some(expanded) = self.timed(Kind::ExpandMacro, || {
            self.input.sema.expand_macro_call(call)
        }) else {
            return;
        };
        if self.regions.contains(&expanded.value) {
            return;
        }
        let previous = self.quiet;
        self.quiet = previous || self.generates_disabled_code(call);
        // An invocation inside this expansion is still written in this call.
        let site = self
            .expansion_site
            .clone()
            .or_else(|| self.site(call.syntax()));
        let outer = std::mem::replace(&mut self.expansion_site, site);
        self.visit(&expanded.value, from, depth + 1, true);
        self.expansion_site = outer;
        self.quiet = previous;
    }

    /// Adds the `Expands` edge of an invocation of a workspace macro.
    ///
    /// Only macros defined by a workspace member are symbols of the graph; an
    /// invocation of a macro of the standard library or of a dependency
    /// resolves to no symbol and produces nothing.
    fn expands(&mut self, call: &ast::MacroCall, from: SymbolId) {
        let Some(macro_) = self.timed(Kind::ResolveMacro, || {
            self.input.sema.resolve_macro_call(call)
        }) else {
            return;
        };
        let Some(id) = self.input.symbols.get(DefKey::Macro(macro_)) else {
            return;
        };
        // An invocation that a macro produced is written where the outermost
        // call is; the inner call has no text of its own in the real file.
        let site = match self.expansion_site.clone() {
            Some(site) => site,
            None => {
                let Some(site) = self.site(call.syntax()) else {
                    return;
                };
                site
            }
        };
        self.push(
            from,
            id,
            EdgeKind::Expands,
            Certainty::Resolved,
            site,
            EdgeDetail::None,
        );
    }

    /// Whether the expansion of `call` holds code that the configuration
    /// disables.
    ///
    /// A macro that defines twice the same item for two `#[cfg]`s produces an
    /// expansion in which half of the items are disabled; rust-analyzer does not
    /// resolve those, so nothing they contain is reported. The disabled items map
    /// back to a part of the macro call, which is what is looked for here.
    fn generates_disabled_code(&self, call: &ast::MacroCall) -> bool {
        let Some(site) = self.site(call.syntax()) else {
            return false;
        };
        self.input.inactive.iter().any(|range| {
            range.file == site.file
                && site.span.start <= range.span.start
                && range.span.end <= site.span.end
        })
    }

    /// Visits a call expression.
    fn call_expr(&mut self, call: &ast::CallExpr, from: SymbolId, depth: u32) {
        if let Some(callee) = call.expr() {
            match &callee {
                ast::Expr::PathExpr(path_expr) => {
                    if let Some(path) = path_expr.path()
                        && path.parent_path().is_none()
                    {
                        self.path_call(&path, from);
                        self.type_arguments(&path, from, depth);
                    }
                }
                other => self.visit(other.syntax(), from, depth, false),
            }
        }
        if let Some(args) = call.arg_list() {
            for arg in args.args() {
                self.visit(arg.syntax(), from, depth, false);
            }
        }
    }

    /// Adds the edge of a path in callee position.
    fn path_call(&mut self, path: &ast::Path, from: SymbolId) {
        match self.timed(Kind::ResolvePath, || self.input.sema.resolve_path(path)) {
            Some(PathResolution::Def(ModuleDef::Function(function))) => {
                self.call_to(function, from, path.syntax());
            }
            // `Wrapper(1)` and `State::Busy(1)` are constructor calls.
            Some(PathResolution::Def(ModuleDef::Adt(adt))) => {
                self.constructs_adt(adt, from, path.syntax(), false);
            }
            Some(PathResolution::Def(ModuleDef::EnumVariant(variant))) => {
                self.constructs_variant_key(DefKey::Variant(variant), from, path.syntax(), false);
            }
            Some(_) => {}
            None => self.record_unresolved_path(path),
        }
    }

    /// Visits a method call.
    fn method_call(&mut self, call: &ast::MethodCallExpr, from: SymbolId, depth: u32) {
        if let Some(receiver) = call.receiver() {
            self.visit(receiver.syntax(), from, depth, false);
        }
        if let Some(name_ref) = call.name_ref() {
            let resolved = self.timed(Kind::ResolveMethod, || {
                self.input.sema.resolve_method_call_fallback(call)
            });
            match resolved {
                Some((target, _)) => {
                    let target = target.either(MethodTarget::Function, MethodTarget::Field);
                    match target {
                        MethodTarget::Function(function) => {
                            self.call_to(function, from, name_ref.syntax());
                        }
                        MethodTarget::Field(field) => {
                            if let Some(id) = self.input.symbols.get(DefKey::Field(field)) {
                                self.located_edge(
                                    from,
                                    id,
                                    EdgeKind::ReadsField,
                                    Certainty::Resolved,
                                    name_ref.syntax(),
                                    EdgeDetail::None,
                                );
                            }
                        }
                    }
                }
                None => self.record_unresolved_name(&name_ref),
            }
        }
        if let Some(args) = call.arg_list() {
            for arg in args.args() {
                self.visit(arg.syntax(), from, depth, false);
            }
        }
        for list in call
            .syntax()
            .children()
            .filter_map(ast::GenericArgList::cast)
        {
            self.visit(list.syntax(), from, depth, false);
        }
    }

    /// Adds the edge of a call to `function`.
    ///
    /// A call that resolves to a trait method declaration dispatches through the
    /// receiver, so every implementation of the trait in the workspace may be
    /// reached as well.
    fn call_to(&mut self, function: Function, from: SymbolId, node: &SyntaxNode) {
        let container = self.timed(Kind::ResolveDef, || {
            function
                .as_assoc_item(self.input.db)
                .map(|item| item.container(self.input.db))
        });
        if let Some(AssocItemContainer::Trait(trait_)) = container
            && let Some(id) = self.input.symbols.get(DefKey::Function(function))
        {
            self.located_edge(
                from,
                id,
                EdgeKind::Calls,
                Certainty::Resolved,
                node,
                EdgeDetail::None,
            );
            self.dispatch_candidates(trait_, function, from, node);
            return;
        }
        if let Some(id) = self.input.symbols.get(DefKey::Function(function)) {
            self.located_edge(
                from,
                id,
                EdgeKind::Calls,
                Certainty::Resolved,
                node,
                EdgeDetail::None,
            );
        }
    }

    /// Adds a candidate edge to every workspace implementation of `trait_` that
    /// has an item named like `method`.
    fn dispatch_candidates(
        &mut self,
        trait_: Trait,
        method: Function,
        from: SymbolId,
        node: &SyntaxNode,
    ) {
        let db = self.input.db;
        let Some(name) = self.timed(Kind::Dispatch, || Some(method.name(db))) else {
            return;
        };
        for index in 0..self.impls.len() {
            let block = self.impls[index].1;
            if self.timed(Kind::Dispatch, || block.trait_(db)) != Some(trait_) {
                continue;
            }
            let Some(items) = self.timed(Kind::Dispatch, || Some(block.items(db))) else {
                continue;
            };
            for item in items {
                let AssocItem::Function(candidate) = item else {
                    continue;
                };
                if self.timed(Kind::Dispatch, || Some(candidate.name(db))) != Some(name.clone()) {
                    continue;
                }
                if let Some(id) = self.input.symbols.assoc_item(item) {
                    self.located_edge(
                        from,
                        id,
                        EdgeKind::MayDispatchTo,
                        Certainty::Candidate,
                        node,
                        EdgeDetail::None,
                    );
                }
            }
        }
    }

    /// Visits a field access, which reads or writes the field.
    fn field_expr(&mut self, field: &ast::FieldExpr, from: SymbolId, depth: u32) {
        if let Some(base) = field.expr() {
            self.visit(base.syntax(), from, depth, false);
        }
        let Some(name_ref) = field.name_ref() else {
            return;
        };
        let kind = if is_write_target(field.syntax()) {
            EdgeKind::WritesField
        } else {
            EdgeKind::ReadsField
        };
        match self.timed(Kind::ResolveField, || self.input.sema.resolve_field(field)) {
            Some(target) => {
                // The second case is a field of a plain tuple, which has no symbol.
                let key = target.either(|item| Some(DefKey::Field(item)), |_tuple_field| None);
                if let Some(key) = key
                    && let Some(id) = self.input.symbols.get(key)
                {
                    self.located_edge(
                        from,
                        id,
                        kind,
                        Certainty::Resolved,
                        name_ref.syntax(),
                        EdgeDetail::None,
                    );
                }
            }
            None => {
                if name_ref.as_tuple_field().is_none() {
                    self.record_unresolved_name(&name_ref);
                }
            }
        }
    }

    /// Visits a struct or variant literal.
    fn record_expr(&mut self, expr: &ast::RecordExpr, from: SymbolId, depth: u32) {
        let path = expr.path();
        // A spread leaves the other fields to the value it copies them from.
        let spread = expr
            .record_expr_field_list()
            .is_some_and(|list| list.spread().is_some());
        match self.timed(Kind::ResolveRecord, || {
            self.input.sema.resolve_variant(expr.clone())
        }) {
            Some(variant) => self.constructs_variant(&variant, from, expr.syntax(), spread),
            None => {
                if let Some(path) = &path
                    && let Some(PathResolution::Def(ModuleDef::Adt(adt))) =
                        self.timed(Kind::ResolvePath, || self.input.sema.resolve_path(path))
                {
                    self.constructs_adt(adt, from, path.syntax(), spread);
                }
            }
        }
        if let Some(path) = &path {
            self.type_arguments(path, from, depth);
        }
        let Some(list) = expr.record_expr_field_list() else {
            return;
        };
        // A field named in a literal is neither read nor written: the literal
        // itself is what constructs the value.
        for field in list.fields() {
            if let Some(value) = field.expr() {
                self.visit(value.syntax(), from, depth, false);
            }
        }
        if let Some(spread) = list.spread() {
            self.visit(spread.syntax(), from, depth, false);
        }
    }

    /// Visits a `match` expression.
    fn match_expr(&mut self, expr: &ast::MatchExpr, from: SymbolId, depth: u32) {
        let detail = self.match_detail_of(expr);
        if let Some(scrutinee) = expr.expr() {
            let scrutinee_ty = self
                .timed(Kind::InferType, || self.input.sema.type_of_expr(&scrutinee))
                .map(|info| info.original.strip_references());
            if let Some(Adt::Enum(item)) = scrutinee_ty.and_then(|ty| ty.as_adt())
                && let Some(id) = self.input.symbols.get(DefKey::Adt(Adt::Enum(item)))
            {
                self.located_edge(
                    from,
                    id,
                    EdgeKind::Matches,
                    Certainty::Resolved,
                    scrutinee.syntax(),
                    detail,
                );
            }
            self.visit(scrutinee.syntax(), from, depth, false);
        }
        if let Some(arms) = expr.match_arm_list() {
            for arm in arms.arms() {
                self.visit(arm.syntax(), from, depth, false);
            }
        }
    }

    /// Visits a record pattern: it matches the struct or variant and its fields.
    fn record_pat(&mut self, pat: &ast::RecordPat, from: SymbolId, depth: u32) {
        self.pattern_path(pat.path(), pat.syntax(), from, record_pat_has_rest(pat));
        let Some(list) = pat.record_pat_field_list() else {
            return;
        };
        for field in list.fields() {
            if let Some((field_def, _)) = self.timed(Kind::ResolveRecord, || {
                self.input.sema.resolve_record_pat_field(&field)
            }) && let Some(id) = self.input.symbols.get(DefKey::Field(field_def))
            {
                let detail = self.match_detail(field.syntax());
                self.located_edge(
                    from,
                    id,
                    EdgeKind::Matches,
                    Certainty::Resolved,
                    field.syntax(),
                    detail,
                );
            }
            if let Some(inner) = field.pat() {
                self.visit(inner.syntax(), from, depth, false);
            }
        }
    }

    /// Adds the `Matches` edge of a pattern that names a struct, variant or
    /// constant.
    ///
    /// `rest` says that the pattern ends with `..`, which is recorded on the edge
    /// to a struct: the pattern then matches whatever fields the struct grows.
    fn pattern_path(
        &mut self,
        path: Option<ast::Path>,
        node: &SyntaxNode,
        from: SymbolId,
        rest: bool,
    ) {
        let Some(path) = path else {
            return;
        };
        match self.timed(Kind::ResolvePath, || self.input.sema.resolve_path(&path)) {
            Some(PathResolution::Def(def)) => {
                let leaves_fields_out = rest && matches!(def, ModuleDef::Adt(Adt::Struct(_)));
                let key = match def {
                    ModuleDef::Adt(adt) => Some(DefKey::Adt(adt)),
                    ModuleDef::EnumVariant(variant) => Some(DefKey::Variant(variant)),
                    ModuleDef::Const(item) => Some(DefKey::Const(item)),
                    _ => None,
                };
                if let Some(key) = key
                    && let Some(id) = self.input.symbols.get(key)
                {
                    let detail = if leaves_fields_out {
                        EdgeDetail::WithRest
                    } else {
                        self.match_detail(node)
                    };
                    self.located_edge(
                        from,
                        id,
                        EdgeKind::Matches,
                        Certainty::Resolved,
                        node,
                        detail,
                    );
                }
            }
            Some(_) => {}
            None => self.record_unresolved_path(&path),
        }
    }

    /// Visits a path in value position.
    fn path_expr(&mut self, expr: &ast::PathExpr, from: SymbolId, depth: u32) {
        let Some(path) = expr.path() else {
            return;
        };
        if path.parent_path().is_some() {
            return;
        }
        match self.timed(Kind::ResolvePath, || self.input.sema.resolve_path(&path)) {
            Some(PathResolution::Def(def)) => {
                if let Some((key, kind)) = value_target(def)
                    && let Some(id) = self.input.symbols.get(key)
                {
                    self.located_edge(
                        from,
                        id,
                        kind,
                        Certainty::Resolved,
                        path.syntax(),
                        EdgeDetail::None,
                    );
                }
            }
            Some(_) => {}
            None => self.record_unresolved_path(&path),
        }
        self.type_arguments(&path, from, depth);
    }

    /// Visits the template of a format string.
    ///
    /// A captured argument (`format!("{NAME}")`) is only a name inside the
    /// literal, so it has no syntax node of its own; the format machinery
    /// resolves the names of the literal, and each of them is a value reference
    /// like a path would be.
    fn format_args(&mut self, expr: &ast::FormatArgsExpr, from: SymbolId, depth: u32) {
        if let Some(string) = expr
            .syntax()
            .descendants_with_tokens()
            .filter_map(|child| child.into_token())
            .find_map(ast::String::cast)
        {
            let file = string.syntax().parent().and_then(|node| {
                self.timed(Kind::Site, || Some(self.input.sema.hir_file_for(&node)))
            });
            if let Some(file) = file
                && let Some(parts) = self.timed(Kind::FormatArgs, || {
                    self.input.sema.as_format_args_parts(&string)
                })
            {
                for (range, resolution) in parts {
                    let Some(PathResolution::Def(def)) =
                        resolution.and_then(|part| part.either(Some, |_asm_operand| None))
                    else {
                        continue;
                    };
                    let Some((key, kind)) = value_target(def) else {
                        continue;
                    };
                    let Some(id) = self.input.symbols.get(key) else {
                        continue;
                    };
                    let Some(site) =
                        self.timed(Kind::Site, || self.input.locate.location(file, range))
                    else {
                        continue;
                    };
                    self.push(from, id, kind, Certainty::Resolved, site, EdgeDetail::None);
                }
            }
        }
        // The explicit arguments are expressions of the expansion.
        for child in expr.syntax().children() {
            self.visit(&child, from, depth, false);
        }
    }

    /// Visits a path in type position.
    fn path_type(&mut self, ty: &ast::PathType, from: SymbolId, depth: u32) {
        let Some(path) = ty.path() else {
            return;
        };
        if path.parent_path().is_some() {
            return;
        }
        match self.timed(Kind::ResolvePath, || self.input.sema.resolve_path(&path)) {
            Some(PathResolution::Def(def)) => {
                let key = match def {
                    ModuleDef::Adt(adt) => Some(DefKey::Adt(adt)),
                    ModuleDef::Trait(item) => Some(DefKey::Trait(item)),
                    ModuleDef::TypeAlias(item) => Some(DefKey::TypeAlias(item)),
                    _ => None,
                };
                if let Some(key) = key
                    && let Some(id) = self.input.symbols.get(key)
                {
                    self.located_edge(
                        from,
                        id,
                        EdgeKind::UsesType,
                        Certainty::Resolved,
                        path.syntax(),
                        EdgeDetail::None,
                    );
                }
            }
            Some(_) => {}
            None => self.record_unresolved_path(&path),
        }
        self.type_arguments(&path, from, depth);
    }

    /// Visits the generic arguments of a path, which are type positions of their
    /// own (`Wrapper<Item>` uses `Item` as well).
    fn type_arguments(&mut self, path: &ast::Path, from: SymbolId, depth: u32) {
        let root = path.syntax();
        let lists: Vec<SyntaxNode> = root
            .descendants()
            .filter_map(ast::GenericArgList::cast)
            .map(|list| list.syntax().clone())
            .filter(|list| {
                !list
                    .ancestors()
                    .skip(1)
                    .take_while(|parent| parent != root)
                    .any(|parent| ast::GenericArgList::can_cast(parent.kind()))
            })
            .collect();
        for list in &lists {
            self.visit(list, from, depth, false);
        }
    }

    /// Adds the `Constructs` edge of a struct, union or enum variant literal.
    ///
    /// `spread` says that the literal ends with `..base`, which is recorded on the
    /// edge to a struct.
    fn constructs_variant(
        &mut self,
        variant: &Variant,
        from: SymbolId,
        node: &SyntaxNode,
        spread: bool,
    ) {
        let key = match variant {
            Variant::Struct(item) => DefKey::Adt(Adt::Struct(*item)),
            Variant::Union(item) => DefKey::Adt(Adt::Union(*item)),
            Variant::EnumVariant(item) => DefKey::Variant(*item),
        };
        self.constructs_variant_key(key, from, node, spread);
    }

    /// Adds a `Constructs` edge to the symbol of `key`, when it has one.
    fn constructs_variant_key(
        &mut self,
        key: DefKey,
        from: SymbolId,
        node: &SyntaxNode,
        spread: bool,
    ) {
        if let Some(id) = self.input.symbols.get(key) {
            let detail = self.constructs_detail(id, spread);
            self.located_edge(
                from,
                id,
                EdgeKind::Constructs,
                Certainty::Resolved,
                node,
                detail,
            );
        }
    }

    /// Adds a `Constructs` edge to a struct or union.
    fn constructs_adt(&mut self, adt: Adt, from: SymbolId, node: &SyntaxNode, spread: bool) {
        self.constructs_variant_key(DefKey::Adt(adt), from, node, spread);
    }

    /// The detail of a `Constructs` edge to `id`.
    ///
    /// Only a struct can be built from another value of its type, and only the
    /// literal that does it leaves fields out; a variant literal keeps the detail
    /// its kind always has.
    fn constructs_detail(&self, id: SymbolId, spread: bool) -> EdgeDetail {
        if !spread {
            return EdgeDetail::None;
        }
        let symbol = &self.input.graph.symbols[id.0 as usize];
        if symbol.path.kind == SymbolKind::Struct {
            EdgeDetail::WithRest
        } else {
            EdgeDetail::None
        }
    }

    /// The detail of a `Matches` edge written at `node`.
    fn match_detail(&self, node: &SyntaxNode) -> EdgeDetail {
        let Some(arm) = node.ancestors().find_map(ast::MatchArm::cast) else {
            return EdgeDetail::None;
        };
        let Some(arm_pat) = arm.pat() else {
            return EdgeDetail::None;
        };
        // A pattern inside the body of an arm is not part of the `match` it sits in.
        if !arm_pat
            .syntax()
            .text_range()
            .contains_range(node.text_range())
        {
            return EdgeDetail::None;
        }
        let Some(list) = arm.syntax().parent().and_then(ast::MatchArmList::cast) else {
            return EdgeDetail::None;
        };
        let Some(expr) = list.syntax().parent().and_then(ast::MatchExpr::cast) else {
            return EdgeDetail::None;
        };
        self.match_detail_of(&expr)
    }

    /// The detail of the `Matches` edges of one `match` expression.
    ///
    /// A `match` that the expansion of another crate's macro writes is not a
    /// choice the user of that macro made: a new variant that falls into its `_`
    /// arm is the macro's business, not a change of behaviour in the code that
    /// invokes it. Only a `match` the user wrote, directly or in the argument of
    /// a macro call, records whether it has a catch-all arm.
    fn match_detail_of(&self, expr: &ast::MatchExpr) -> EdgeDetail {
        if self.written_by_macro(expr.syntax()) {
            return EdgeDetail::None;
        }
        if self.has_catch_all(expr) {
            EdgeDetail::MatchWildcard
        } else {
            EdgeDetail::MatchExhaustive
        }
    }

    /// Whether a macro of another crate wrote the code at `node`.
    ///
    /// A node that an expansion copies from the arguments of the call is written
    /// by the caller, and so is every token the caller typed: the first token of
    /// the node maps back into the file, while a token the macro itself wrote
    /// maps back to the whole call.
    fn written_by_macro(&self, node: &SyntaxNode) -> bool {
        let written = self.guard(|| {
            let db = self.input.db;
            let file = self.input.sema.hir_file_for(node);
            let call = file.macro_file()?;
            let location = call.loc(db);
            if self.input.workspace_crates.contains(&location.def.krate) {
                return Some(false);
            }
            let anchor = location.kind.original_call_range(db, location.krate);
            let first = node.first_token()?;
            let mapped = InFile::new(file, first.text_range()).original_node_file_range_rooted(db);
            Some(mapped.file_id == anchor.file_id && mapped.range == anchor.range)
        });
        written.unwrap_or(false)
    }

    /// Whether one arm of `expr` catches every value.
    fn has_catch_all(&self, expr: &ast::MatchExpr) -> bool {
        expr.match_arm_list().is_some_and(|list| {
            list.arms()
                .filter_map(|arm| arm.pat())
                .any(|pat| self.is_catch_all(&pat))
        })
    }

    /// Whether a pattern is `_` or a binding that catches every value.
    fn is_catch_all(&self, pat: &ast::Pat) -> bool {
        match pat {
            ast::Pat::WildcardPat(_) => true,
            ast::Pat::IdentPat(ident) => match ident.pat() {
                // `x @ _` catches everything, `x @ Variant(_)` does not.
                Some(inner) => self.is_catch_all(&inner),
                // A bare ident that names a constant or a unit variant is not a
                // binding at all.
                None => self
                    .timed(Kind::ResolveBinding, || {
                        self.input.sema.resolve_bind_pat_to_const(ident)
                    })
                    .is_none(),
            },
            _ => false,
        }
    }

    /// Records a path that resolved to nothing.
    fn record_unresolved_path(&mut self, path: &ast::Path) {
        let text = hashing::text_without_ws(path.syntax());
        self.record_unresolved(path.syntax(), text);
    }

    /// Records a method call that resolved to nothing.
    fn record_unresolved_name(&mut self, name_ref: &ast::NameRef) {
        self.record_unresolved(name_ref.syntax(), name_ref.text().to_owned());
    }

    /// Records one unresolved reference.
    ///
    /// Code that is inactive under the analysed configuration is not resolved by
    /// rust-analyzer at all, so every reference in it would be reported here;
    /// it is already reported as a `Location` of its own instead. The same holds
    /// for the code a macro produced, which contains the disabled copies of
    /// every `#[cfg]`-duplicated item the macro defines.
    fn record_unresolved(&mut self, node: &SyntaxNode, text: String) {
        if self.quiet {
            return;
        }
        let Some(location) = self.site(node) else {
            return;
        };
        if self.is_inactive(&location) {
            return;
        }
        self.unresolved.push(UnresolvedRef { location, text });
    }

    /// Whether `location` lies in code that is inactive under the configuration.
    fn is_inactive(&self, location: &Location) -> bool {
        self.input.inactive.iter().any(|range| {
            range.file == location.file
                && range.span.start <= location.span.start
                && location.span.end <= range.span.end
        })
    }

    /// The location of `node`, mapped back to the real file.
    fn site(&self, node: &SyntaxNode) -> Option<Location> {
        self.timed(Kind::Site, || {
            let file = self.input.sema.hir_file_for(node);
            self.input.locate.location(file, node.text_range())
        })
    }

    /// The location of a symbol of the graph.
    fn symbol_location(&self, id: SymbolId) -> Location {
        // The id comes from the graph, so it indexes an existing symbol.
        self.input.graph.symbols[id.0 as usize].location.clone()
    }

    /// Adds an edge whose site is the location of `node`.
    fn located_edge(
        &mut self,
        from: SymbolId,
        to: SymbolId,
        kind: EdgeKind,
        certainty: Certainty,
        node: &SyntaxNode,
        detail: EdgeDetail,
    ) {
        let Some(site) = self.site(node) else {
            return;
        };
        self.push(from, to, kind, certainty, site, detail);
    }

    /// Adds one edge.
    fn push(
        &mut self,
        from: SymbolId,
        to: SymbolId,
        kind: EdgeKind,
        certainty: Certainty,
        site: Location,
        detail: EdgeDetail,
    ) {
        self.edges.push(Edge {
            from,
            to,
            kind,
            certainty,
            site,
            detail,
        });
    }

    /// Sorts the unresolved references by location and drops duplicates.
    fn sort_unresolved(&mut self) {
        self.unresolved.sort_by(|left, right| {
            let left_key = (
                left.location.file.as_str(),
                left.location.span.start,
                left.location.span.end,
                left.text.as_str(),
            );
            let right_key = (
                right.location.file.as_str(),
                right.location.span.start,
                right.location.span.end,
                right.text.as_str(),
            );
            left_key.cmp(&right_key)
        });
        self.unresolved
            .dedup_by(|left, right| left.location == right.location && left.text == right.text);
    }

    /// Runs a rust-analyzer query, treating a panic as "unknown".
    ///
    /// rust-analyzer can panic on syntax it cannot map, for example inside a
    /// broken macro expansion. One such reference must not take the whole
    /// extraction down, and every caller can do without the answer.
    fn guard<T>(&self, query: impl FnOnce() -> Option<T>) -> Option<T> {
        catch_unwind(AssertUnwindSafe(query)).ok().flatten()
    }
}

/// The edge a path in value position produces for the definition it resolves
/// to, if it is a value at all.
fn value_target(def: ModuleDef) -> Option<(DefKey, EdgeKind)> {
    Some(match def {
        ModuleDef::Function(item) => (DefKey::Function(item), EdgeKind::TakesAsValue),
        ModuleDef::Const(item) => (DefKey::Const(item), EdgeKind::UsesValue),
        ModuleDef::Static(item) => (DefKey::Static(item), EdgeKind::UsesValue),
        ModuleDef::EnumVariant(item) => (DefKey::Variant(item), EdgeKind::Constructs),
        // A unit struct built by naming it.
        ModuleDef::Adt(item) => (DefKey::Adt(item), EdgeKind::Constructs),
        _ => return None,
    })
}

/// Whether two items of a trait and of its implementation are of the same kind.
fn same_kind(left: AssocItem, right: AssocItem) -> bool {
    matches!(
        (left, right),
        (AssocItem::Function(_), AssocItem::Function(_))
            | (AssocItem::Const(_), AssocItem::Const(_))
            | (AssocItem::TypeAlias(_), AssocItem::TypeAlias(_))
    )
}

/// Whether a tuple-struct pattern ends with a rest pattern (`Pair(a, ..)`).
fn tuple_struct_has_rest(pat: &ast::TupleStructPat) -> bool {
    pat.fields()
        .any(|field| matches!(field, ast::Pat::RestPat(_)))
}

/// Whether a record pattern ends with `..` (`Point { x, .. }`).
fn record_pat_has_rest(pat: &ast::RecordPat) -> bool {
    pat.record_pat_field_list()
        .is_some_and(|list| list.rest_pat().is_some())
}

/// Whether `node` is the target of an assignment or of a mutable borrow.
fn is_write_target(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if let Some(assign) = ast::BinExpr::cast(parent.clone()) {
        return assign
            .op_kind()
            .is_some_and(|op| matches!(op, ast::BinaryOp::Assignment { .. }))
            && assign.lhs().is_some_and(|lhs| lhs.syntax() == node);
    }
    match ast::RefExpr::cast(parent) {
        Some(reference) => {
            reference.mut_token().is_some()
                && reference.expr().is_some_and(|inner| inner.syntax() == node)
        }
        None => false,
    }
}

/// Sorts edges by `(from, to, kind, site)` and drops exact duplicates.
pub(crate) fn sort_edges(edges: &mut Vec<Edge>) {
    edges.sort_by(|left, right| {
        let left_key = (
            left.from,
            left.to,
            left.kind.as_str(),
            left.site.file.as_str(),
            left.site.span.start,
            left.site.span.end,
        );
        let right_key = (
            right.from,
            right.to,
            right.kind.as_str(),
            right.site.file.as_str(),
            right.site.span.start,
            right.site.span.end,
        );
        left_key.cmp(&right_key)
    });
    edges.dedup_by(|left, right| {
        left.from == right.from
            && left.to == right.to
            && left.kind == right.kind
            && left.site == right.site
    });
}
