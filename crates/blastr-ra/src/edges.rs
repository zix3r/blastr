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

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use blastr_model::{
    Certainty, Edge, EdgeDetail, EdgeKind, Location, RelPath, RevisionGraph, Span, SymbolId,
    SymbolKind, UnresolvedRef,
};
use ra_ap_hir::{
    Adt, AsAssocItem, AssocItem, AssocItemContainer, Crate, Field, Function, HirFileId, Impl,
    InFile, Macro, ModuleDef, Name, PathResolution, Semantics, Trait, db::HirDatabase,
};
use ra_ap_ide_db::RootDatabase;
use ra_ap_ide_db::base_db::{Crate as BaseCrate, CrateOrigin};
use ra_ap_ide_db::defs::{Definition, NameClass, NameRefClass};
use ra_ap_syntax::ast::{HasArgList, HasAttrs};
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
/// inference of the whole body for nothing. Only the standard library's traits count: a trait
/// of the project that is merely named `Add` is not what `+` calls.
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
    pub(crate) sema: &'a Semantics<'db, RootDatabase>,
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
    /// The queries that panicked, whose edges are missing.
    pub(crate) failed: u32,
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
        inactive: inactive_by_file(input.inactive),
        impl_fns: HashMap::new(),
        implicit: HashMap::new(),
        operators: HashSet::new(),
        invocations: HashMap::new(),
        quiet: false,
        expansion_site: None,
        failed: Cell::new(0),
    };
    collector.run();
    Output {
        edges: collector.edges,
        unresolved: collector.unresolved,
        failed: collector.failed.get(),
    }
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

/// A standard trait whose methods the code calls without naming them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Implicit {
    /// `Display`, which `{}` and `to_string` invoke.
    Display,
    /// `Debug`, which `{:?}` invokes.
    Debug,
    /// `From`, which `?` invokes on the error of its operand.
    From,
    /// `Iterator`, which a `for` loop and every adaptor call.
    Iterator,
    /// `FromIterator`, which `collect` invokes.
    FromIterator,
}

/// The standard traits the code calls without naming them: the trait, its
/// name in the standard library, and the methods that run.
///
/// A trait of the same name that the project defines itself is a trait like
/// any other: its methods are called by name.
const IMPLICIT: [(Implicit, &str, &[&str]); 5] = [
    (Implicit::Display, "Display", &["fmt"]),
    (Implicit::Debug, "Debug", &["fmt"]),
    (Implicit::From, "From", &["from"]),
    (Implicit::Iterator, "Iterator", &["next", "size_hint"]),
    (Implicit::FromIterator, "FromIterator", &["from_iter"]),
];

impl Implicit {
    /// The methods of the trait that its implicit calls run.
    fn methods(self) -> &'static [&'static str] {
        IMPLICIT
            .iter()
            .find(|(family, _, _)| *family == self)
            .map_or(&[], |(_, _, methods)| methods)
    }
}

/// One function of a workspace impl of a trait.
#[derive(Debug, Clone)]
struct ImplFn {
    /// The data type the impl applies to: the self type, and for `From` the
    /// type converted from.
    ///
    /// `None` for a type this pass cannot name, such as a reference or a type
    /// parameter: any value may reach it.
    ty: Option<Adt>,
    /// The name of the function.
    name: Name,
    /// The symbol of the function.
    symbol: SymbolId,
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
    /// The code ranges that are inactive under the analysed configuration, by
    /// file and sorted by their start.
    inactive: HashMap<RelPath, Vec<Span>>,
    /// The functions of the workspace's trait impls, by trait, which a call
    /// through the trait may dispatch to.
    impl_fns: HashMap<Trait, Vec<ImplFn>>,
    /// The implicitly called standard traits the workspace implements.
    implicit: HashMap<Implicit, Trait>,
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
    /// The queries that panicked, whose edges are missing.
    failed: Cell<u32>,
}

impl<'db> Collector<'_, 'db> {
    /// Walks every region and adds the edges that do not come from a region.
    fn run(&mut self) {
        for container in self.input.containers {
            if let Some(DefKey::Impl(block)) = container.def {
                self.index_impl(container.id, block);
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

    /// Adds the `Implements` edges of one impl block and records the functions
    /// of a trait impl, which calls through the trait may dispatch to.
    fn index_impl(&mut self, id: SymbolId, block: Impl) {
        let db = self.input.db;
        let Some(trait_) = self.guard(|| block.trait_(db)) else {
            return;
        };
        let items = self.guard(|| Some(block.items(db))).unwrap_or_default();
        self.implements(id, trait_, &items);
        let Some(name) = self.guard(|| Some(trait_.name(db))) else {
            return;
        };
        let standard = self.is_std(trait_);
        if let Some(operator) = OPERATOR_TRAITS
            .iter()
            .find(|known| standard && **known == name.as_str())
        {
            self.operators.insert(operator);
        }
        let family = IMPLICIT
            .iter()
            .find(|(_, known, _)| standard && *known == name.as_str())
            .map(|(family, _, _)| *family);
        if let Some(family) = family {
            self.implicit.insert(family, trait_);
        }
        let ty = self.guard(|| {
            let ty = if family == Some(Implicit::From) {
                // The first argument of `From<T>` is the `Self` type, so the
                // second is the type the conversion takes.
                block.trait_ref(db)?.get_type_argument(1)?
            } else {
                block.self_ty(db)
            };
            ty.strip_references().as_adt()
        });
        for item in items {
            let AssocItem::Function(function) = item else {
                continue;
            };
            let (Some(symbol), Some(name)) = (
                self.input.symbols.assoc_item(item),
                self.guard(|| Some(function.name(db))),
            ) else {
                continue;
            };
            let found = ImplFn { ty, name, symbol };
            self.impl_fns.entry(trait_).or_default().push(found);
        }
    }

    /// Adds the `Implements` edges of one trait impl block: from the block to
    /// the trait, and from each item to the trait item it implements.
    fn implements(&mut self, id: SymbolId, trait_: Trait, items: &[AssocItem]) {
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
        for &item in items {
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

    /// Whether a trait belongs to the standard library.
    fn is_std(&self, trait_: Trait) -> bool {
        let db = self.input.db;
        self.guard(|| {
            Some(matches!(
                trait_.module(db).krate(db).origin(db),
                CrateOrigin::Lang(_)
            ))
        })
        .unwrap_or(false)
    }

    /// The standard trait a function is a method of, and the function's own
    /// name: `("ToString", "to_string")`.
    ///
    /// A method of an impl counts as a method of the trait it implements, so
    /// the blanket impls of the standard library are found too.
    fn std_method(&self, function: Function) -> Option<(Name, Name)> {
        let db = self.input.db;
        let trait_ = self.guard(|| match function.as_assoc_item(db)?.container(db) {
            AssocItemContainer::Trait(trait_) => Some(trait_),
            AssocItemContainer::Impl(block) => block.trait_(db),
        })?;
        if !self.is_std(trait_) {
            return None;
        }
        self.guard(|| Some((trait_.name(db), function.name(db))))
    }

    /// Whether the workspace implements one of the implicitly called standard
    /// traits, which is what makes a use of it worth looking at.
    fn implements_std(&self, family: Implicit) -> bool {
        self.implicit.contains_key(&family)
    }

    /// Adds the dispatch edges of one implicit call of a standard trait.
    ///
    /// Nothing in the syntax names the impl that runs: the type of the value
    /// decides it, and a value whose type this pass cannot name reaches every
    /// impl of the trait, as a coarse candidate.
    fn dispatch_use(
        &mut self,
        from: SymbolId,
        site: &Location,
        family: Implicit,
        value: ValueType,
    ) {
        let Some(trait_) = self.implicit.get(&family) else {
            return;
        };
        let methods = family.methods();
        let targets: Vec<SymbolId> = self
            .impl_fns
            .get(trait_)
            .into_iter()
            .flatten()
            .filter(|found| methods.contains(&found.name.as_str()) && value.can_reach(found.ty))
            .map(|found| found.symbol)
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

    /// The same, for an implicit call written at `node`.
    fn dispatch_at(
        &mut self,
        from: SymbolId,
        node: &SyntaxNode,
        family: Implicit,
        value: ValueType,
    ) {
        if self.implements_std(family)
            && let Some(site) = self.site(node)
        {
            self.dispatch_use(from, &site, family, value);
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

    /// Which formatting impls one name of a format string can reach.
    ///
    /// A name in a template is not an expression, so it has no syntax node to
    /// infer: a local's type is read from its binding, a constant's from its
    /// declaration, and a unit struct or variant is its own type.
    fn typed_definition(&mut self, def: Definition<'_>) -> Option<ValueType> {
        let db = self.input.db;
        let ty = match def {
            Definition::Local(local) => self.guard(|| Some(local.ty(db)))?,
            Definition::Const(item) => self.guard(|| Some(item.ty(db)))?,
            Definition::Static(item) => self.guard(|| Some(item.ty(db)))?,
            Definition::Adt(adt) => return Some(ValueType::Data(adt)),
            Definition::EnumVariant(variant) => {
                let parent = self.guard(|| Some(variant.parent_enum(db)))?;
                return Some(ValueType::Data(Adt::Enum(parent)));
            }
            _ => return None,
        };
        Some(self.value_of_type(ty))
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
    ///
    /// A reference is a [`ast::NameRef`] or a [`ast::Path`], which rust-analyzer
    /// classifies the way find-references does; the syntax around it decides
    /// what kind of edge it is. The calls nothing in the syntax names (an
    /// operator, a `?`, a `for` loop, a format string) are added by
    /// [`Collector::implicit`].
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
        if let Some(path) = ast::Path::cast(node.clone()) {
            self.path(&path, from, depth);
            return;
        }
        if let Some(name_ref) = ast::NameRef::cast(node.clone()) {
            self.name_ref(&name_ref, from);
            return;
        }
        if let Some(name) = ast::Name::cast(node.clone()) {
            self.name(&name, from);
            return;
        }
        self.implicit(node, from);
        for child in node.children() {
            self.visit(&child, from, depth, false);
        }
    }

    /// Adds the edges of the calls that one node makes without naming them.
    fn implicit(&mut self, node: &SyntaxNode, from: SymbolId) {
        if let Some(expr) = ast::MatchExpr::cast(node.clone()) {
            self.match_expr(&expr, from);
        } else if let Some(expr) = ast::ForExpr::cast(node.clone()) {
            // A `for` loop calls the iterator's own `next`.
            if let Some(iterable) = expr.iterable()
                && self.implements_std(Implicit::Iterator)
            {
                let iterator = self.typed_value(&iterable);
                self.dispatch_at(from, iterable.syntax(), Implicit::Iterator, iterator);
            }
        } else if let Some(expr) = ast::TryExpr::cast(node.clone()) {
            // A `?` runs `From::from` on the error type of its operand.
            if let Some(operand) = expr.expr()
                && self.implements_std(Implicit::From)
            {
                let error = self.error_type(&operand);
                self.dispatch_at(from, expr.syntax(), Implicit::From, error);
            }
        } else if let Some(expr) = ast::FormatArgsExpr::cast(node.clone()) {
            self.format_args(&expr, from);
        } else if ast::BinExpr::can_cast(node.kind())
            || ast::PrefixExpr::can_cast(node.kind())
            || ast::IndexExpr::can_cast(node.kind())
        {
            self.operator_expr(node, from);
        }
    }

    /// The definition a name reference refers to.
    fn classify(&self, name_ref: &ast::NameRef) -> Option<Definition<'db>> {
        self.guard(
            || match NameRefClass::classify(self.input.sema, name_ref)? {
                NameRefClass::Definition(def, _) => Some(def),
                NameRefClass::FieldShorthand { field_ref, .. } => {
                    Some(Definition::Field(field_ref))
                }
                NameRefClass::ExternCrateShorthand { .. } => None,
            },
        )
    }

    /// The definition a path refers to, which is what its last segment names.
    fn resolve(&self, path: &ast::Path) -> Option<Definition<'db>> {
        match path.segment().and_then(|segment| segment.name_ref()) {
            Some(name_ref) => self.classify(&name_ref),
            None => self
                .guard(|| self.input.sema.resolve_path(path))
                .map(Definition::from),
        }
    }

    /// The symbol of a definition, if it produced one.
    fn symbol(&self, def: Definition<'_>) -> Option<SymbolId> {
        let key = match def {
            Definition::Module(it) => DefKey::Module(it),
            Definition::Function(it) => DefKey::Function(it),
            Definition::Adt(it) => DefKey::Adt(it),
            Definition::EnumVariant(it) => DefKey::Variant(it),
            Definition::Field(it) => DefKey::Field(it),
            Definition::Const(it) => DefKey::Const(it),
            Definition::Static(it) => DefKey::Static(it),
            Definition::Trait(it) => DefKey::Trait(it),
            Definition::TypeAlias(it) => DefKey::TypeAlias(it),
            Definition::Macro(it) => DefKey::Macro(it),
            Definition::SelfType(it) => DefKey::Impl(it),
            _ => return None,
        };
        self.input.symbols.get(key)
    }

    /// Adds the edges of a path, by where it is written.
    ///
    /// Only the whole path is a reference: its qualifier names the scope it
    /// is found in. The generic arguments of a path in an expression or a type
    /// are type positions of their own.
    fn path(&mut self, path: &ast::Path, from: SymbolId, depth: u32) {
        let Some(parent) = path.syntax().parent() else {
            return;
        };
        if let Some(expr) = ast::PathExpr::cast(parent.clone()) {
            self.path_expr(path, &expr, from);
        } else if ast::PathType::can_cast(parent.kind()) {
            match self.resolve(path) {
                Some(
                    def @ (Definition::Adt(_) | Definition::Trait(_) | Definition::TypeAlias(_)),
                ) => {
                    self.edge_to(
                        def,
                        from,
                        EdgeKind::UsesType,
                        path.syntax(),
                        EdgeDetail::None,
                    );
                }
                Some(_) => {}
                None => self.record_unresolved_path(path),
            }
        } else if ast::PathPat::can_cast(parent.kind()) {
            self.pattern_path(path, &parent, from, false);
            return;
        } else if let Some(pat) = ast::TupleStructPat::cast(parent.clone()) {
            let rest = pat
                .fields()
                .any(|field| matches!(field, ast::Pat::RestPat(_)));
            if let Some(def) = self.pattern_path(path, &parent, from, rest) {
                self.tuple_pattern_fields(&pat, def, from);
            }
            return;
        } else if let Some(pat) = ast::RecordPat::cast(parent.clone()) {
            let rest = pat
                .record_pat_field_list()
                .is_some_and(|list| list.rest_pat().is_some());
            self.pattern_path(path, &parent, from, rest);
            return;
        } else if let Some(expr) = ast::RecordExpr::cast(parent.clone()) {
            self.record_expr(path, &expr, from);
        } else {
            return;
        }
        self.type_arguments(path, from, depth);
    }

    /// Adds the edges of a path in value position: a call, a constructor, a
    /// value, or the value of a shorthand field of a literal.
    fn path_expr(&mut self, path: &ast::Path, expr: &ast::PathExpr, from: SymbolId) {
        let parent = expr.syntax().parent();
        if let Some(call) = parent.clone().and_then(ast::CallExpr::cast)
            && call
                .expr()
                .is_some_and(|callee| callee.syntax() == expr.syntax())
        {
            let def = self.resolve(path);
            match def {
                Some(Definition::Function(function)) => self.call_to(function, from, path.syntax()),
                // `Wrapper(1)` and `State::Busy(1)` are constructor calls.
                Some(def @ (Definition::Adt(_) | Definition::EnumVariant(_))) => {
                    self.constructs(def, from, path.syntax(), false);
                }
                Some(_) => {}
                None => self.record_unresolved_path(path),
            }
            if let Some(def) = def {
                self.tuple_literal_fields(def, call.arg_list(), from);
            }
            return;
        }
        let mut def = self.resolve(path);
        // `Point { x }` names the field `x` and takes the value `x`.
        if let Some(field) = parent.and_then(ast::RecordExprField::cast)
            && field.name_ref().is_none()
            && let Some(Definition::Field(target)) = def
        {
            self.literal_field(target, field.syntax(), from);
            def = self
                .guard(|| self.input.sema.resolve_path(path))
                .map(Definition::from);
        }
        let Some(def) = def else {
            self.record_unresolved_path(path);
            return;
        };
        if let Some(kind) = value_kind(def) {
            self.edge_to(def, from, kind, path.syntax(), EdgeDetail::None);
        }
        // `map(ToString::to_string)` hands the formatting trait to a caller
        // instead of calling it, so the value it formats is not named where it
        // is formatted.
        if let Definition::Function(function) = def
            && is_to_string(self.std_method(function))
        {
            let value = ValueType::Unknown;
            self.dispatch_at(from, path.syntax(), Implicit::Display, value);
        }
    }

    /// Adds the edges of a name reference that is not a path: a method, a
    /// field, or the field of a literal or a pattern.
    fn name_ref(&mut self, name_ref: &ast::NameRef, from: SymbolId) {
        let Some(parent) = name_ref.syntax().parent() else {
            return;
        };
        if let Some(call) = ast::MethodCallExpr::cast(parent.clone()) {
            match self.classify(name_ref) {
                Some(Definition::Function(function)) => {
                    self.method_call(function, &call, name_ref, from);
                }
                Some(def @ Definition::Field(_)) => {
                    let kind = EdgeKind::ReadsField;
                    self.edge_to(def, from, kind, name_ref.syntax(), EdgeDetail::None);
                }
                Some(_) => {}
                None => self.record_unresolved_name(name_ref),
            }
        } else if ast::FieldExpr::can_cast(parent.kind()) {
            match self.classify(name_ref) {
                Some(def @ Definition::Field(_)) => {
                    let kind = if is_write_target(&parent) {
                        EdgeKind::WritesField
                    } else {
                        EdgeKind::ReadsField
                    };
                    self.edge_to(def, from, kind, name_ref.syntax(), EdgeDetail::None);
                }
                // A field of a plain tuple has no symbol.
                Some(Definition::TupleField(_)) => {}
                _ => {
                    if name_ref.as_tuple_field().is_none() {
                        self.record_unresolved_name(name_ref);
                    }
                }
            }
        } else if ast::RecordExprField::can_cast(parent.kind()) {
            if let Some(Definition::Field(field)) = self.classify(name_ref) {
                self.literal_field(field, name_ref.syntax(), from);
            }
        } else if ast::RecordPatField::can_cast(parent.kind())
            && let Some(Definition::Field(field)) = self.classify(name_ref)
        {
            self.pattern_field(field, &parent, from);
        }
    }

    /// Adds the edge of a name a pattern binds: the field of a shorthand
    /// (`Point { x }`), or the constant or unit variant a bare name matches
    /// (`MAX`, or `Idle` after `use State::*`).
    fn name(&mut self, name: &ast::Name, from: SymbolId) {
        let Some(pat) = name.syntax().parent().and_then(ast::IdentPat::cast) else {
            return;
        };
        match self.guard(|| NameClass::classify(self.input.sema, name)) {
            Some(NameClass::PatFieldShorthand { field_ref, .. }) => {
                if let Some(field) = pat.syntax().parent() {
                    self.pattern_field(field_ref, &field, from);
                }
            }
            Some(NameClass::ConstReference(
                def @ (Definition::Adt(_) | Definition::EnumVariant(_) | Definition::Const(_)),
            )) => {
                let detail = self.match_detail(pat.syntax());
                self.edge_to(def, from, EdgeKind::Matches, pat.syntax(), detail);
            }
            _ => {}
        }
    }

    /// Adds the edges of a method call to `function`.
    fn method_call(
        &mut self,
        function: Function,
        call: &ast::MethodCallExpr,
        name_ref: &ast::NameRef,
        from: SymbolId,
    ) {
        self.call_to(function, from, name_ref.syntax());
        let called = self.std_method(function);
        let receiver = call.receiver();
        // `ToString::to_string` calls the `Display::fmt` of the receiver.
        if let Some(receiver) = &receiver
            && is_to_string(called.clone())
            && self.implements_std(Implicit::Display)
        {
            let value = self.typed_value(receiver);
            self.dispatch_at(from, receiver.syntax(), Implicit::Display, value);
        }
        let Some((trait_, method)) = called else {
            return;
        };
        if trait_.as_str() != "Iterator" {
            return;
        }
        // An adaptor call on an iterator runs the iterator's own `next` from
        // inside the standard library.
        if let Some(receiver) = &receiver
            && self.implements_std(Implicit::Iterator)
        {
            let iterator = self.typed_value(receiver);
            self.dispatch_at(from, name_ref.syntax(), Implicit::Iterator, iterator);
        }
        // `collect` builds the collection the type asks for through
        // `FromIterator`, which nothing names.
        if method.as_str() == "collect" && self.implements_std(Implicit::FromIterator) {
            let collection = self.typed_value(&ast::Expr::MethodCallExpr(call.clone()));
            self.dispatch_at(from, name_ref.syntax(), Implicit::FromIterator, collection);
        }
    }

    /// Adds the `Constructs` edge of a struct or variant literal.
    ///
    /// `Self { .. }` and a type alias name the struct they stand for. A spread
    /// leaves the other fields to the value it copies them from.
    fn record_expr(&mut self, path: &ast::Path, expr: &ast::RecordExpr, from: SymbolId) {
        let spread = expr
            .record_expr_field_list()
            .is_some_and(|list| list.spread().is_some());
        let db = self.input.db;
        let aliased = |adt: Adt| (!matches!(adt, Adt::Enum(_))).then_some(Definition::Adt(adt));
        let target = match self.resolve(path) {
            Some(Definition::SelfType(block)) => {
                self.guard(|| block.self_ty(db).as_adt()).and_then(aliased)
            }
            Some(Definition::TypeAlias(alias)) => {
                self.guard(|| alias.ty(db).as_adt()).and_then(aliased)
            }
            other => other,
        };
        match target {
            // An enum is not a literal: only its path is located.
            Some(def @ Definition::Adt(Adt::Enum(_))) => {
                self.constructs(def, from, path.syntax(), spread);
            }
            Some(def @ (Definition::Adt(_) | Definition::EnumVariant(_))) => {
                self.constructs(def, from, expr.syntax(), spread);
            }
            _ => {}
        }
    }

    /// Adds the `WritesField` edge of a field named in a literal.
    ///
    /// A field named in a literal is not read: the literal itself is what
    /// constructs the value. It does name the field, so a removed or retyped
    /// field has to reach this site, and a field the literal leaves to a
    /// `..base` does not.
    fn literal_field(&mut self, field: Field, node: &SyntaxNode, from: SymbolId) {
        let def = Definition::Field(field);
        self.edge_to(def, from, EdgeKind::WritesField, node, EdgeDetail::None);
    }

    /// Adds the `Matches` edge of a field named in a record pattern.
    fn pattern_field(&mut self, field: Field, node: &SyntaxNode, from: SymbolId) {
        let detail = self.match_detail(node);
        let def = Definition::Field(field);
        self.edge_to(def, from, EdgeKind::Matches, node, detail);
    }

    /// Adds the `Matches` edge of an enum `match` on its scrutinee.
    fn match_expr(&mut self, expr: &ast::MatchExpr, from: SymbolId) {
        let Some(scrutinee) = expr.expr() else {
            return;
        };
        let scrutinee_ty = self
            .guard(|| self.input.sema.type_of_expr(&scrutinee))
            .map(|info| info.original.strip_references());
        if let Some(adt @ Adt::Enum(_)) = scrutinee_ty.and_then(|ty| ty.as_adt()) {
            let detail = self.match_detail_of(expr);
            let def = Definition::Adt(adt);
            self.edge_to(def, from, EdgeKind::Matches, scrutinee.syntax(), detail);
        }
    }

    /// Adds the `Matches` edge of a pattern that names a struct, variant or
    /// constant, and returns what the pattern names.
    ///
    /// `rest` says that the pattern ends with `..`, which is recorded on the edge
    /// to a struct or a variant: the pattern then matches whatever fields the
    /// struct or variant grows, and leaves the fields it already has to be
    /// checked individually.
    fn pattern_path(
        &mut self,
        path: &ast::Path,
        node: &SyntaxNode,
        from: SymbolId,
        rest: bool,
    ) -> Option<Definition<'db>> {
        let Some(def) = self.resolve(path) else {
            self.record_unresolved_path(path);
            return None;
        };
        let detail = match def {
            Definition::Adt(Adt::Struct(_)) | Definition::EnumVariant(_) if rest => {
                EdgeDetail::WithRest
            }
            Definition::Adt(_) | Definition::EnumVariant(_) | Definition::Const(_) => {
                self.match_detail(node)
            }
            _ => return Some(def),
        };
        self.edge_to(def, from, EdgeKind::Matches, node, detail);
        Some(def)
    }

    /// Visits the template of a format string.
    ///
    /// A captured argument (`format!("{NAME}")`) is only a name inside the
    /// literal, so it has no syntax node of its own; the format machinery
    /// resolves the names of the literal, and each of them is a value reference
    /// like a path would be.
    fn format_args(&mut self, expr: &ast::FormatArgsExpr, from: SymbolId) {
        let template = expr
            .syntax()
            .descendants_with_tokens()
            .filter_map(|child| child.into_token())
            .find_map(ast::String::cast);
        let traits = format_traits(&template);
        if let Some(string) = template
            && let Some(file) = string
                .syntax()
                .parent()
                .and_then(|node| self.guard(|| Some(self.input.sema.hir_file_for(&node))))
            && let Some(parts) = self.guard(|| self.input.sema.as_format_args_parts(&string))
        {
            for (range, resolution) in parts {
                let Some(def) = resolution
                    .and_then(|part| part.left())
                    .map(Definition::from)
                else {
                    continue;
                };
                let Some(site) = self.guard(|| self.input.locate.location(file, range)) else {
                    continue;
                };
                // A part that names an item is a value reference.
                if let Some(kind) = value_kind(def)
                    && let Some(id) = self.symbol(def)
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
                if traits.iter().any(|family| self.implements_std(*family))
                    && let Some(value) = self.typed_definition(def)
                {
                    for &family in &traits {
                        self.dispatch_use(from, &site, family, value);
                    }
                }
            }
        }
        // The values the template prints are handed to `fmt` of the formatting
        // traits it names, which is a dispatch no reference in the syntax
        // resolves.
        if traits.iter().any(|family| self.implements_std(*family)) {
            for argument in expr
                .syntax()
                .children()
                .filter_map(ast::FormatArgsArg::cast)
            {
                let Some(argument) = argument.expr() else {
                    continue;
                };
                let value = self.typed_value(&argument);
                for &family in &traits {
                    self.dispatch_at(from, argument.syntax(), family, value);
                }
            }
        }
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

    /// The fields of a struct, union or variant, in declaration order.
    fn fields_of(&self, def: Definition<'_>) -> Option<Vec<Field>> {
        let db = self.input.db;
        self.guard(|| match def {
            Definition::Adt(Adt::Struct(it)) => Some(it.fields(db)),
            Definition::Adt(Adt::Union(it)) => Some(it.fields(db)),
            Definition::EnumVariant(it) => Some(it.fields(db)),
            _ => None,
        })
    }

    /// Adds a `WritesField` edge to every positional field a tuple constructor
    /// literal supplies.
    ///
    /// `Wrapper(1)` names its field by position rather than by name; the field
    /// it supplies is the one at that position.
    fn tuple_literal_fields(
        &mut self,
        def: Definition<'_>,
        args: Option<ast::ArgList>,
        from: SymbolId,
    ) {
        let (Some(args), Some(fields)) = (args, self.fields_of(def)) else {
            return;
        };
        for (arg, field) in args.args().zip(fields) {
            self.literal_field(field, arg.syntax(), from);
        }
    }

    /// Adds a `Matches` edge to every positional field a tuple pattern names.
    ///
    /// A pattern with a `..` does not line its fields up with the declaration
    /// positions, so only a pattern that names every field gets one.
    fn tuple_pattern_fields(
        &mut self,
        pat: &ast::TupleStructPat,
        def: Definition<'_>,
        from: SymbolId,
    ) {
        let Some(fields) = self.fields_of(def) else {
            return;
        };
        let inner: Vec<ast::Pat> = pat.fields().collect();
        if inner.len() != fields.len() {
            return;
        }
        for (sub, field) in inner.iter().zip(fields) {
            self.pattern_field(field, sub.syntax(), from);
        }
    }

    /// Adds a `Constructs` edge to a struct, union or variant.
    ///
    /// `spread` says that the literal ends with `..base`, which is recorded on
    /// the edge to a struct or a variant: only those can be given from another
    /// value of their type.
    fn constructs(&mut self, def: Definition<'_>, from: SymbolId, node: &SyntaxNode, spread: bool) {
        let Some(id) = self.symbol(def) else {
            return;
        };
        let kind = self.input.graph.symbols[id.0 as usize].path.kind;
        let detail = if spread && matches!(kind, SymbolKind::Struct | SymbolKind::Variant) {
            EdgeDetail::WithRest
        } else {
            EdgeDetail::None
        };
        self.located_edge(
            from,
            id,
            EdgeKind::Constructs,
            Certainty::Resolved,
            node,
            detail,
        );
    }

    /// Adds a resolved edge to the symbol of `def`, if it has one.
    fn edge_to(
        &mut self,
        def: Definition<'_>,
        from: SymbolId,
        kind: EdgeKind,
        node: &SyntaxNode,
        detail: EdgeDetail,
    ) {
        if let Some(id) = self.symbol(def) {
            self.located_edge(from, id, kind, Certainty::Resolved, node, detail);
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
        let ranges = self.inactive_in(&site);
        // The ranges that start inside the call, of which one must end inside it.
        let first = ranges.partition_point(|range| range.start < site.span.start);
        ranges[first..]
            .iter()
            .take_while(|range| range.start <= site.span.end)
            .any(|range| range.end <= site.span.end)
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

    /// Adds the edge of a call to `function`.
    ///
    /// A call that resolves to a trait method declaration dispatches through the
    /// receiver, so every implementation of the trait in the workspace may be
    /// reached as well.
    fn call_to(&mut self, function: Function, from: SymbolId, node: &SyntaxNode) {
        let Some(id) = self.input.symbols.get(DefKey::Function(function)) else {
            return;
        };
        let kind = EdgeKind::Calls;
        self.located_edge(from, id, kind, Certainty::Resolved, node, EdgeDetail::None);
        let db = self.input.db;
        if let Some(AssocItemContainer::Trait(trait_)) =
            self.guard(|| Some(function.as_assoc_item(db)?.container(db)))
        {
            self.dispatch_candidates(trait_, function, from, node);
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
        let Some(name) = self.guard(|| Some(method.name(self.input.db))) else {
            return;
        };
        let targets: Vec<SymbolId> = self
            .impl_fns
            .get(&trait_)
            .into_iter()
            .flatten()
            .filter(|found| found.name == name)
            .map(|found| found.symbol)
            .collect();
        for id in targets {
            let certainty = Certainty::Candidate;
            let kind = EdgeKind::MayDispatchTo;
            self.located_edge(from, id, kind, certainty, node, EdgeDetail::None);
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
        let ranges = self.inactive_in(location);
        // The ranges that start before the location, of which one must cover it.
        let end = ranges.partition_point(|range| range.start <= location.span.start);
        ranges[..end]
            .iter()
            .any(|range| location.span.end <= range.end)
    }

    /// The inactive ranges of the file of `location`, sorted by their start.
    fn inactive_in(&self, location: &Location) -> &[Span] {
        self.inactive.get(&location.file).map_or(&[], Vec::as_slice)
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
    /// extraction down, and every caller can do without the answer. It is
    /// counted, so that the graph can say that something is missing.
    fn guard<T>(&self, query: impl FnOnce() -> Option<T>) -> Option<T> {
        catch_unwind(AssertUnwindSafe(query)).unwrap_or_else(|_| {
            self.failed.set(self.failed.get() + 1);
            None
        })
    }
}

/// The inactive ranges by file, each list sorted by start.
fn inactive_by_file(ranges: &[Location]) -> HashMap<RelPath, Vec<Span>> {
    let mut by_file: HashMap<RelPath, Vec<Span>> = HashMap::new();
    for range in ranges {
        by_file
            .entry(range.file.clone())
            .or_default()
            .push(range.span);
    }
    for spans in by_file.values_mut() {
        spans.sort_by_key(|span| (span.start, span.end));
    }
    by_file
}

/// The edge a path in value position produces for the definition it resolves
/// to, if it is a value at all.
fn value_kind(def: Definition<'_>) -> Option<EdgeKind> {
    match def {
        Definition::Function(_) => Some(EdgeKind::TakesAsValue),
        Definition::Const(_) | Definition::Static(_) => Some(EdgeKind::UsesValue),
        // A unit struct or variant built by naming it.
        Definition::EnumVariant(_) | Definition::Adt(_) => Some(EdgeKind::Constructs),
        _ => None,
    }
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

/// Whether a function is `ToString::to_string`, which calls the `Display`
/// implementation of whatever it is given, given its [`Collector::std_method`].
fn is_to_string(called: Option<(Name, Name)>) -> bool {
    called.is_some_and(|(trait_, method)| {
        trait_.as_str() == "ToString" && method.as_str() == "to_string"
    })
}

/// Which formatting traits a format string invokes.
///
/// `{}` prints its value with `Display` and `{:?}` (and `{:#?}`) with `Debug`,
/// so the placeholders of the template decide what a site may call. A template
/// that prints both is reported as both; one without a placeholder prints
/// nothing and reaches nothing.
fn format_traits(template: &Option<ast::String>) -> Vec<Implicit> {
    let Some(string) = template else {
        return Vec::new();
    };
    let text = string.syntax().text().to_string();
    let mut traits: Vec<Implicit> = Vec::new();
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
                    Implicit::Debug
                } else {
                    Implicit::Display
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
