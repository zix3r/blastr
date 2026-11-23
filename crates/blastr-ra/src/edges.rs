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

use blastr_model::{
    Certainty, Edge, EdgeDetail, EdgeKind, Location, RevisionGraph, SymbolId, SymbolKind,
    UnresolvedRef,
};
use ra_ap_hir::{
    Adt, AsAssocItem, AssocItem, AssocItemContainer, Crate, Field, Function, HirFileId, Impl,
    InFile, Macro, ModuleDef, PathResolution, Semantics, Trait, Variant, db::HirDatabase,
};
use ra_ap_ide_db::base_db::{Crate as BaseCrate, CrateOrigin};
use ra_ap_syntax::ast::{HasArgList, HasAttrs, HasLoopBody};
use ra_ap_syntax::{AstNode, AstToken, SyntaxNode, ast};

use crate::hashing;
use crate::locate::Locator;
use crate::symbols::{DefKey, SymbolTable};

/// The standard operator traits whose methods an operator expression invokes.
///
/// Nothing in the syntax names the method, so only a resolution that lands on
/// an impl of one of these traits produces an edge. The list is also the
/// pruning set: an operator whose traits have no implementation in the
/// workspace cannot reach one, and asking rust-analyzer would force the type
/// inference of the whole body for nothing.
const OPERATOR_TRAITS: &[&str] = &[
    "Add",
    "Sub",
    "Mul",
    "Div",
    "Rem",
    "Shl",
    "Shr",
    "BitXor",
    "BitOr",
    "BitAnd",
    "AddAssign",
    "SubAssign",
    "MulAssign",
    "DivAssign",
    "RemAssign",
    "ShlAssign",
    "ShrAssign",
    "BitXorAssign",
    "BitOrAssign",
    "BitAndAssign",
    "Neg",
    "Not",
    "Deref",
    "DerefMut",
    "Index",
    "IndexMut",
    "PartialEq",
    "PartialOrd",
];

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
pub(crate) fn collect(input: Input<'_, '_>) -> Output {
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
        formatting: Vec::new(),
        conversions: Vec::new(),
        iterators: Vec::new(),
        collects: Vec::new(),
        operators: HashSet::new(),
        invocations: HashMap::new(),
        quiet: false,
        expansion_site: None,
    };
    collector.run();
    Output {
        edges: collector.edges,
        unresolved: collector.unresolved,
    }
}

/// What one method call resolved to.
#[derive(Debug, Clone, Copy)]
enum MethodTarget {
    /// A method or an associated function.
    Function(Function),
    /// A field holding a function, which the call goes through.
    Field(Field),
}

/// Which impls the value of one expression can reach.
#[derive(Debug, Clone, Copy)]
enum ValueType {
    /// The value is of a data type of the workspace.
    Data(Adt),
    /// The type is known and is not a data type: a number, a string slice, a
    /// tuple. No impl in the workspace can apply to it.
    Other,
    /// The type could not be derived, or it is a type parameter or a trait
    /// object that any impl may apply to.
    Unknown,
}

impl ValueType {
    /// Whether a value of this type can reach one implementation of a trait.
    ///
    /// `ty` is the impl's self type, and `None` when the impl names a type this
    /// pass cannot read: any value may then reach it.
    fn can_reach(self, ty: Option<Adt>) -> bool {
        match (self, ty) {
            (ValueType::Data(value), Some(other)) => value == other,
            (ValueType::Data(_) | ValueType::Other, None) => true,
            (ValueType::Other, Some(_)) => false,
            (ValueType::Unknown, _) => true,
        }
    }
}

/// One workspace implementation of a formatting trait.
#[derive(Debug, Clone, Copy)]
struct FormattingImpl {
    /// Which trait the impl provides.
    trait_: FormattingTrait,
    /// The data type it is implemented for, when the impl names one.
    ///
    /// `None` for a self type this pass cannot name, such as a reference or a
    /// type parameter: any formatted value may reach it.
    self_ty: Option<Adt>,
    /// The symbol of the impl's `fmt`.
    symbol: SymbolId,
}

/// The standard formatting traits whose methods are invoked implicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormattingTrait {
    /// `Display`, which `{}` and `to_string` invoke.
    Display,
    /// `Debug`, which `{:?}` invokes.
    Debug,
}

/// One workspace implementation of the standard `From`, which `?` invokes.
#[derive(Debug, Clone, Copy)]
struct ConversionImpl {
    /// The error type it converts from, when that type is a data type.
    ///
    /// `None` for a source type this pass cannot name, such as a string or an
    /// error of the standard library: any `?` may reach it.
    source: Option<Adt>,
    /// The symbol of the impl's `from`.
    symbol: SymbolId,
}

/// One workspace implementation of `FromIterator`, which `collect` invokes.
#[derive(Debug, Clone, Copy)]
struct CollectImpl {
    /// The collection type, when the impl names one.
    ///
    /// `None` for a self type this pass cannot name, such as a reference or a
    /// type parameter: any `collect` may reach it.
    self_ty: Option<Adt>,
    /// The symbol of the impl's `from_iter`.
    symbol: SymbolId,
}

/// One workspace implementation of the standard `Iterator`.
#[derive(Debug, Clone)]
struct IteratorImpl {
    /// The iterator type, when the impl names one.
    self_ty: Option<Adt>,
    /// The impl's `next`, and its `size_hint` when it provides one: a `for`
    /// loop and an adaptor both call them without naming the impl.
    symbols: Vec<SymbolId>,
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
    /// The `fmt` methods of the workspace's `Display` and `Debug` impls, which
    /// a format string invokes without naming them.
    formatting: Vec<FormattingImpl>,
    /// The `from` methods of the workspace's `From` impls, which a `?` invokes
    /// without naming one.
    conversions: Vec<ConversionImpl>,
    /// The workspace's `Iterator` impls, which a `for` loop and an adaptor call
    /// through the trait.
    iterators: Vec<IteratorImpl>,
    /// The `from_iter` methods of the workspace's `FromIterator` impls, which
    /// a `collect` invokes without naming one.
    collects: Vec<CollectImpl>,
    /// The operator traits the workspace implements.
    ///
    /// Resolving an operator to ask what it calls forces the type inference of
    /// the whole body, so an operator whose trait has no impl in the workspace
    /// is skipped before that query: it could only reach the standard library.
    operators: HashSet<&'static str>,
    /// Where a derive or an attribute macro was invoked, and the macro it
    /// resolved to.
    ///
    /// The items such a macro generates are located at the invocation in the
    /// real file, so this is how they are found: see
    /// [`Collector::generated_items`].
    invocations: HashMap<Location, Vec<SymbolId>>,
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
        for index in 0..self.impls.len() {
            let (_, block) = self.impls[index];
            if let Some(found) = self.formatting_impl(block) {
                self.formatting.push(found);
            }
            if let Some(found) = self.conversion_impl(block) {
                self.conversions.push(found);
            }
            if let Some(found) = self.iterator_impl(block) {
                self.iterators.push(found);
            }
            if let Some(found) = self.collect_impl(block) {
                self.collects.push(found);
            }
            if let Some(name) = self.operator_trait(block) {
                self.operators.insert(name);
            }
        }
        for container in self.input.containers {
            // The semantics can only resolve nodes of trees it has parsed.
            self.guard(|| {
                self.input.sema.parse_or_expand(container.file);
                Some(())
            });
            let previous = self.quiet;
            self.quiet = container.file.macro_file().is_some();
            self.attrs(&container.node, container.file, container.id);
            self.visit(&container.node, container.id, 0, true);
            self.quiet = previous;
        }
        self.generated_items();
        self.sort_unresolved();
    }

    /// Adds the `Implements` edges of one trait impl block.
    fn implements(&mut self, id: SymbolId, block: Impl) {
        let db = self.input.db;
        let Some(trait_) = self.guard(|| block.trait_(db)) else {
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
        let Some(items) = self.guard(|| Some(block.items(db))) else {
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

    /// The formatting impl a block provides, if it is one of the standard
    /// traits.
    ///
    /// A `Display` the project defines itself is a trait like any other: its
    /// methods are called by name, so only the standard library's traits are
    /// recorded, and only their `fmt` is invoked implicitly.
    fn formatting_impl(&mut self, block: Impl) -> Option<FormattingImpl> {
        let db = self.input.db;
        let trait_ = self.guard(|| block.trait_(db))?;
        let standard = self.guard(|| {
            Some(matches!(
                trait_.module(db).krate(db).origin(db),
                CrateOrigin::Lang(_)
            ))
        })?;
        if !standard {
            return None;
        }
        let trait_ = self.guard(|| Some(trait_.name(db)))?;
        let trait_ = match trait_.as_str() {
            "Display" => FormattingTrait::Display,
            "Debug" => FormattingTrait::Debug,
            _ => return None,
        };
        let fmt = self.guard(|| {
            block.items(db).into_iter().find_map(|item| match item {
                AssocItem::Function(function) if function.name(db).as_str() == "fmt" => {
                    Some(function)
                }
                _ => None,
            })
        })?;
        let symbol = self.input.symbols.assoc_item(AssocItem::Function(fmt))?;
        let self_ty = self.guard(|| block.self_ty(db).strip_references().as_adt());
        Some(FormattingImpl {
            trait_,
            self_ty,
            symbol,
        })
    }

    /// The conversion impl a block provides, if it implements the standard
    /// `From`.
    ///
    /// A `From` the project defines itself is a trait like any other: its
    /// methods are called by name, so only the standard library's is invoked by
    /// `?` without one.
    fn conversion_impl(&mut self, block: Impl) -> Option<ConversionImpl> {
        let db = self.input.db;
        let reference = self.guard(|| block.trait_ref(db))?;
        let trait_ = self.guard(|| Some(reference.trait_()))?;
        let standard = self.guard(|| {
            Some(matches!(
                trait_.module(db).krate(db).origin(db),
                CrateOrigin::Lang(_)
            ))
        })?;
        if !standard || self.guard(|| Some(trait_.name(db)))?.as_str() != "From" {
            return None;
        }
        let from = self.guard(|| {
            block.items(db).into_iter().find_map(|item| match item {
                AssocItem::Function(function) if function.name(db).as_str() == "from" => {
                    Some(function)
                }
                _ => None,
            })
        })?;
        let symbol = self.input.symbols.assoc_item(AssocItem::Function(from))?;
        // The first argument of `From<T>` is the `Self` type, so the second is
        // the error type the conversion takes.
        let source = self.guard(|| reference.get_type_argument(1)?.strip_references().as_adt());
        Some(ConversionImpl { source, symbol })
    }

    /// Adds the dispatch edges of one `?`, which converts its operand's error
    /// through `From`.
    ///
    /// Which impl runs is decided by that error type, which is the second
    /// argument of the operand's `Result`; a `?` whose type this pass cannot
    /// read reaches every impl, as a coarse candidate.
    fn conversion_use(&mut self, from: SymbolId, node: &SyntaxNode, error: ValueType) {
        let Some(site) = self.site(node) else {
            return;
        };
        let targets: Vec<SymbolId> = self
            .conversions
            .iter()
            .filter(|impl_| error.can_reach(impl_.source))
            .map(|impl_| impl_.symbol)
            .collect();
        for target in targets {
            self.push(
                from,
                target,
                EdgeKind::MayDispatchTo,
                Certainty::Candidate,
                site.clone(),
                EdgeDetail::None,
            );
        }
    }

    /// The iterator impl a block provides, if it implements the standard
    /// `Iterator`.
    fn iterator_impl(&mut self, block: Impl) -> Option<IteratorImpl> {
        let db = self.input.db;
        let reference = self.guard(|| block.trait_ref(db))?;
        let trait_ = self.guard(|| Some(reference.trait_()))?;
        let standard = self.guard(|| {
            Some(matches!(
                trait_.module(db).krate(db).origin(db),
                CrateOrigin::Lang(_)
            ))
        })?;
        if !standard || self.guard(|| Some(trait_.name(db)))?.as_str() != "Iterator" {
            return None;
        }
        let mut symbols = Vec::new();
        for item in self.guard(|| Some(block.items(db)))? {
            let AssocItem::Function(function) = item else {
                continue;
            };
            let name = self.guard(|| Some(function.name(db)))?;
            if name.as_str() != "next" && name.as_str() != "size_hint" {
                continue;
            }
            if let Some(id) = self.input.symbols.assoc_item(item) {
                symbols.push(id);
            }
        }
        if symbols.is_empty() {
            return None;
        }
        let self_ty = self.guard(|| block.self_ty(db).strip_references().as_adt());
        Some(IteratorImpl { self_ty, symbols })
    }

    /// Adds the dispatch edges of one use of the iterator protocol.
    ///
    /// A `for` loop and every adaptor call `next` (and `size_hint`) through the
    /// trait, so no reference in the syntax names the impl of the iterator the
    /// loop was given. The iterator's type decides which impl can run; one this
    /// pass cannot name reaches every impl, as a coarse candidate.
    fn iterator_use(&mut self, from: SymbolId, node: &SyntaxNode, iterator: ValueType) {
        let Some(site) = self.site(node) else {
            return;
        };
        let targets: Vec<SymbolId> = self
            .iterators
            .iter()
            .filter(|impl_| iterator.can_reach(impl_.self_ty))
            .flat_map(|impl_| impl_.symbols.iter().copied())
            .collect();
        for target in targets {
            self.push(
                from,
                target,
                EdgeKind::MayDispatchTo,
                Certainty::Candidate,
                site.clone(),
                EdgeDetail::None,
            );
        }
    }

    /// Whether a function is one of the iterator protocol's methods.
    fn is_iterator_method(&mut self, function: Function) -> bool {
        let db = self.input.db;
        self.guard(|| {
            let container = function.as_assoc_item(db)?.container(db);
            let trait_ = match container {
                AssocItemContainer::Trait(trait_) => trait_,
                AssocItemContainer::Impl(block) => block.trait_(db)?,
            };
            Some(
                trait_.name(db).as_str() == "Iterator"
                    && matches!(trait_.module(db).krate(db).origin(db), CrateOrigin::Lang(_)),
            )
        }) == Some(true)
    }

    /// The collection impl a block provides, if it implements the standard
    /// `FromIterator`.
    ///
    /// `collect` builds a collection by calling the `from_iter` of the type it
    /// is asked for, so a `from_iter` of this workspace runs without any
    /// reference naming it.
    fn collect_impl(&mut self, block: Impl) -> Option<CollectImpl> {
        let db = self.input.db;
        let reference = self.guard(|| block.trait_ref(db))?;
        let trait_ = self.guard(|| Some(reference.trait_()))?;
        let standard = self.guard(|| {
            Some(matches!(
                trait_.module(db).krate(db).origin(db),
                CrateOrigin::Lang(_)
            ))
        })?;
        if !standard || self.guard(|| Some(trait_.name(db)))?.as_str() != "FromIterator" {
            return None;
        }
        let from_iter = self.guard(|| {
            block.items(db).into_iter().find_map(|item| match item {
                AssocItem::Function(function) if function.name(db).as_str() == "from_iter" => {
                    Some(function)
                }
                _ => None,
            })
        })?;
        let symbol = self
            .input
            .symbols
            .assoc_item(AssocItem::Function(from_iter))?;
        let self_ty = self.guard(|| block.self_ty(db).strip_references().as_adt());
        Some(CollectImpl { self_ty, symbol })
    }

    /// Adds the dispatch edges of one `collect`, which builds the collection by
    /// calling the `from_iter` of its type.
    ///
    /// The type of the call decides which impl can run; a collection type this
    /// pass cannot name reaches every impl, as a coarse candidate.
    fn collect_use(&mut self, from: SymbolId, node: &SyntaxNode, collection: ValueType) {
        let Some(site) = self.site(node) else {
            return;
        };
        let targets: Vec<SymbolId> = self
            .collects
            .iter()
            .filter(|impl_| collection.can_reach(impl_.self_ty))
            .map(|impl_| impl_.symbol)
            .collect();
        for target in targets {
            self.push(
                from,
                target,
                EdgeKind::MayDispatchTo,
                Certainty::Candidate,
                site.clone(),
                EdgeDetail::None,
            );
        }
    }

    /// Whether a function is the standard `Iterator::collect`, which builds a
    /// collection through `FromIterator`.
    fn is_collect(&mut self, function: Function) -> bool {
        let db = self.input.db;
        self.guard(|| {
            let container = function.as_assoc_item(db)?.container(db);
            let trait_ = match container {
                AssocItemContainer::Trait(trait_) => trait_,
                AssocItemContainer::Impl(block) => block.trait_(db)?,
            };
            Some(
                function.name(db).as_str() == "collect"
                    && trait_.name(db).as_str() == "Iterator"
                    && matches!(trait_.module(db).krate(db).origin(db), CrateOrigin::Lang(_)),
            )
        }) == Some(true)
    }

    /// Visits a `for` loop, whose iteration calls the iterator's own `next`.
    fn for_expr(&mut self, expr: &ast::ForExpr, from: SymbolId, depth: u32) {
        if let Some(iterable) = expr.iterable() {
            let iterator = self.typed_value(&iterable);
            self.iterator_use(from, iterable.syntax(), iterator);
            self.visit(iterable.syntax(), from, depth, false);
        }
        if let Some(body) = expr.loop_body() {
            self.visit(body.syntax(), from, depth, false);
        }
    }

    /// The error type of the operand of a `?`.
    fn error_type(&mut self, expr: &ast::Expr) -> ValueType {
        let Some(info) = self.guard(|| self.input.sema.type_of_expr(expr)) else {
            return ValueType::Unknown;
        };
        let ty = info.original.strip_references();
        let Some((adt, arguments)) = ty.as_adt_with_args() else {
            return self.value_of_type(ty);
        };
        let name = match adt {
            Adt::Struct(it) => it.name(self.input.db),
            Adt::Enum(it) => it.name(self.input.db),
            Adt::Union(it) => it.name(self.input.db),
        };
        if name.as_str() != "Result" {
            return ValueType::Unknown;
        }
        match arguments.get(1).cloned().flatten() {
            Some(error) => self.value_of_type(error),
            None => ValueType::Unknown,
        }
    }

    /// Adds the dispatch edges of one formatting invocation.
    ///
    /// A format string calls the `fmt` of the value it prints without naming
    /// it, so no reference in the syntax resolves to the implementation. The
    /// value's type decides which impl can run; a value whose type this pass
    /// cannot name reaches every impl of the trait, as a coarse candidate.
    fn formatting_use(
        &mut self,
        from: SymbolId,
        site: Location,
        traits: &[FormattingTrait],
        value: ValueType,
    ) {
        let targets: Vec<SymbolId> = self
            .formatting
            .iter()
            .filter(|impl_| traits.contains(&impl_.trait_) && value.can_reach(impl_.self_ty))
            .map(|impl_| impl_.symbol)
            .collect();
        for target in targets {
            self.push(
                from,
                target,
                EdgeKind::MayDispatchTo,
                Certainty::Candidate,
                site.clone(),
                EdgeDetail::None,
            );
        }
    }

    /// The same, for a formatting use written at a node.
    fn formatting_site(
        &mut self,
        from: SymbolId,
        node: &SyntaxNode,
        traits: &[FormattingTrait],
        value: ValueType,
    ) {
        let Some(site) = self.site(node) else {
            return;
        };
        self.formatting_use(from, site, traits, value);
    }

    /// Which formatting impls one name of a format string can reach.
    ///
    /// A name in a template is not an expression, so it has no syntax node to
    /// infer: a local's type is read from its binding, a constant's from its
    /// declaration, and a unit struct or variant is its own type.
    fn typed_resolution(&mut self, resolution: PathResolution<'_>) -> Option<ValueType> {
        let db = self.input.db;
        let ty = match resolution {
            PathResolution::Local(local) => Some(self.guard(|| Some(local.ty(db)))?),
            PathResolution::Def(ModuleDef::Const(item)) => Some(self.guard(|| Some(item.ty(db)))?),
            PathResolution::Def(ModuleDef::Static(item)) => Some(self.guard(|| Some(item.ty(db)))?),
            PathResolution::Def(ModuleDef::Adt(adt)) => return Some(ValueType::Data(adt)),
            PathResolution::Def(ModuleDef::EnumVariant(variant)) => {
                return Some(ValueType::Data(Adt::Enum(
                    self.guard(|| Some(variant.parent_enum(db)))?,
                )));
            }
            _ => None,
        };
        ty.map(|ty| self.value_of_type(ty))
    }

    /// Which formatting impls the value of one expression can reach.
    fn typed_value(&mut self, expr: &ast::Expr) -> ValueType {
        let Some(info) = self.guard(|| self.input.sema.type_of_expr(expr)) else {
            return ValueType::Unknown;
        };
        self.value_of_type(info.original)
    }

    /// Which formatting impls a value of this type can reach.
    fn value_of_type(&mut self, ty: ra_ap_hir::Type<'_>) -> ValueType {
        let ty = ty.strip_references();
        if let Some(adt) = ty.as_adt() {
            return ValueType::Data(adt);
        }
        // An unknown type and a trait object are formatted by an impl this
        // pass cannot pick; a type parameter is too.
        if ty.is_unknown() || ty.as_dyn_trait().is_some() {
            return ValueType::Unknown;
        }
        let db = self.input.db;
        if self
            .guard(|| Some(ty.as_type_param(db).is_some()))
            .unwrap_or(false)
        {
            return ValueType::Unknown;
        }
        // A type that is known and is not a data type — a number, a string
        // slice, a tuple — has no impl in the workspace to dispatch to.
        ValueType::Other
    }

    /// The item of `trait_` that `item` of an impl block implements.
    fn trait_item(&self, trait_: Trait, item: AssocItem) -> Option<AssocItem> {
        let db = self.input.db;
        let name = self.guard(|| item.name(db))?;
        let items = self.guard(|| Some(trait_.items(db)))?;
        items.into_iter().find(|candidate| {
            same_kind(*candidate, item) && self.guard(|| candidate.name(db)) == Some(name.clone())
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
        // Attributes are not part of the signature or the body of anything, but
        // the ones that invoke a macro are references like any other; they were
        // read with the item that declares them.
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
            self.tuple_pattern_fields(&pat, from);
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
        if let Some(expr) = ast::TryExpr::cast(node.clone()) {
            self.try_expr(&expr, from, depth);
            return;
        }
        if let Some(expr) = ast::ForExpr::cast(node.clone()) {
            self.for_expr(&expr, from, depth);
            return;
        }
        if let Some(expr) = ast::PathExpr::cast(node.clone()) {
            self.path_expr(&expr, from, depth);
            return;
        }
        if ast::BinExpr::can_cast(node.kind())
            || ast::PrefixExpr::can_cast(node.kind())
            || ast::IndexExpr::can_cast(node.kind())
        {
            self.operator_expr(node, from);
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
        let Some(expanded) = self.guard(|| self.input.sema.expand_macro_call(call)) else {
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
        let Some(macro_) = self.guard(|| self.input.sema.resolve_macro_call(call)) else {
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

    /// Adds the `Expands` edges of the attributes of the item a region belongs
    /// to.
    ///
    /// Two kinds of procedural macro are invoked by an attribute: a derive
    /// names its macros in `#[derive(..)]`, while an attribute macro is an
    /// attribute itself. Both build the item they are written on, and a derive
    /// additionally builds the impls, the items inside them and whatever else
    /// its output holds, which [`Collector::generated_items`] covers.
    ///
    /// The item is read where it is written: an item a macro produced carries
    /// the attributes of the code the macro was invoked at, while an attribute
    /// a macro writes itself is no choice of that item and names no macro.
    fn attrs(&mut self, node: &SyntaxNode, file: HirFileId, from: SymbolId) {
        let Some(item) = self.written_item(node, file) else {
            return;
        };
        let attrs: Vec<ast::Attr> = item.attrs().collect();
        if attrs.is_empty() {
            return;
        }
        // The definition map already knows whether an attribute macro applies
        // to the item, which is much cheaper than resolving the attributes of
        // every item of the workspace.
        let attribute_macro = self.guard(|| {
            let file = self.input.sema.hir_file_for(item.syntax());
            Some(self.input.sema.is_attr_macro_call(InFile::new(file, &item)))
        });
        for attr in attrs {
            if self.derives(&attr, from) {
                continue;
            }
            if attribute_macro.unwrap_or(false) {
                self.attribute_macro(&attr, from);
            }
        }
    }

    /// The item whose attributes are written at `node`, which may be code a
    /// macro wrote.
    ///
    /// An item inside a macro expansion is reached through the call the macro
    /// was invoked at, until the code is written in a real file.
    fn written_item(&self, node: &SyntaxNode, file: HirFileId) -> Option<ast::Item> {
        let mut file = file;
        let mut node = node.clone();
        while let Some(call) = file.macro_file() {
            let call = call.call_node(self.input.db);
            file = call.file_id;
            node = call.value;
        }
        ast::Item::cast(node)
    }

    /// Adds the `Expands` edges of one `#[derive(..)]` attribute, if it is one.
    ///
    /// A derive of another crate, the standard library's included, resolves to
    /// a macro of that crate and produces nothing: only a macro of a workspace
    /// member is a symbol of the graph.
    fn derives(&mut self, attr: &ast::Attr, from: SymbolId) -> bool {
        let Some(meta) = attr.meta() else {
            return false;
        };
        if !meta
            .simple_name()
            .is_some_and(|name| name.as_str() == "derive")
        {
            return false;
        }
        let Some(site) = self.site(meta.syntax()) else {
            return true;
        };
        let derives = self.guard(|| self.input.sema.resolve_derive_macro(&meta));
        for derive in derives.into_iter().flatten().flatten() {
            self.invocation(derive, from, &site);
        }
        true
    }

    /// Adds the `Expands` edge of an attribute macro on an item.
    fn attribute_macro(&mut self, attr: &ast::Attr, from: SymbolId) {
        let Some(meta) = attr.meta() else {
            return;
        };
        let Some(path) = meta.path() else {
            return;
        };
        let resolved = self.guard(|| self.input.sema.resolve_path_per_ns(&path));
        let Some(PathResolution::Def(ModuleDef::Macro(macro_))) =
            resolved.and_then(|resolution| resolution.macro_ns)
        else {
            return;
        };
        let Some(site) = self.site(meta.syntax()) else {
            return;
        };
        self.invocation(macro_, from, &site);
    }

    /// Adds the `Expands` edge of one derive or attribute macro invocation.
    ///
    /// The site is the range of the attribute, which is also where every item
    /// the macro generates is located; [`Collector::generated_items`] uses that
    /// to give those items the edge as well.
    fn invocation(&mut self, macro_: Macro, from: SymbolId, site: &Location) {
        let Some(to) = self.input.symbols.get(DefKey::Macro(macro_)) else {
            return;
        };
        self.push(
            from,
            to,
            EdgeKind::Expands,
            Certainty::Resolved,
            site.clone(),
            EdgeDetail::None,
        );
        let recorded = self.invocations.entry(site.clone()).or_default();
        if !recorded.contains(&to) {
            recorded.push(to);
        }
    }

    /// Adds an `Expands` edge from every item a derive or an attribute macro
    /// generated to the macro that generated it.
    ///
    /// Such an item has no text of its own: it is located at the attribute that
    /// produced it, which is the site its invocation recorded. Its users then
    /// follow through the ordinary edges, so a change to the macro reaches the
    /// code it wrote as well as the item it was invoked on.
    fn generated_items(&mut self) {
        if self.invocations.is_empty() {
            return;
        }
        for (index, symbol) in self.input.graph.symbols.iter().enumerate() {
            if !symbol.flags.from_macro {
                continue;
            }
            let Some(sources) = self
                .invocations
                .get(&symbol.location)
                .cloned()
                .filter(|sources| !sources.is_empty())
            else {
                continue;
            };
            let from = SymbolId(index as u32);
            let sites = symbol.location.clone();
            for to in sources {
                if to == from {
                    continue;
                }
                self.push(
                    from,
                    to,
                    EdgeKind::Expands,
                    Certainty::Resolved,
                    sites.clone(),
                    EdgeDetail::None,
                );
            }
        }
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
                        self.tuple_literal_fields(&path, call.arg_list(), from);
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

    /// The operator trait a block implements, when it is one of the standard
    /// operator traits.
    fn operator_trait(&mut self, block: Impl) -> Option<&'static str> {
        let reference = self.guard(|| block.trait_ref(self.input.db))?;
        let name = self.guard(|| Some(reference.trait_().name(self.input.db)))?;
        OPERATOR_TRAITS
            .iter()
            .copied()
            .find(|known| *known == name.as_str())
    }

    /// Adds the edge of an operator expression, which calls the operator's
    /// trait method.
    ///
    /// `left * right` is `<impl Mul for T>::mul(left, right)`: no path in the
    /// syntax names that method, so without this edge a changed `mul` has no
    /// callers and operator-heavy code looks unused. The same holds for the
    /// prefix operators (`-`, `*`, `!`), for indexing and for comparisons.
    fn operator_expr(&mut self, node: &SyntaxNode, from: SymbolId) {
        if !operator_traits(node)
            .iter()
            .any(|name| self.operators.contains(name))
        {
            return;
        }
        let sema = self.input.sema;
        let resolved = if let Some(expr) = ast::BinExpr::cast(node.clone()) {
            self.guard(|| sema.resolve_bin_expr(&expr))
        } else if let Some(expr) = ast::PrefixExpr::cast(node.clone()) {
            self.guard(|| sema.resolve_prefix_expr(&expr))
        } else if let Some(expr) = ast::IndexExpr::cast(node.clone()) {
            self.guard(|| sema.resolve_index_expr(&expr))
        } else {
            None
        };
        if let Some(function) = resolved {
            self.call_to(function, from, node);
        }
    }

    /// Adds the edge of a path in callee position.
    fn path_call(&mut self, path: &ast::Path, from: SymbolId) {
        match self.guard(|| self.input.sema.resolve_path(path)) {
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
            let resolved = self.guard(|| self.input.sema.resolve_method_call_fallback(call));
            match resolved {
                Some((target, _)) => {
                    let target = target.either(MethodTarget::Function, MethodTarget::Field);
                    match target {
                        MethodTarget::Function(function) => {
                            self.call_to(function, from, name_ref.syntax());
                            self.dispatch_to_display(function, from, call);
                            // An adaptor call on an iterator runs the iterator's
                            // own `next` from inside the standard library.
                            if self.is_iterator_method(function)
                                && let Some(receiver) = call.receiver()
                            {
                                let iterator = self.typed_value(&receiver);
                                self.iterator_use(from, name_ref.syntax(), iterator);
                            }
                            // `collect` builds the collection the type asks for
                            // through `FromIterator`, which nothing names.
                            if self.is_collect(function) {
                                let collection =
                                    self.typed_value(&ast::Expr::MethodCallExpr(call.clone()));
                                self.collect_use(from, name_ref.syntax(), collection);
                            }
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

    /// Adds the dispatch edges of a `to_string` call.
    ///
    /// `ToString::to_string` is implemented in the standard library and calls
    /// `Display::fmt` of the receiver, so the workspace impl that can run is
    /// reached through the receiver's type and not through a reference.
    fn dispatch_to_display(
        &mut self,
        function: Function,
        from: SymbolId,
        call: &ast::MethodCallExpr,
    ) {
        if !self.is_to_string(function) {
            return;
        }
        let Some(receiver) = call.receiver() else {
            return;
        };
        let value = self.typed_value(&receiver);
        self.formatting_site(from, receiver.syntax(), &[FormattingTrait::Display], value);
    }

    /// Whether a function is `ToString::to_string`, which calls the `Display`
    /// implementation of whatever it is given.
    fn is_to_string(&mut self, function: Function) -> bool {
        let db = self.input.db;
        self.guard(|| {
            let container = function.as_assoc_item(db)?.container(db);
            // `to_string` comes from the trait's declaration or from the
            // blanket implementation the standard library provides for it.
            let trait_ = match container {
                AssocItemContainer::Trait(trait_) => trait_,
                AssocItemContainer::Impl(block) => block.trait_(db)?,
            };
            Some(
                function.name(db).as_str() == "to_string"
                    && trait_.name(db).as_str() == "ToString"
                    && matches!(trait_.module(db).krate(db).origin(db), CrateOrigin::Lang(_)),
            )
        }) == Some(true)
    }

    /// Adds the edge of a call to `function`.
    ///
    /// A call that resolves to a trait method declaration dispatches through the
    /// receiver, so every implementation of the trait in the workspace may be
    /// reached as well.
    fn call_to(&mut self, function: Function, from: SymbolId, node: &SyntaxNode) {
        let container = self.guard(|| {
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
        let Some(name) = self.guard(|| Some(method.name(db))) else {
            return;
        };
        for index in 0..self.impls.len() {
            let block = self.impls[index].1;
            if self.guard(|| block.trait_(db)) != Some(trait_) {
                continue;
            }
            let Some(items) = self.guard(|| Some(block.items(db))) else {
                continue;
            };
            for item in items {
                let AssocItem::Function(candidate) = item else {
                    continue;
                };
                if self.guard(|| Some(candidate.name(db))) != Some(name.clone()) {
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
        match self.guard(|| self.input.sema.resolve_field(field)) {
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
        match self.guard(|| self.input.sema.resolve_variant(expr.clone())) {
            Some(variant) => self.constructs_variant(&variant, from, expr.syntax(), spread),
            None => {
                if let Some(path) = &path
                    && let Some(PathResolution::Def(ModuleDef::Adt(adt))) =
                        self.guard(|| self.input.sema.resolve_path(path))
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
        // A field named in a literal is not read: the literal itself is what
        // constructs the value. It does name the field, so a change to that
        // field has to reach this site, and a field the literal does not name
        // leaves it alone.
        for field in list.fields() {
            self.literal_field(&field, from);
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
                .guard(|| self.input.sema.type_of_expr(&scrutinee))
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
            if let Some((field_def, _)) =
                self.guard(|| self.input.sema.resolve_record_pat_field(&field))
                && let Some(id) = self.input.symbols.get(DefKey::Field(field_def))
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
    /// to a struct or a variant: the pattern then matches whatever fields the
    /// struct or variant grows, and leaves the fields it already has to be
    /// checked individually.
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
        match self.guard(|| self.input.sema.resolve_path(&path)) {
            Some(PathResolution::Def(def)) => {
                let leaves_fields_out = rest
                    && matches!(
                        def,
                        ModuleDef::Adt(Adt::Struct(_)) | ModuleDef::EnumVariant(_)
                    );
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
        match self.guard(|| self.input.sema.resolve_path(&path)) {
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
                // `map(ToString::to_string)` hands the formatting trait to a
                // caller instead of calling it, so the value it formats is not
                // named where it is formatted.
                if let ModuleDef::Function(function) = def
                    && self.is_to_string(function)
                {
                    self.formatting_site(
                        from,
                        path.syntax(),
                        &[FormattingTrait::Display],
                        ValueType::Unknown,
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
        let template = expr
            .syntax()
            .descendants_with_tokens()
            .filter_map(|child| child.into_token())
            .find_map(ast::String::cast);
        let traits = format_traits(&template);
        if let Some(string) = template.clone() {
            let file = string
                .syntax()
                .parent()
                .and_then(|node| self.guard(|| Some(self.input.sema.hir_file_for(&node))));
            if let Some(file) = file
                && let Some(parts) = self.guard(|| self.input.sema.as_format_args_parts(&string))
            {
                for (range, resolution) in parts {
                    let Some(resolution) =
                        resolution.and_then(|part| part.either(Some, |_asm_operand| None))
                    else {
                        continue;
                    };
                    let Some(site) = self.guard(|| self.input.locate.location(file, range)) else {
                        continue;
                    };
                    // A part that names an item is a value reference.
                    if let PathResolution::Def(def) = resolution
                        && let Some((key, kind)) = value_target(def)
                        && let Some(id) = self.input.symbols.get(key)
                    {
                        self.push(
                            from,
                            id,
                            kind,
                            Certainty::Resolved,
                            site.clone(),
                            EdgeDetail::None,
                        );
                    }
                    // The part may also print a value, which runs the `fmt` of
                    // the value's formatting trait without naming it.
                    if !traits.is_empty()
                        && let Some(value) = self.typed_resolution(resolution)
                    {
                        self.formatting_use(from, site, &traits, value);
                    }
                }
            }
        }
        // The values the template prints are handed to `fmt` of the formatting
        // traits it names, which is a dispatch no reference in the syntax
        // resolves.
        if !traits.is_empty() {
            for argument in expr
                .syntax()
                .children()
                .filter_map(ast::FormatArgsArg::cast)
            {
                let Some(argument) = argument.expr() else {
                    continue;
                };
                let value = self.typed_value(&argument);
                self.formatting_site(from, argument.syntax(), &traits, value);
            }
        }
        // The explicit arguments are expressions of the expansion.
        for child in expr.syntax().children() {
            self.visit(&child, from, depth, false);
        }
    }

    /// Visits a `?`, which converts its operand's error to the error of the
    /// function it is written in.
    fn try_expr(&mut self, expr: &ast::TryExpr, from: SymbolId, depth: u32) {
        let operand = expr.expr();
        if let Some(operand) = &operand {
            // A `?` runs `From::from` on the error type of its operand: a
            // conversion no reference in the syntax names.
            let error = self.error_type(operand);
            self.conversion_use(from, expr.syntax(), error);
            self.visit(operand.syntax(), from, depth, false);
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
        match self.guard(|| self.input.sema.resolve_path(&path)) {
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

    /// Adds the `WritesField` edge of a field named in a literal.
    ///
    /// The literal supplies the field: it spells its name out and gives it a
    /// value, so a removed or retyped field has to reach this site, and a field
    /// the literal leaves to a `..base` does not.
    fn literal_field(&mut self, field: &ast::RecordExprField, from: SymbolId) {
        let resolved = self.guard(|| self.input.sema.resolve_record_field(field));
        let Some((def, _, _)) = resolved else {
            return;
        };
        let Some(id) = self.input.symbols.get(DefKey::Field(def)) else {
            return;
        };
        let node = field
            .name_ref()
            .map_or_else(|| field.syntax().clone(), |name| name.syntax().clone());
        self.located_edge(
            from,
            id,
            EdgeKind::WritesField,
            Certainty::Resolved,
            &node,
            EdgeDetail::None,
        );
    }

    /// Adds a `WritesField` edge to every positional field a tuple constructor
    /// literal supplies.
    ///
    /// `Wrapper(1)` names its field by position rather than by name; the field
    /// it supplies is the one at that position.
    fn tuple_literal_fields(
        &mut self,
        path: &ast::Path,
        args: Option<ast::ArgList>,
        from: SymbolId,
    ) {
        let Some(args) = args else {
            return;
        };
        let Some(fields) = self.container_fields(path) else {
            return;
        };
        for (index, arg) in args.args().enumerate() {
            let Some(field) = fields.get(index) else {
                return;
            };
            if let Some(id) = self.input.symbols.get(DefKey::Field(*field)) {
                self.located_edge(
                    from,
                    id,
                    EdgeKind::WritesField,
                    Certainty::Resolved,
                    arg.syntax(),
                    EdgeDetail::None,
                );
            }
        }
    }

    /// Adds a `Matches` edge to every positional field a tuple pattern names.
    ///
    /// A pattern with a `..` does not line its fields up with the declaration
    /// positions, so only a pattern that names every field gets one.
    fn tuple_pattern_fields(&mut self, pat: &ast::TupleStructPat, from: SymbolId) {
        let Some(path) = pat.path() else {
            return;
        };
        let Some(fields) = self.container_fields(&path) else {
            return;
        };
        let inner: Vec<ast::Pat> = pat.fields().collect();
        if inner.len() != fields.len() {
            return;
        }
        for (sub, field) in inner.iter().zip(fields.iter()) {
            let Some(id) = self.input.symbols.get(DefKey::Field(*field)) else {
                continue;
            };
            let detail = self.match_detail(sub.syntax());
            self.located_edge(
                from,
                id,
                EdgeKind::Matches,
                Certainty::Resolved,
                sub.syntax(),
                detail,
            );
        }
    }

    /// The fields of the struct or variant a path names, in declaration order.
    fn container_fields(&mut self, path: &ast::Path) -> Option<Vec<Field>> {
        let db = self.input.db;
        self.guard(|| match self.input.sema.resolve_path(path)? {
            PathResolution::Def(ModuleDef::Adt(Adt::Struct(it))) => Some(it.fields(db)),
            PathResolution::Def(ModuleDef::Adt(Adt::Union(it))) => Some(it.fields(db)),
            PathResolution::Def(ModuleDef::EnumVariant(variant)) => Some(variant.fields(db)),
            _ => None,
        })
    }

    /// Adds the `Constructs` edge of a struct, union or enum variant literal.
    ///
    /// `spread` says that the literal ends with `..base`, which is recorded on the
    /// edge to a struct or a variant.
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
    /// Only a struct or a variant field can be given from another value of its
    /// type, and only the literal that does it leaves fields out; any other
    /// literal names every field.
    fn constructs_detail(&self, id: SymbolId, spread: bool) -> EdgeDetail {
        if !spread {
            return EdgeDetail::None;
        }
        let symbol = &self.input.graph.symbols[id.0 as usize];
        if matches!(symbol.path.kind, SymbolKind::Struct | SymbolKind::Variant) {
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
                    .guard(|| self.input.sema.resolve_bind_pat_to_const(ident))
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
        self.guard(|| {
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

/// The traits one arithmetic operator invokes, plain and assigning.
fn arithmetic_traits(op: ast::ArithOp, assignment: bool) -> &'static [&'static str] {
    match (op, assignment) {
        (ast::ArithOp::Add, false) => &["Add"],
        (ast::ArithOp::Add, true) => &["AddAssign"],
        (ast::ArithOp::Sub, false) => &["Sub"],
        (ast::ArithOp::Sub, true) => &["SubAssign"],
        (ast::ArithOp::Mul, false) => &["Mul"],
        (ast::ArithOp::Mul, true) => &["MulAssign"],
        (ast::ArithOp::Div, false) => &["Div"],
        (ast::ArithOp::Div, true) => &["DivAssign"],
        (ast::ArithOp::Rem, false) => &["Rem"],
        (ast::ArithOp::Rem, true) => &["RemAssign"],
        (ast::ArithOp::Shl, false) => &["Shl"],
        (ast::ArithOp::Shl, true) => &["ShlAssign"],
        (ast::ArithOp::Shr, false) => &["Shr"],
        (ast::ArithOp::Shr, true) => &["ShrAssign"],
        (ast::ArithOp::BitXor, false) => &["BitXor"],
        (ast::ArithOp::BitXor, true) => &["BitXorAssign"],
        (ast::ArithOp::BitOr, false) => &["BitOr"],
        (ast::ArithOp::BitOr, true) => &["BitOrAssign"],
        (ast::ArithOp::BitAnd, false) => &["BitAnd"],
        (ast::ArithOp::BitAnd, true) => &["BitAndAssign"],
    }
}

/// The operator traits one operator expression may invoke.
///
/// `&&` and `||` are not calls, and a plain `=` assigns a place rather than
/// invoking a trait.
fn operator_traits(node: &SyntaxNode) -> &'static [&'static str] {
    if let Some(expr) = ast::BinExpr::cast(node.clone()) {
        return match expr.op_kind() {
            Some(ast::BinaryOp::ArithOp(op)) => arithmetic_traits(op, false),
            Some(ast::BinaryOp::Assignment { op: Some(op) }) => arithmetic_traits(op, true),
            Some(ast::BinaryOp::CmpOp(ast::CmpOp::Eq { .. })) => &["PartialEq"],
            Some(ast::BinaryOp::CmpOp(ast::CmpOp::Ord { .. })) => &["PartialOrd"],
            Some(ast::BinaryOp::Assignment { op: None } | ast::BinaryOp::LogicOp(_)) | None => &[],
        };
    }
    if let Some(expr) = ast::PrefixExpr::cast(node.clone()) {
        return match expr.op_kind() {
            Some(ast::UnaryOp::Deref) => &["Deref", "DerefMut"],
            Some(ast::UnaryOp::Not) => &["Not"],
            Some(ast::UnaryOp::Neg) => &["Neg"],
            None => &[],
        };
    }
    if ast::IndexExpr::can_cast(node.kind()) {
        return &["Index", "IndexMut"];
    }
    &[]
}

/// Which formatting traits a format string invokes.
///
/// `{}` prints its value with `Display` and `{:?}` (and `{:#?}`) with `Debug`,
/// so the placeholders of the template decide what a site may call. A template
/// that prints both is reported as both; one without a placeholder prints
/// nothing and reaches nothing.
fn format_traits(template: &Option<ast::String>) -> Vec<FormattingTrait> {
    let Some(string) = template else {
        return Vec::new();
    };
    let text = string.syntax().text().to_string();
    let mut traits: Vec<FormattingTrait> = Vec::new();
    let mut placeholder = false;
    let mut spec = false;
    let mut debug = false;
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            // `{{` and `}}` print a brace instead of starting a placeholder.
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
            }
            '{' => {
                placeholder = true;
                spec = false;
                debug = false;
            }
            ':' if placeholder => spec = true,
            '?' if placeholder && spec => debug = true,
            '}' if placeholder => {
                placeholder = false;
                let found = if debug {
                    FormattingTrait::Debug
                } else {
                    FormattingTrait::Display
                };
                if !traits.contains(&found) {
                    traits.push(found);
                }
            }
            _ => {}
        }
    }
    traits
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
