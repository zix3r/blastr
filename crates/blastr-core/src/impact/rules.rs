//! The impact rules: which dependents a change reaches, with what class,
//! certainty and reason.
//!
//! [`first_steps`] matches a change on its kind and detail and follows the
//! incoming edges that case is about; [`onward`] is the behaviour step the
//! search repeats from every item it has reached.
//!
//! A formatting-only change, a widened visibility, an added item, a reshaped
//! variant, a trait item that gained a body and an attribute change on anything
//! but a function reach nothing by themselves.

use std::collections::HashSet;

use blastr_model::{
    Certainty, Change, ChangeDetail, ChangeKind, Delta, Edge, EdgeDetail, EdgeKind, GraphIndex,
    ImpactClass, Reason, RevisionGraph, SymbolFlags, SymbolId, SymbolKind, SymbolPath, Visibility,
};

/// Which side of the comparison a step reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Side {
    /// The graph of the older side.
    Base,
    /// The graph of the newer side.
    Head,
}

/// The two graphs of a comparison and their lookup tables.
pub(crate) struct Graphs<'a> {
    /// The graph of the older side.
    pub(crate) base: &'a RevisionGraph,
    /// The graph of the newer side.
    pub(crate) head: &'a RevisionGraph,
    /// Lookup tables of the base graph.
    pub(crate) base_index: GraphIndex,
    /// Lookup tables of the head graph.
    pub(crate) head_index: GraphIndex,
}

impl<'a> Graphs<'a> {
    /// Builds the lookup tables of both sides.
    pub(crate) fn new(base: &'a RevisionGraph, head: &'a RevisionGraph) -> Graphs<'a> {
        Graphs {
            base,
            head,
            base_index: GraphIndex::new(base),
            head_index: GraphIndex::new(head),
        }
    }

    /// The graph of one side.
    pub(crate) fn graph(&self, side: Side) -> &'a RevisionGraph {
        match side {
            Side::Base => self.base,
            Side::Head => self.head,
        }
    }

    /// The lookup tables of one side.
    pub(crate) fn index(&self, side: Side) -> &GraphIndex {
        match side {
            Side::Base => &self.base_index,
            Side::Head => &self.head_index,
        }
    }
}

/// One dependency step: the edge it followed and what the reached item means.
/// The reached symbol is the edge's `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reach {
    /// The graph the edge index belongs to.
    pub(crate) side: Side,
    /// Index into that side's `edges`.
    pub(crate) edge: u32,
    /// The class the reached item gets from this step.
    pub(crate) class: ImpactClass,
    /// How certain the step is.
    pub(crate) certainty: Certainty,
    /// Why the step applies.
    pub(crate) reason: Reason,
    /// A step between the changed symbol and [`Reach::edge`], when the item
    /// hangs off the change two steps away.
    pub(crate) via: Option<Via>,
    /// Whether the rule cannot tell exactly who is affected.
    pub(crate) coarse: bool,
}

/// The first of two steps: kept so that the evidence path can show how the
/// item hangs off the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Via {
    /// Index of the first step's edge on the reach's side.
    pub(crate) edge: u32,
    /// How certain that step is.
    pub(crate) certainty: Certainty,
}

/// Every edge kind that is a reference to the target: all but `Contains`.
const REFERENCES: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::TakesAsValue,
    EdgeKind::UsesType,
    EdgeKind::ReadsField,
    EdgeKind::WritesField,
    EdgeKind::Constructs,
    EdgeKind::Matches,
    EdgeKind::Implements,
    EdgeKind::MayDispatchTo,
    EdgeKind::UsesValue,
    EdgeKind::Expands,
];
/// Functions and methods.
const FUNCTION_OR_METHOD: &[SymbolKind] = &[SymbolKind::Function, SymbolKind::Method];
/// The kinds a signature change is reported for as a declaration change.
const DECLARATIONS: &[SymbolKind] = &[
    SymbolKind::Struct,
    SymbolKind::Enum,
    SymbolKind::Union,
    SymbolKind::Trait,
    SymbolKind::Impl,
];
/// The containers a field can belong to.
const STRUCT_OR_VARIANT: &[SymbolKind] = &[SymbolKind::Struct, SymbolKind::Variant];

/// What the steps of one case give the items they reach.
#[derive(Debug, Clone, Copy)]
struct Step {
    /// The class of the reached item.
    class: ImpactClass,
    /// Why the step applies.
    reason: Reason,
    /// Whether the case cannot tell exactly who is affected.
    coarse: bool,
}

/// A step whose item has to be edited.
fn adapt(reason: Reason) -> Step {
    Step {
        class: ImpactClass::Adaptation,
        reason,
        coarse: false,
    }
}

/// A step whose item may behave differently.
fn behave(reason: Reason) -> Step {
    Step {
        class: ImpactClass::Behaviour,
        reason,
        coarse: false,
    }
}

/// The edge's own certainty.
fn as_resolved(edge: &Edge) -> Option<Certainty> {
    Some(edge.certainty)
}

/// Always a candidate: the step itself is uncertain whatever the edge is.
fn as_candidate(_: &Edge) -> Option<Certainty> {
    Some(Certainty::Candidate)
}

/// The edge's own certainty, for an edge that does not end with `..`.
fn without_rest(edge: &Edge) -> Option<Certainty> {
    (edge.detail != EdgeDetail::WithRest).then_some(edge.certainty)
}

/// The first steps a change takes: the dependencies that arrive at what the
/// change is about, ordered by side and edge index.
pub(crate) fn first_steps(change: &Change, graphs: &Graphs<'_>) -> Vec<Reach> {
    use EdgeKind as E;
    let head = Scope::new(graphs, Side::Head, change);
    let base = Scope::new(graphs, Side::Base, change);
    let function = FUNCTION_OR_METHOD.contains(&change.symbol.kind);
    let mut reaches = Vec::new();
    let out = &mut reaches;
    match (change.kind, &change.detail) {
        (ChangeKind::Body, _) => {
            let changed = head.symbol;
            let calls = behave(Reason::CallsChanged);
            head.follow(out, changed, &[E::Calls], calls, as_resolved);
            let values = behave(Reason::TakesChangedAsValue);
            head.follow(out, changed, &[E::TakesAsValue], values, as_candidate);
            let dispatch = behave(Reason::DispatchesToChanged);
            head.follow(out, changed, &[E::MayDispatchTo], dispatch, as_candidate);
            // A procedural macro's function: a seed is never expanded by
            // `onward`, so its expansions need a first step of their own. The
            // reference is resolved, but a body change does not prove that
            // the expansion changed, so the step is a candidate.
            if function {
                let expands = behave(Reason::InvokesChangedMacro);
                head.follow(out, changed, &[E::Expands], expands, as_candidate);
            }
        }
        (ChangeKind::Signature, _) if function => {
            // A method that implements a trait item is called through the
            // trait, whose declaration did not change, so its callers compile.
            if !head.flag(|flags| flags.in_trait_impl) {
                let step = adapt(Reason::SignatureChanged);
                let kinds = &[E::Calls, E::TakesAsValue];
                head.follow(out, head.symbol, kinds, step, as_resolved);
            }
            let dispatch = behave(Reason::DispatchesToChanged);
            head.follow(
                out,
                head.symbol,
                &[E::MayDispatchTo],
                dispatch,
                as_candidate,
            );
        }
        (ChangeKind::Signature, _) if DECLARATIONS.contains(&change.symbol.kind) => {
            let step = Step {
                coarse: true,
                ..adapt(Reason::DeclarationChanged)
            };
            let kinds = &[E::UsesType, E::Constructs, E::Matches, E::Implements];
            head.follow(out, head.symbol, kinds, step, as_candidate);
        }
        (ChangeKind::Visibility, &ChangeDetail::Visibility { from, to }) if to < from => {
            let step = Step {
                coarse: to == Visibility::Restricted,
                ..adapt(Reason::NoLongerVisible)
            };
            let outside = |edge: &Edge| head.outside_visibility(to, edge.from);
            head.follow(out, head.symbol, REFERENCES, step, |edge| {
                outside(edge).then_some(edge.certainty)
            });
            // A field is also named by the literals of its container, which
            // carry no edge to the field itself; one that ends with `..` may
            // or may not name it.
            let container = head.container(STRUCT_OR_VARIANT);
            head.follow(out, container, &[E::Constructs], step, |edge| {
                outside(edge).then_some(match edge.detail {
                    EdgeDetail::WithRest => Certainty::Candidate,
                    _ => edge.certainty,
                })
            });
        }
        (ChangeKind::Removed, _) => {
            let step = adapt(Reason::RefersToRemoved);
            base.follow(out, base.descendants(), REFERENCES, step, as_resolved);
        }
        (ChangeKind::Renamed, ChangeDetail::Renamed { from }) => {
            let previous = graphs.base_index.by_path(from);
            let step = adapt(Reason::RefersToRenamed);
            base.follow(out, previous, REFERENCES, step, as_resolved);
        }
        (ChangeKind::Field, ChangeDetail::Member(Delta::Removed)) => {
            let step = adapt(Reason::UsesChangedField);
            let kinds = &[E::ReadsField, E::WritesField, E::Matches];
            base.follow(out, base.symbol, kinds, step, as_resolved);
            // The field was the only use of a generic parameter of its
            // container, which then has to drop it as well (`E0392`).
            if base.flag(|flags| flags.sole_parameter_use) {
                let step = adapt(Reason::ParameterBecomesUnused);
                base.follow(out, base.symbol, &[E::Contains], step, as_resolved);
            }
        }
        (ChangeKind::Field, ChangeDetail::Member(Delta::Modified)) => {
            let step = adapt(Reason::UsesChangedField);
            let kinds = &[E::ReadsField, E::WritesField, E::Matches];
            head.follow(out, head.symbol, kinds, step, as_resolved);
        }
        (ChangeKind::Field, ChangeDetail::Member(Delta::Added)) => {
            let container = head.container(STRUCT_OR_VARIANT);
            let literals = adapt(Reason::ConstructsWithoutField);
            head.follow(out, container, &[E::Constructs], literals, without_rest);
            let patterns = adapt(Reason::PatternMissesField);
            head.follow(out, container, &[E::Matches], patterns, without_rest);
        }
        (ChangeKind::Variant, ChangeDetail::Member(Delta::Added)) => {
            let container = head.container(&[SymbolKind::Enum]);
            let detail = |wanted: EdgeDetail| {
                move |edge: &Edge| (edge.detail == wanted).then_some(edge.certainty)
            };
            let exhaustive = adapt(Reason::ExhaustiveMatchMissesVariant);
            let matches = &[E::Matches];
            head.follow(
                out,
                container,
                matches,
                exhaustive,
                detail(EdgeDetail::MatchExhaustive),
            );
            let wildcard = behave(Reason::WildcardCatchesNewVariant);
            head.follow(
                out,
                container,
                matches,
                wildcard,
                detail(EdgeDetail::MatchWildcard),
            );
        }
        (ChangeKind::Variant, ChangeDetail::Member(Delta::Removed)) => {
            let step = adapt(Reason::UsesChangedVariant);
            base.follow(out, base.descendants(), REFERENCES, step, as_resolved);
        }
        (ChangeKind::TraitItem, ChangeDetail::Member(Delta::Added)) => {
            // Only a required item has to be implemented.
            if head.flag(|flags| !flags.has_body) {
                let container = head.container(&[SymbolKind::Trait]);
                let step = adapt(Reason::MustImplementTraitItem);
                head.follow(out, container, &[E::Implements], step, |edge| {
                    (head.kind_of(edge.from) == Some(SymbolKind::Impl)).then_some(edge.certainty)
                });
            }
        }
        (ChangeKind::TraitItem, ChangeDetail::Member(Delta::Modified)) => {
            let uses = adapt(Reason::UsesChangedTraitItem);
            let start = out.len();
            let implements = adapt(Reason::MustImplementTraitItem);
            head.follow(out, head.symbol, &[E::Implements], implements, as_resolved);
            // The implementations have to be edited to match, so a call that
            // names one of them directly sees the new signature too: that
            // caller is two steps from the change.
            let implementations = out[start..].to_vec();
            for implementation in implementations {
                let Some(edge) = head.graph.edges.get(implementation.edge as usize) else {
                    continue;
                };
                let chained = out.len();
                let kinds = &[E::Calls, E::TakesAsValue];
                head.follow(out, Some(edge.from), kinds, uses, as_resolved);
                for reach in &mut out[chained..] {
                    reach.via = Some(Via {
                        edge: implementation.edge,
                        certainty: implementation.certainty,
                    });
                }
            }
            let kinds = &[E::Calls, E::TakesAsValue];
            head.follow(out, head.symbol, kinds, uses, as_resolved);
        }
        (ChangeKind::TraitItem, ChangeDetail::Member(Delta::Removed)) => {
            let step = adapt(Reason::UsesChangedTraitItem);
            base.follow(out, base.symbol, REFERENCES, step, as_resolved);
        }
        (ChangeKind::TraitItem, ChangeDetail::DefaultBody) => {
            let container = head.container(&[SymbolKind::Trait]);
            let inherits = behave(Reason::InheritsChangedDefault);
            head.follow(out, container, &[E::Implements], inherits, |edge| {
                head.keeps_default(edge.from).then_some(edge.certainty)
            });
            // A call is less sure once an implementation replaces the default.
            let overridden = head.is_overridden(container);
            let calls = behave(Reason::CallsChanged);
            head.follow(out, head.symbol, &[E::Calls], calls, |edge| {
                Some(if overridden {
                    Certainty::Candidate
                } else {
                    edge.certainty
                })
            });
            let values = behave(Reason::TakesChangedAsValue);
            head.follow(out, head.symbol, &[E::TakesAsValue], values, as_candidate);
        }
        (ChangeKind::Impl, ChangeDetail::Member(delta @ (Delta::Added | Delta::Removed))) => {
            // The resolution of every reference to the block's items changed.
            let side = if *delta == Delta::Added { &head } else { &base };
            let step = behave(Reason::ImplResolutionChanged);
            let kinds = &[E::Calls, E::TakesAsValue, E::MayDispatchTo];
            side.follow(out, side.children(), kinds, step, as_resolved);
        }
        (ChangeKind::ConstValue, ChangeDetail::None) => {
            let step = behave(Reason::UsesChangedValue);
            head.follow(out, head.symbol, &[E::UsesValue], step, as_resolved);
        }
        (ChangeKind::ConstValue, ChangeDetail::Retyped) => {
            let step = adapt(Reason::UsesRetypedValue);
            head.follow(out, head.symbol, &[E::UsesValue], step, as_resolved);
        }
        (ChangeKind::TypeAlias, _) => {
            let step = Step {
                coarse: true,
                ..adapt(Reason::UsesChangedAlias)
            };
            head.follow(out, head.symbol, &[E::UsesType], step, as_candidate);
        }
        (ChangeKind::MacroDef, _) => {
            let step = behave(Reason::InvokesChangedMacro);
            head.follow(out, head.symbol, &[E::Expands], step, as_candidate);
        }
        (ChangeKind::Attribute, _) if function => {
            let step = behave(Reason::AttributeChanged);
            let kinds = &[E::Calls, E::TakesAsValue, E::MayDispatchTo];
            head.follow(out, head.symbol, kinds, step, as_candidate);
        }
        _ => {}
    }
    reaches.sort_by_key(|reach| (reach.side, reach.edge));
    reaches.dedup_by(|left, right| left.side == right.side && left.edge == right.edge);
    reaches
}

/// The behaviour-propagation step: the references that reach `item` on the head
/// graph.
///
/// A call or a value read is certain; passing the item around as a value,
/// dispatching to it through a trait and expanding a macro it implements are
/// not. The `Expands` edge carries a change anywhere in a procedural macro's
/// implementation on to the items whose expansion names it.
pub(crate) fn onward(item: SymbolId, graphs: &Graphs<'_>) -> Vec<Reach> {
    let graph = graphs.head;
    let mut reaches = Vec::new();
    for &edge in graphs.head_index.edges_to(item) {
        let Some(reference) = graph.edges.get(edge as usize) else {
            continue;
        };
        let (certainty, reason) = match reference.kind {
            EdgeKind::Calls => (reference.certainty, Reason::CallsChanged),
            EdgeKind::TakesAsValue => (Certainty::Candidate, Reason::TakesChangedAsValue),
            EdgeKind::MayDispatchTo => (Certainty::Candidate, Reason::DispatchesToChanged),
            EdgeKind::UsesValue => (reference.certainty, Reason::UsesChangedValue),
            EdgeKind::Expands => (Certainty::Candidate, Reason::InvokesChangedMacro),
            _ => continue,
        };
        if is_self_or_descendant(graph, reference.from, item) {
            continue;
        }
        reaches.push(Reach {
            side: Side::Head,
            edge,
            class: ImpactClass::Behaviour,
            certainty,
            reason,
            via: None,
            coarse: false,
        });
    }
    reaches
}

/// One change looked up on one side.
struct Scope<'a> {
    /// The side read.
    side: Side,
    /// The graph of that side.
    graph: &'a RevisionGraph,
    /// Its lookup tables.
    index: &'a GraphIndex,
    /// The graph of the newer side, which decides visibility.
    head: &'a RevisionGraph,
    /// The change.
    change: &'a Change,
    /// The changed symbol on this side.
    symbol: Option<SymbolId>,
}

impl<'a> Scope<'a> {
    /// Looks the changed symbol up on `side`.
    fn new(graphs: &'a Graphs<'a>, side: Side, change: &'a Change) -> Scope<'a> {
        let index = graphs.index(side);
        Scope {
            side,
            graph: graphs.graph(side),
            index,
            head: graphs.head,
            change,
            symbol: index.by_path(&change.symbol),
        }
    }

    /// Adds a reach for every edge of one of `kinds` that arrives at one of
    /// `targets` from outside it and gets a certainty from `judge`.
    fn follow(
        &self,
        out: &mut Vec<Reach>,
        targets: impl IntoIterator<Item = SymbolId>,
        kinds: &[EdgeKind],
        step: Step,
        judge: impl Fn(&Edge) -> Option<Certainty>,
    ) {
        for target in targets {
            for &edge in self.index.edges_to(target) {
                let Some(reference) = self.graph.edges.get(edge as usize) else {
                    continue;
                };
                if !kinds.contains(&reference.kind)
                    || is_self_or_descendant(self.graph, reference.from, target)
                {
                    continue;
                }
                let Some(certainty) = judge(reference) else {
                    continue;
                };
                out.push(Reach {
                    side: self.side,
                    edge,
                    class: step.class,
                    certainty,
                    reason: step.reason,
                    via: None,
                    coarse: step.coarse,
                });
            }
        }
    }

    /// Whether the changed symbol exists on this side with `test` true of its
    /// flags.
    fn flag(&self, test: impl Fn(&SymbolFlags) -> bool) -> bool {
        self.symbol
            .and_then(|id| self.graph.symbols.get(id.0 as usize))
            .is_some_and(|symbol| test(&symbol.flags))
    }

    /// The kind of a symbol on this side.
    fn kind_of(&self, id: SymbolId) -> Option<SymbolKind> {
        self.graph
            .symbols
            .get(id.0 as usize)
            .map(|symbol| symbol.path.kind)
    }

    /// The changed symbol and everything inside it, the symbol first.
    fn descendants(&self) -> Vec<SymbolId> {
        let mut out: Vec<SymbolId> = self.symbol.into_iter().collect();
        let mut seen: HashSet<SymbolId> = out.iter().copied().collect();
        let mut cursor = 0;
        while let Some(&current) = out.get(cursor) {
            for &child in self.index.children(current) {
                if seen.insert(child) {
                    out.push(child);
                }
            }
            cursor += 1;
        }
        out
    }

    /// The direct children of the changed symbol.
    fn children(&self) -> Vec<SymbolId> {
        self.symbol
            .map(|symbol| self.index.children(symbol).to_vec())
            .unwrap_or_default()
    }

    /// The container of the changed symbol when it has one of `kinds`: its
    /// parent, or the symbol its path names without the last segment when
    /// this side does not hold the changed symbol.
    fn container(&self, kinds: &[SymbolKind]) -> Option<SymbolId> {
        if let Some(id) = self.symbol {
            let parent = self.graph.symbols.get(id.0 as usize)?.parent?;
            return kinds.contains(&self.kind_of(parent)?).then_some(parent);
        }
        let text = container_text(&self.change.symbol)?;
        self.index
            .find(&text)
            .iter()
            .copied()
            .find(|&id| self.kind_of(id).is_some_and(|kind| kinds.contains(&kind)))
    }

    /// Whether `referrer` can no longer see the changed symbol once it has
    /// visibility `to`.
    fn outside_visibility(&self, to: Visibility, referrer: SymbolId) -> bool {
        let head = self.head;
        let Some(symbol) = self.symbol else {
            return false;
        };
        let referrer_module = enclosing_module(head, referrer);
        let outside = |boundary: Option<SymbolId>| match boundary {
            None => true,
            Some(boundary) => {
                referrer_module.is_none_or(|module| !is_self_or_descendant(head, module, boundary))
            }
        };
        match to {
            Visibility::Private => outside(enclosing_module(head, symbol)),
            Visibility::Restricted => outside(
                enclosing_module(head, symbol).and_then(|module| enclosing_module(head, module)),
            ),
            Visibility::Crate => {
                match (
                    head.symbols.get(referrer.0 as usize),
                    head.symbols.get(symbol.0 as usize),
                ) {
                    (Some(referrer), Some(changed)) => referrer.krate != changed.krate,
                    _ => false,
                }
            }
            Visibility::Public => false,
        }
    }

    /// Whether some implementation of `container`, a trait, replaces the
    /// changed item.
    fn is_overridden(&self, container: Option<SymbolId>) -> bool {
        let (Some(container), Some(symbol)) = (container, self.symbol) else {
            return false;
        };
        self.index
            .edges_to(container)
            .iter()
            .filter_map(|&edge| self.graph.edges.get(edge as usize))
            .filter(|reference| reference.kind == EdgeKind::Implements)
            .filter(|reference| self.kind_of(reference.from) == Some(SymbolKind::Impl))
            .any(|reference| self.implements(reference.from, symbol))
    }

    /// Whether `block` is an implementation that keeps the default of the
    /// changed trait item.
    fn keeps_default(&self, block: SymbolId) -> bool {
        self.kind_of(block) == Some(SymbolKind::Impl)
            && self
                .symbol
                .is_none_or(|symbol| !self.implements(block, symbol))
    }

    /// Whether a child of `block` has an `Implements` edge to `item`.
    fn implements(&self, block: SymbolId, item: SymbolId) -> bool {
        self.index.children(block).iter().any(|&child| {
            self.index.edges_from(child).iter().any(|&edge| {
                self.graph
                    .edges
                    .get(edge as usize)
                    .is_some_and(|reference| {
                        reference.kind == EdgeKind::Implements && reference.to == item
                    })
            })
        })
    }
}

/// The path text of a symbol's container: the path without its last segment.
fn container_text(path: &SymbolPath) -> Option<String> {
    let text = path.path.as_str();
    let rest = text.strip_suffix(path.name())?;
    let parent = rest.strip_suffix("::")?;
    (!parent.is_empty()).then(|| parent.to_owned())
}

/// Whether `id` is `ancestor` itself or lies inside it by `parent`.
fn is_self_or_descendant(graph: &RevisionGraph, id: SymbolId, ancestor: SymbolId) -> bool {
    let mut current = Some(id);
    while let Some(symbol) = current {
        if symbol == ancestor {
            return true;
        }
        current = graph
            .symbols
            .get(symbol.0 as usize)
            .and_then(|symbol| symbol.parent);
    }
    false
}

/// The nearest ancestor of `id` that is a module.
fn enclosing_module(graph: &RevisionGraph, id: SymbolId) -> Option<SymbolId> {
    let mut current = graph.symbols.get(id.0 as usize)?.parent;
    while let Some(parent) = current {
        let symbol = graph.symbols.get(parent.0 as usize)?;
        if symbol.path.kind == SymbolKind::Module {
            return Some(parent);
        }
        current = symbol.parent;
    }
    None
}

#[cfg(test)]
pub(crate) mod testkit {
    use blastr_model::{
        Certainty, Change, ChangeDetail, ChangeKind, CrateId, CrateInfo, Delta, Edge, EdgeDetail,
        EdgeKind, Location, RelPath, RevisionGraph, Span, Symbol, SymbolFlags, SymbolId,
        SymbolKind, SymbolPath, TargetKind, Visibility,
    };

    use super::{Graphs, Reach, Side, first_steps};

    /// Builds a [`RevisionGraph`] one symbol and one edge at a time.
    pub(crate) struct GraphBuilder {
        graph: RevisionGraph,
    }

    impl GraphBuilder {
        /// A builder whose graph holds one crate named `krate`.
        pub(crate) fn new() -> GraphBuilder {
            let mut graph = RevisionGraph::default();
            graph.crates.push(CrateInfo {
                name: "krate".to_owned(),
                package: "krate".to_owned(),
                target_kind: TargetKind::Lib,
                target_name: "krate".to_owned(),
                root_file: RelPath::new("src/lib.rs"),
            });
            GraphBuilder { graph }
        }

        /// Adds a symbol in `src/lib.rs` without a parent.
        pub(crate) fn add(&mut self, path: &str, kind: SymbolKind) -> SymbolId {
            let id = SymbolId(self.graph.symbols.len() as u32);
            self.graph.symbols.push(Symbol {
                path: SymbolPath::new(path, kind),
                krate: CrateId(0),
                location: Location {
                    file: RelPath::new("src/lib.rs"),
                    span: Span {
                        start: 0,
                        end: 10,
                        start_line: 1,
                        end_line: 1,
                    },
                },
                visibility: Visibility::Private,
                signature_hash: 0,
                body_hash: 0,
                attr_hash: 0,
                body_sketch: Vec::new(),
                parent: None,
                flags: SymbolFlags::default(),
                impl_info: None,
            });
            id
        }

        /// Adds a symbol inside `parent`.
        pub(crate) fn child(&mut self, parent: SymbolId, path: &str, kind: SymbolKind) -> SymbolId {
            let id = self.add(path, kind);
            self.symbol(id).parent = Some(parent);
            id
        }

        /// One symbol while it is being described.
        pub(crate) fn symbol(&mut self, id: SymbolId) -> &mut Symbol {
            &mut self.graph.symbols[id.0 as usize]
        }

        /// Puts the symbol in `file`.
        pub(crate) fn file(&mut self, id: SymbolId, file: &str) {
            self.symbol(id).location.file = RelPath::new(file);
        }

        /// Gives the symbol a body.
        pub(crate) fn body(&mut self, id: SymbolId) {
            self.symbol(id).flags.has_body = true;
        }

        /// Puts the symbol in another crate, adding the crate if needed.
        pub(crate) fn krate(&mut self, id: SymbolId, index: u32) {
            while self.graph.crates.len() <= index as usize {
                let number = self.graph.crates.len();
                self.graph.crates.push(CrateInfo {
                    name: format!("krate{number}"),
                    package: format!("krate{number}"),
                    target_kind: TargetKind::Lib,
                    target_name: format!("krate{number}"),
                    root_file: RelPath::new("src/lib.rs"),
                });
            }
            self.symbol(id).krate = CrateId(index);
        }

        /// Adds a resolved edge.
        pub(crate) fn edge(
            &mut self,
            from: SymbolId,
            to: SymbolId,
            kind: EdgeKind,
            detail: EdgeDetail,
        ) {
            self.push(from, to, kind, detail, Certainty::Resolved);
        }

        /// Adds a candidate edge.
        pub(crate) fn candidate(
            &mut self,
            from: SymbolId,
            to: SymbolId,
            kind: EdgeKind,
            detail: EdgeDetail,
        ) {
            self.push(from, to, kind, detail, Certainty::Candidate);
        }

        /// Adds an edge of the given certainty.
        fn push(
            &mut self,
            from: SymbolId,
            to: SymbolId,
            kind: EdgeKind,
            detail: EdgeDetail,
            certainty: Certainty,
        ) {
            self.graph.edges.push(Edge {
                from,
                to,
                kind,
                certainty,
                site: Location {
                    file: RelPath::new("src/lib.rs"),
                    span: Span {
                        start: 1,
                        end: 2,
                        start_line: 1,
                        end_line: 1,
                    },
                },
                detail,
            });
        }

        /// The graph built so far.
        pub(crate) fn finish(self) -> RevisionGraph {
            self.graph
        }
    }

    /// A change of `kind` to the symbol at `path`, without a detail.
    pub(crate) fn change(kind: ChangeKind, path: &str, symbol_kind: SymbolKind) -> Change {
        detailed(kind, path, symbol_kind, ChangeDetail::None)
    }

    /// A change that carries `detail`.
    pub(crate) fn detailed(
        kind: ChangeKind,
        path: &str,
        symbol_kind: SymbolKind,
        detail: ChangeDetail,
    ) -> Change {
        Change {
            kind,
            symbol: SymbolPath::new(path, symbol_kind),
            old: None,
            new: None,
            detail,
        }
    }

    /// A changed member of a field, variant, trait item or implementation.
    pub(crate) fn member(
        kind: ChangeKind,
        path: &str,
        symbol_kind: SymbolKind,
        delta: Delta,
    ) -> Change {
        detailed(kind, path, symbol_kind, ChangeDetail::Member(delta))
    }

    /// The first steps of `change` over the two graphs.
    pub(crate) fn first(base: &RevisionGraph, head: &RevisionGraph, change: &Change) -> Vec<Reach> {
        let graphs = Graphs::new(base, head);
        first_steps(change, &graphs)
    }

    /// The path of the symbol one step reaches.
    pub(crate) fn step_path(reach: &Reach, base: &RevisionGraph, head: &RevisionGraph) -> String {
        let graph = match reach.side {
            Side::Base => base,
            Side::Head => head,
        };
        let from = graph
            .edges
            .get(reach.edge as usize)
            .map(|reference| reference.from);
        from.and_then(|id| graph.symbols.get(id.0 as usize))
            .map(|symbol| symbol.path.path.clone())
            .unwrap_or_default()
    }

    /// The paths the steps reach, sorted.
    pub(crate) fn reached_paths(
        steps: &[Reach],
        base: &RevisionGraph,
        head: &RevisionGraph,
    ) -> Vec<String> {
        let mut paths: Vec<String> = steps
            .iter()
            .map(|reach| step_path(reach, base, head))
            .collect();
        paths.sort();
        paths
    }

    /// The step that reaches the symbol at `path`.
    pub(crate) fn step_to<'s>(
        steps: &'s [Reach],
        base: &RevisionGraph,
        head: &RevisionGraph,
        path: &str,
    ) -> &'s Reach {
        steps
            .iter()
            .find(|reach| step_path(reach, base, head) == path)
            .unwrap_or_else(|| panic!("no step reaches `{path}`"))
    }
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        Certainty, ChangeDetail, ChangeKind, Delta, EdgeDetail, EdgeKind, ImpactClass, Reason,
        RevisionGraph, SymbolKind, SymbolPath, Visibility,
    };

    use super::testkit::{GraphBuilder, change, detailed, first, member, reached_paths, step_to};
    use super::*;

    /// An empty graph, for rules that read one side only.
    fn nothing() -> RevisionGraph {
        RevisionGraph::default()
    }

    #[test]
    fn a_body_change_reaches_its_callers() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let user = builder.child(module, "krate::User", SymbolKind::Struct);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(user, function, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::f", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::caller"]
        );
        let step = step_to(&steps, &nothing(), &head, "krate::caller");
        assert_eq!(step.class, ImpactClass::Behaviour);
        assert_eq!(step.certainty, Certainty::Resolved);
        assert_eq!(step.reason, Reason::CallsChanged);
        assert!(!step.coarse);
    }

    #[test]
    fn a_body_change_reaches_holders_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(reader, function, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::f", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::holder"]
        );
        let step = step_to(&steps, &nothing(), &head, "krate::holder");
        assert_eq!(step.certainty, Certainty::Candidate);
        assert_eq!(step.reason, Reason::TakesChangedAsValue);
        assert_eq!(step.class, ImpactClass::Behaviour);
    }

    #[test]
    fn a_body_change_reaches_dispatch_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let method = builder.child(module, "krate::f", SymbolKind::Method);
        let dispatcher = builder.child(module, "krate::dispatcher", SymbolKind::Function);
        let user = builder.child(module, "krate::User", SymbolKind::Struct);
        builder.edge(
            dispatcher,
            method,
            EdgeKind::MayDispatchTo,
            EdgeDetail::None,
        );
        builder.edge(user, method, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::f", SymbolKind::Method);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::dispatcher"]
        );
        assert!(
            steps
                .iter()
                .all(|step| step.certainty == Certainty::Candidate)
        );
        assert!(!steps.is_empty());
        assert_eq!(steps[0].reason, Reason::DispatchesToChanged);
    }

    #[test]
    fn a_body_change_in_a_macro_function_reaches_its_expansions() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let derive = builder.child(module, "krate::derive_name", SymbolKind::Function);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let block = builder.child(module, "krate::<impl Name for Point>", SymbolKind::Impl);
        builder.edge(point, derive, EdgeKind::Expands, EdgeDetail::None);
        builder.edge(block, derive, EdgeKind::Expands, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::derive_name", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::<impl Name for Point>", "krate::Point"]
        );
        assert!(
            steps
                .iter()
                .all(|step| step.certainty == Certainty::Candidate),
            "a body change does not prove the expansion changed"
        );
        assert!(
            steps
                .iter()
                .all(|step| step.reason == Reason::InvokesChangedMacro)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.class == ImpactClass::Behaviour)
        );
    }

    #[test]
    fn a_body_change_in_something_that_is_not_a_function_follows_no_expansion() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let macro_ = builder.child(module, "krate::declare", SymbolKind::Macro);
        let invoker = builder.child(module, "krate::invoker", SymbolKind::Function);
        builder.edge(invoker, macro_, EdgeKind::Expands, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::declare", SymbolKind::Macro);

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_signature_change_reaches_callers_and_holders() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(reader, function, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Signature, "krate::f", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::caller", "krate::holder"],
            "an edge of a kind a signature change does not follow is not reached"
        );
        for path in ["krate::caller", "krate::holder"] {
            let step = step_to(&steps, &nothing(), &head, path);
            assert_eq!(step.class, ImpactClass::Adaptation);
            assert_eq!(step.certainty, Certainty::Resolved);
            assert_eq!(step.reason, Reason::SignatureChanged);
        }
    }

    #[test]
    fn a_signature_change_to_a_trait_impl_method_leaves_callers_alone() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let stage = builder.child(module, "krate::Stage", SymbolKind::Trait);
        let item = builder.child(stage, "krate::Stage::apply", SymbolKind::Method);
        let block = builder.child(module, "krate::<impl Stage for V>", SymbolKind::Impl);
        let method = builder.child(
            block,
            "krate::<impl Stage for V>::apply",
            SymbolKind::Method,
        );
        builder.symbol(method).flags.in_trait_impl = true;
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        builder.edge(block, stage, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(method, item, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(caller, method, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, method, EdgeKind::TakesAsValue, EdgeDetail::None);
        let head = builder.finish();
        let change = change(
            ChangeKind::Signature,
            "krate::<impl Stage for V>::apply",
            SymbolKind::Method,
        );

        // The call above names the trait item, which did not change.
        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_signature_change_reaches_dispatch_only() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let method = builder.child(module, "krate::run", SymbolKind::Method);
        let dispatcher = builder.child(module, "krate::dispatcher", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(
            dispatcher,
            method,
            EdgeKind::MayDispatchTo,
            EdgeDetail::None,
        );
        builder.edge(user, method, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Signature, "krate::run", SymbolKind::Method);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::dispatcher"],
            "a direct call is reached by another rule, not here"
        );
        let step = step_to(&steps, &nothing(), &head, "krate::dispatcher");
        assert_eq!(step.class, ImpactClass::Behaviour);
        assert_eq!(step.certainty, Certainty::Candidate);
        assert_eq!(step.reason, Reason::DispatchesToChanged);
    }

    #[test]
    fn a_declaration_change_reaches_the_users_of_the_type() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let field = builder.child(point, "krate::Point::x", SymbolKind::Field);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        let builder_fn = builder.child(module, "krate::build", SymbolKind::Function);
        let matcher = builder.child(module, "krate::match_it", SymbolKind::Function);
        let block = builder.child(module, "krate::<impl Trait for Point>", SymbolKind::Impl);
        builder.edge(user, point, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(builder_fn, point, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(matcher, point, EdgeKind::Matches, EdgeDetail::None);
        builder.edge(block, point, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(field, point, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(user, point, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Signature, "krate::Point", SymbolKind::Struct);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec![
                "krate::<impl Trait for Point>",
                "krate::build",
                "krate::match_it",
                "krate::user"
            ],
            "a call edge and the type's own field are not followed"
        );
        assert!(
            steps
                .iter()
                .all(|step| step.class == ImpactClass::Adaptation)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.certainty == Certainty::Candidate)
        );
        assert!(steps.iter().all(|step| step.coarse));
        assert_eq!(steps[0].reason, Reason::DeclarationChanged);
    }

    #[test]
    fn a_private_change_reaches_only_referrers_outside_the_module() {
        let mut builder = GraphBuilder::new();
        let root = builder.add("krate", SymbolKind::Module);
        let inner = builder.child(root, "krate::inner", SymbolKind::Module);
        let hidden = builder.child(inner, "krate::inner::hidden", SymbolKind::Struct);
        let sibling = builder.child(inner, "krate::inner::sibling", SymbolKind::Function);
        let deep = builder.child(inner, "krate::inner::deep", SymbolKind::Module);
        let nested = builder.child(deep, "krate::inner::deep::user", SymbolKind::Function);
        let outside = builder.child(root, "krate::outside", SymbolKind::Function);
        builder.edge(sibling, hidden, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(nested, hidden, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(outside, hidden, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::inner::hidden",
            SymbolKind::Struct,
            ChangeDetail::Visibility {
                from: Visibility::Public,
                to: Visibility::Private,
            },
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::outside"]
        );
        let step = step_to(&steps, &nothing(), &head, "krate::outside");
        assert_eq!(step.class, ImpactClass::Adaptation);
        assert_eq!(step.reason, Reason::NoLongerVisible);
        assert!(!step.coarse);
    }

    #[test]
    fn restricted_visibility_is_coarse_and_crate_visibility_crosses_crates() {
        let mut builder = GraphBuilder::new();
        let root = builder.add("krate", SymbolKind::Module);
        let inner = builder.child(root, "krate::inner", SymbolKind::Module);
        let deeper = builder.child(inner, "krate::inner::deeper", SymbolKind::Module);
        let hidden = builder.child(deeper, "krate::inner::deeper::hidden", SymbolKind::Struct);
        let sibling = builder.child(
            deeper,
            "krate::inner::deeper::sibling",
            SymbolKind::Function,
        );
        let cousin = builder.child(inner, "krate::inner::cousin", SymbolKind::Function);
        let outside = builder.child(root, "krate::outside", SymbolKind::Function);
        builder.edge(sibling, hidden, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(cousin, hidden, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(outside, hidden, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::inner::deeper::hidden",
            SymbolKind::Struct,
            ChangeDetail::Visibility {
                from: Visibility::Public,
                to: Visibility::Restricted,
            },
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::outside"]
        );
        assert!(steps[0].coarse, "a restricted visibility is a coarse rule");

        let mut builder = GraphBuilder::new();
        let root = builder.add("krate", SymbolKind::Module);
        let shared = builder.child(root, "krate::shared", SymbolKind::Struct);
        let neighbour = builder.child(root, "krate::neighbour", SymbolKind::Function);
        let elsewhere = builder.child(root, "other::user", SymbolKind::Function);
        builder.krate(elsewhere, 1);
        builder.edge(neighbour, shared, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(elsewhere, shared, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::shared",
            SymbolKind::Struct,
            ChangeDetail::Visibility {
                from: Visibility::Public,
                to: Visibility::Crate,
            },
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["other::user"]
        );
        assert!(!steps[0].coarse);
    }

    #[test]
    fn a_narrowed_field_reaches_the_literals_and_patterns_that_name_it() {
        let mut builder = GraphBuilder::new();
        let root = builder.add("krate", SymbolKind::Module);
        let spec = builder.child(root, "krate::Spec", SymbolKind::Struct);
        let argument = builder.child(spec, "krate::Spec::argument", SymbolKind::Field);
        let name = builder.child(spec, "krate::Spec::name", SymbolKind::Field);
        let parse = builder.child(root, "krate::Spec::parse", SymbolKind::Function);
        let build = builder.add("other::build", SymbolKind::Function);
        let rebuild = builder.add("other::rebuild", SymbolKind::Function);
        let reader = builder.add("other::reader", SymbolKind::Function);
        let pattern = builder.add("other::pattern", SymbolKind::Function);
        let naming = builder.add("other::naming", SymbolKind::Function);
        builder.krate(build, 1);
        builder.krate(rebuild, 1);
        builder.krate(reader, 1);
        builder.krate(pattern, 1);
        builder.krate(naming, 1);
        // A literal carries no edge to the field, only to its container.
        builder.edge(parse, spec, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(build, spec, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(rebuild, spec, EdgeKind::Constructs, EdgeDetail::WithRest);
        builder.edge(reader, argument, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(pattern, argument, EdgeKind::Matches, EdgeDetail::None);
        builder.edge(naming, name, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::Spec::argument",
            SymbolKind::Field,
            ChangeDetail::Visibility {
                from: Visibility::Public,
                to: Visibility::Crate,
            },
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec![
                "other::build",
                "other::pattern",
                "other::reader",
                "other::rebuild"
            ]
        );
        let step = step_to(&steps, &nothing(), &head, "other::build");
        assert_eq!(step.class, ImpactClass::Adaptation);
        assert_eq!(step.reason, Reason::NoLongerVisible);
        assert_eq!(step.certainty, Certainty::Resolved);
        let step = step_to(&steps, &nothing(), &head, "other::rebuild");
        assert_eq!(
            step.certainty,
            Certainty::Candidate,
            "a literal with `..` may leave the field out"
        );
    }

    #[test]
    fn a_widened_visibility_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let item = builder.child(module, "krate::item", SymbolKind::Struct);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(user, item, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::Visibility,
            "krate::item",
            SymbolKind::Struct,
            ChangeDetail::Visibility {
                from: Visibility::Private,
                to: Visibility::Public,
            },
        );

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_removal_reaches_base_side_referrers_but_not_the_container() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let gone = builder.child(module, "krate::gone", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, gone, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(module, gone, EdgeKind::Contains, EdgeDetail::None);
        let base = builder.finish();
        let change = change(ChangeKind::Removed, "krate::gone", SymbolKind::Function);
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(reached_paths(&steps, &base, &head), vec!["krate::caller"]);
        assert_eq!(steps[0].side, Side::Base);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert_eq!(steps[0].reason, Reason::RefersToRemoved);
    }

    #[test]
    fn a_rename_reaches_base_side_referrers_of_the_previous_name() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let old = builder.child(module, "krate::old_name", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(caller, old, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(other, module, EdgeKind::Calls, EdgeDetail::None);
        let base = builder.finish();
        let change = detailed(
            ChangeKind::Renamed,
            "krate::new_name",
            SymbolKind::Function,
            ChangeDetail::Renamed {
                from: SymbolPath::new("krate::old_name", SymbolKind::Function),
            },
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(reached_paths(&steps, &base, &head), vec!["krate::caller"]);
        assert_eq!(steps[0].reason, Reason::RefersToRenamed);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
    }

    #[test]
    fn a_removed_field_skips_readers_of_other_fields() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let x = builder.child(point, "krate::Point::x", SymbolKind::Field);
        let y = builder.child(point, "krate::Point::y", SymbolKind::Field);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let other_reader = builder.child(module, "krate::other_reader", SymbolKind::Function);
        let writer = builder.child(module, "krate::writer", SymbolKind::Function);
        let literal = builder.child(module, "krate::literal", SymbolKind::Function);
        let rest_literal = builder.child(module, "krate::rest_literal", SymbolKind::Function);
        builder.edge(reader, x, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(other_reader, y, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(writer, x, EdgeKind::WritesField, EdgeDetail::None);
        builder.edge(literal, x, EdgeKind::WritesField, EdgeDetail::None);
        builder.edge(rest_literal, y, EdgeKind::WritesField, EdgeDetail::None);
        let base = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::x",
            SymbolKind::Field,
            Delta::Removed,
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(
            reached_paths(&steps, &base, &head),
            vec!["krate::literal", "krate::reader", "krate::writer"],
            "a reader of another field is not reached, and neither is a literal\n             that supplies another field"
        );
        assert!(steps.iter().all(|step| step.side == Side::Base));
        assert_eq!(
            step_to(&steps, &base, &head, "krate::reader").certainty,
            Certainty::Resolved
        );
        assert_eq!(steps[0].reason, Reason::UsesChangedField);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
    }

    #[test]
    fn a_modified_field_carries_only_reads_further() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let x = builder.child(point, "krate::Point::x", SymbolKind::Field);
        let y = builder.child(point, "krate::Point::y", SymbolKind::Field);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let other_reader = builder.child(module, "krate::other_reader", SymbolKind::Function);
        let writer = builder.child(module, "krate::writer", SymbolKind::Function);
        let literal = builder.child(module, "krate::literal", SymbolKind::Function);
        builder.edge(reader, x, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(other_reader, y, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(writer, x, EdgeKind::WritesField, EdgeDetail::None);
        builder.edge(literal, x, EdgeKind::WritesField, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::x",
            SymbolKind::Field,
            Delta::Modified,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::literal", "krate::reader", "krate::writer"]
        );
    }

    #[test]
    fn a_new_field_reaches_literals_without_rest() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let literal = builder.child(module, "krate::literal", SymbolKind::Function);
        let rest_literal = builder.child(module, "krate::rest_literal", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let field = builder.child(point, "krate::Point::x", SymbolKind::Field);
        builder.edge(literal, point, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(
            rest_literal,
            point,
            EdgeKind::Constructs,
            EdgeDetail::WithRest,
        );
        builder.edge(reader, field, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::x",
            SymbolKind::Field,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::literal"]
        );
        assert_eq!(steps[0].reason, Reason::ConstructsWithoutField);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
    }

    #[test]
    fn an_invented_field_finds_its_container_by_path() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let literal = builder.child(module, "krate::literal", SymbolKind::Function);
        let rest_literal = builder.child(module, "krate::rest_literal", SymbolKind::Function);
        builder.edge(literal, point, EdgeKind::Constructs, EdgeDetail::None);
        builder.edge(
            rest_literal,
            point,
            EdgeKind::Constructs,
            EdgeDetail::WithRest,
        );
        let head = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::<new>",
            SymbolKind::Field,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::literal"]
        );
    }

    #[test]
    fn a_new_field_reaches_patterns_without_rest() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let pattern = builder.child(module, "krate::pattern", SymbolKind::Function);
        let rest_pattern = builder.child(module, "krate::rest_pattern", SymbolKind::Function);
        let variant = builder.child(module, "krate::Choice::First", SymbolKind::Variant);
        let variant_pattern = builder.child(module, "krate::variant_pattern", SymbolKind::Function);
        builder.edge(pattern, point, EdgeKind::Matches, EdgeDetail::None);
        builder.edge(rest_pattern, point, EdgeKind::Matches, EdgeDetail::WithRest);
        builder.edge(
            variant_pattern,
            variant,
            EdgeKind::Matches,
            EdgeDetail::None,
        );
        let head = builder.finish();
        let change = member(
            ChangeKind::Field,
            "krate::Point::<new>",
            SymbolKind::Field,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::pattern"]
        );
        assert_eq!(steps[0].reason, Reason::PatternMissesField);

        let variant_change = member(
            ChangeKind::Field,
            "krate::Choice::First::<new>",
            SymbolKind::Field,
            Delta::Added,
        );
        assert_eq!(
            reached_paths(
                &first(&nothing(), &head, &variant_change),
                &nothing(),
                &head
            ),
            vec!["krate::variant_pattern"],
            "a pattern of a variant that names every field misses a new one too"
        );
    }

    #[test]
    fn an_exhaustive_match_is_an_adaptation_and_a_wildcard_is_not() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let choice = builder.child(module, "krate::Choice", SymbolKind::Enum);
        let exhaustive = builder.child(module, "krate::exhaustive", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(
            exhaustive,
            choice,
            EdgeKind::Matches,
            EdgeDetail::MatchExhaustive,
        );
        builder.edge(other, choice, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Variant,
            "krate::Choice::<new>",
            SymbolKind::Variant,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::exhaustive"],
            "a wildcard match is reached by another rule, not here"
        );
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert_eq!(steps[0].reason, Reason::ExhaustiveMatchMissesVariant);
    }

    #[test]
    fn a_wildcard_match_is_behaviour() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let choice = builder.child(module, "krate::Choice", SymbolKind::Enum);
        let wildcard = builder.child(module, "krate::wildcard", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(
            wildcard,
            choice,
            EdgeKind::Matches,
            EdgeDetail::MatchWildcard,
        );
        builder.edge(other, choice, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Variant,
            "krate::Choice::<new>",
            SymbolKind::Variant,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::wildcard"],
            "an exhaustive match is reached by another rule, not here"
        );
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
        assert_eq!(steps[0].reason, Reason::WildcardCatchesNewVariant);
    }

    #[test]
    fn a_removed_variant_reaches_its_uses_on_the_base_side() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let choice = builder.child(module, "krate::Choice", SymbolKind::Enum);
        let first_variant = builder.child(choice, "krate::Choice::First", SymbolKind::Variant);
        let payload = builder.child(
            first_variant,
            "krate::Choice::First::value",
            SymbolKind::Field,
        );
        let second = builder.child(choice, "krate::Choice::Second", SymbolKind::Variant);
        let constructor = builder.child(module, "krate::constructor", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(
            constructor,
            first_variant,
            EdgeKind::Constructs,
            EdgeDetail::None,
        );
        builder.edge(reader, payload, EdgeKind::ReadsField, EdgeDetail::None);
        builder.edge(other, second, EdgeKind::Matches, EdgeDetail::None);
        let base = builder.finish();
        let change = member(
            ChangeKind::Variant,
            "krate::Choice::First",
            SymbolKind::Variant,
            Delta::Removed,
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(
            reached_paths(&steps, &base, &head),
            vec!["krate::constructor", "krate::reader"],
            "a use of another variant is not reached"
        );
        assert!(steps.iter().all(|step| step.side == Side::Base));
        assert_eq!(steps[0].reason, Reason::UsesChangedVariant);
    }

    // A reshaped variant has no rule of its own: its fields are reported with
    // it, and each field rule reaches the literals and patterns that name it.
    #[test]
    fn a_reshaped_variant_reaches_nobody_by_itself() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let choice = builder.child(module, "krate::Choice", SymbolKind::Enum);
        let first_variant = builder.child(choice, "krate::Choice::First", SymbolKind::Variant);
        let constructor = builder.child(module, "krate::constructor", SymbolKind::Function);
        let other = builder.child(module, "krate::other", SymbolKind::Function);
        builder.edge(
            constructor,
            first_variant,
            EdgeKind::Constructs,
            EdgeDetail::None,
        );
        builder.edge(other, choice, EdgeKind::Matches, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Variant,
            "krate::Choice::First",
            SymbolKind::Variant,
            Delta::Modified,
        );

        let steps = first(&nothing(), &head, &change);

        assert!(
            steps.is_empty(),
            "the fields that reshaped the variant carry its consequences"
        );
    }

    #[test]
    fn a_required_trait_item_reaches_the_implementations() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        builder.child(trait_, "krate::Trait::extra", SymbolKind::Method);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let block = builder.child(module, "krate::<impl Trait for Point>", SymbolKind::Impl);
        builder.edge(block, trait_, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(point, trait_, EdgeKind::Implements, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::TraitItem,
            "krate::Trait::extra",
            SymbolKind::Method,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::<impl Trait for Point>"],
            "only an implementation block is reached"
        );
        assert_eq!(steps[0].reason, Reason::MustImplementTraitItem);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
    }

    #[test]
    fn a_trait_item_with_a_body_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let default = builder.child(trait_, "krate::Trait::extra", SymbolKind::Method);
        builder.body(default);
        let block = builder.child(module, "krate::<impl Trait for Point>", SymbolKind::Impl);
        builder.edge(block, trait_, EdgeKind::Implements, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::TraitItem,
            "krate::Trait::extra",
            SymbolKind::Method,
            Delta::Added,
        );

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_modified_trait_item_reaches_implementations_and_callers() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        let block = builder.child(module, "krate::<impl Trait for Point>", SymbolKind::Impl);
        let implementation = builder.child(
            block,
            "krate::<impl Trait for Point>::run",
            SymbolKind::Method,
        );
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(block, trait_, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(implementation, run, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, run, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(user, trait_, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            Delta::Modified,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec![
                "krate::<impl Trait for Point>::run",
                "krate::caller",
                "krate::holder"
            ]
        );
        let implementation = step_to(
            &steps,
            &nothing(),
            &head,
            "krate::<impl Trait for Point>::run",
        );
        assert_eq!(implementation.reason, Reason::MustImplementTraitItem);
        let caller = step_to(&steps, &nothing(), &head, "krate::caller");
        assert_eq!(caller.reason, Reason::UsesChangedTraitItem);
    }

    #[test]
    fn a_removed_trait_item_reaches_its_uses_on_the_base_side() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(user, trait_, EdgeKind::UsesType, EdgeDetail::None);
        let base = builder.finish();
        let change = member(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            Delta::Removed,
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(reached_paths(&steps, &base, &head), vec!["krate::caller"]);
        assert_eq!(steps[0].reason, Reason::UsesChangedTraitItem);
    }

    #[test]
    fn a_changed_default_body_skips_implementations_that_override_it() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        builder.body(run);
        let overriding = builder.child(module, "krate::<impl Trait for A>", SymbolKind::Impl);
        let overridden = builder.child(
            overriding,
            "krate::<impl Trait for A>::run",
            SymbolKind::Method,
        );
        let inheriting = builder.child(module, "krate::<impl Trait for B>", SymbolKind::Impl);
        builder.child(
            inheriting,
            "krate::<impl Trait for B>::other",
            SymbolKind::Method,
        );
        builder.edge(overriding, trait_, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(inheriting, trait_, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(overridden, run, EdgeKind::Implements, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            ChangeDetail::DefaultBody,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::<impl Trait for B>"],
            "an implementation that overrides the default is not reached"
        );
        assert_eq!(steps[0].reason, Reason::InheritsChangedDefault);
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
    }

    #[test]
    fn a_call_to_a_default_body_is_certain_unless_an_impl_overrides_it() {
        let build = |overriding: bool| {
            let mut builder = GraphBuilder::new();
            let module = builder.add("krate", SymbolKind::Module);
            let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
            let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
            builder.body(run);
            let block = builder.child(module, "krate::<impl Trait for A>", SymbolKind::Impl);
            let caller = builder.child(module, "krate::caller", SymbolKind::Function);
            builder.edge(block, trait_, EdgeKind::Implements, EdgeDetail::None);
            if overriding {
                let overridden =
                    builder.child(block, "krate::<impl Trait for A>::run", SymbolKind::Method);
                builder.edge(overridden, run, EdgeKind::Implements, EdgeDetail::None);
            }
            builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
            builder.finish()
        };
        let change = detailed(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            ChangeDetail::DefaultBody,
        );

        let plain = build(false);
        let steps = first(&nothing(), &plain, &change);
        let caller = step_to(&steps, &nothing(), &plain, "krate::caller");
        assert_eq!(caller.certainty, Certainty::Resolved);
        assert_eq!(caller.reason, Reason::CallsChanged);

        let overriding = build(true);
        let steps = first(&nothing(), &overriding, &change);
        let caller = step_to(&steps, &nothing(), &overriding, "krate::caller");
        assert_eq!(
            caller.certainty,
            Certainty::Candidate,
            "an implementation may replace the default"
        );
    }

    #[test]
    fn a_default_body_held_as_a_value_is_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        builder.body(run);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(holder, run, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(reader, run, EdgeKind::ReadsField, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            ChangeDetail::DefaultBody,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::holder"],
            "a call is reached by another rule, not here"
        );
        assert_eq!(steps[0].certainty, Certainty::Candidate);
        assert_eq!(steps[0].reason, Reason::TakesChangedAsValue);
    }

    #[test]
    fn a_new_implementation_reaches_the_users_of_its_items() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let block = builder.child(module, "krate::<impl Point>", SymbolKind::Impl);
        let run = builder.child(block, "krate::<impl Point>::run", SymbolKind::Method);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let dispatcher = builder.child(module, "krate::dispatcher", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(dispatcher, run, EdgeKind::MayDispatchTo, EdgeDetail::None);
        builder.edge(user, block, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::Impl,
            "krate::<impl Point>",
            SymbolKind::Impl,
            Delta::Added,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::caller", "krate::dispatcher"],
            "a use of the impl block itself is not followed"
        );
        assert_eq!(steps[0].reason, Reason::ImplResolutionChanged);
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
    }

    #[test]
    fn a_removed_implementation_reaches_its_users_on_the_base_side() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let block = builder.child(module, "krate::<impl Point>", SymbolKind::Impl);
        let run = builder.child(block, "krate::<impl Point>::run", SymbolKind::Method);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(caller, run, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(user, block, EdgeKind::UsesType, EdgeDetail::None);
        let base = builder.finish();
        let change = member(
            ChangeKind::Impl,
            "krate::<impl Point>",
            SymbolKind::Impl,
            Delta::Removed,
        );
        let head = nothing();

        let steps = first(&base, &head, &change);

        assert_eq!(reached_paths(&steps, &base, &head), vec!["krate::caller"]);
        assert_eq!(steps[0].reason, Reason::ImplResolutionChanged);
    }

    #[test]
    fn a_changed_value_reaches_its_readers() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let limit = builder.child(module, "krate::LIMIT", SymbolKind::Const);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(reader, limit, EdgeKind::UsesValue, EdgeDetail::None);
        builder.edge(user, limit, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::ConstValue, "krate::LIMIT", SymbolKind::Const);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::reader"]
        );
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
        assert_eq!(steps[0].reason, Reason::UsesChangedValue);
    }

    #[test]
    fn a_retyped_value_makes_its_readers_adapt() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let limit = builder.child(module, "krate::LIMIT", SymbolKind::Static);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(reader, limit, EdgeKind::UsesValue, EdgeDetail::None);
        let head = builder.finish();
        let change = detailed(
            ChangeKind::ConstValue,
            "krate::LIMIT",
            SymbolKind::Static,
            ChangeDetail::Retyped,
        );

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::reader"]
        );
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert_eq!(steps[0].reason, Reason::UsesRetypedValue);
    }

    #[test]
    fn a_changed_alias_reaches_its_users_as_a_coarse_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let alias = builder.child(module, "krate::Alias", SymbolKind::TypeAlias);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(user, alias, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(reader, alias, EdgeKind::UsesValue, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::TypeAlias, "krate::Alias", SymbolKind::TypeAlias);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::user"]
        );
        assert_eq!(steps[0].class, ImpactClass::Adaptation);
        assert_eq!(steps[0].certainty, Certainty::Candidate);
        assert_eq!(steps[0].reason, Reason::UsesChangedAlias);
        assert!(steps[0].coarse);
    }

    #[test]
    fn a_changed_macro_reaches_its_invocations() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let macro_ = builder.child(module, "krate::declare", SymbolKind::Macro);
        let invoker = builder.child(module, "krate::invoker", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(invoker, macro_, EdgeKind::Expands, EdgeDetail::None);
        builder.edge(caller, invoker, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::MacroDef, "krate::declare", SymbolKind::Macro);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::invoker"]
        );
        assert_eq!(steps[0].class, ImpactClass::Behaviour);
        assert_eq!(steps[0].certainty, Certainty::Candidate);
        assert_eq!(steps[0].reason, Reason::InvokesChangedMacro);
    }

    #[test]
    fn a_changed_attribute_reaches_what_the_body_rules_reach_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let datatype = builder.child(module, "krate::Type", SymbolKind::Struct);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(datatype, function, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Attribute, "krate::f", SymbolKind::Function);

        let steps = first(&nothing(), &head, &change);

        assert_eq!(
            reached_paths(&steps, &nothing(), &head),
            vec!["krate::caller", "krate::holder"]
        );
        assert!(
            steps
                .iter()
                .all(|step| step.certainty == Certainty::Candidate)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.reason == Reason::AttributeChanged)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.class == ImpactClass::Behaviour)
        );
    }

    #[test]
    fn an_attribute_on_anything_but_a_function_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(user, point, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Attribute, "krate::Point", SymbolKind::Struct);

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_formatting_only_change_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::NonSemantic, "krate::f", SymbolKind::Function);

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_change_to_a_symbol_the_side_does_not_hold_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::gone", SymbolKind::Function);

        assert!(first(&nothing(), &head, &change).is_empty());
    }

    #[test]
    fn a_chain_does_not_reach_through_the_target_itself() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let inner = builder.child(function, "krate::f::inner", SymbolKind::Function);
        builder.edge(inner, function, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = change(ChangeKind::Body, "krate::f", SymbolKind::Function);

        assert!(
            first(&nothing(), &head, &change).is_empty(),
            "a reference from inside the target is not followed"
        );
    }

    #[test]
    fn behaviour_propagation_follows_calls_and_values_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let constant = builder.child(module, "krate::LIMIT", SymbolKind::Const);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let dispatcher = builder.child(module, "krate::dispatcher", SymbolKind::Function);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        let user = builder.child(module, "krate::user", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(
            dispatcher,
            function,
            EdgeKind::MayDispatchTo,
            EdgeDetail::None,
        );
        builder.edge(reader, constant, EdgeKind::UsesValue, EdgeDetail::None);
        builder.edge(user, function, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let base = nothing();
        let graphs = Graphs::new(&base, &head);

        let mut reached = super::onward(function, &graphs);
        reached.extend(super::onward(constant, &graphs));

        let mut paths = reached_paths(&reached, &base, &head);
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "krate::caller",
                "krate::dispatcher",
                "krate::holder",
                "krate::reader"
            ]
        );
        assert!(
            reached
                .iter()
                .all(|step| step.class == ImpactClass::Behaviour)
        );
        assert_eq!(
            step_to(&reached, &base, &head, "krate::caller").certainty,
            Certainty::Resolved
        );
        let candidate = step_to(&reached, &base, &head, "krate::dispatcher");
        assert_eq!(candidate.certainty, Certainty::Candidate);
        assert_eq!(candidate.reason, Reason::DispatchesToChanged);
    }

    #[test]
    fn behaviour_propagation_follows_an_invocation_as_a_candidate() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let derive = builder.child(module, "krate::derive_name", SymbolKind::Function);
        let point = builder.child(module, "krate::Point", SymbolKind::Struct);
        let helper = builder.child(module, "krate::helper", SymbolKind::Function);
        builder.edge(point, derive, EdgeKind::Expands, EdgeDetail::None);
        builder.edge(helper, derive, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let base = nothing();
        let graphs = Graphs::new(&base, &head);

        let reached = super::onward(derive, &graphs);

        assert_eq!(
            reached_paths(&reached, &base, &head),
            vec!["krate::Point", "krate::helper"]
        );
        let expansion = step_to(&reached, &base, &head, "krate::Point");
        assert_eq!(expansion.class, ImpactClass::Behaviour);
        assert_eq!(expansion.certainty, Certainty::Candidate);
        assert_eq!(expansion.reason, Reason::InvokesChangedMacro);
        assert!(!expansion.coarse);
    }

    #[test]
    fn a_removed_field_that_alone_used_a_parameter_reaches_its_container() {
        let build = |sole: bool| {
            let mut builder = GraphBuilder::new();
            let module = builder.add("krate", SymbolKind::Module);
            let holder = builder.child(module, "krate::Holder", SymbolKind::Struct);
            let field = builder.child(holder, "krate::Holder::value", SymbolKind::Field);
            builder.symbol(field).flags.sole_parameter_use = sole;
            builder.edge(holder, field, EdgeKind::Contains, EdgeDetail::None);
            builder.finish()
        };
        let change = member(
            ChangeKind::Field,
            "krate::Holder::value",
            SymbolKind::Field,
            Delta::Removed,
        );

        let base = build(true);
        let steps = first(&base, &nothing(), &change);
        assert_eq!(
            reached_paths(&steps, &base, &nothing()),
            vec!["krate::Holder"]
        );
        assert_eq!(steps[0].reason, Reason::ParameterBecomesUnused);
        assert_eq!(steps[0].class, ImpactClass::Adaptation);

        let base = build(false);
        assert!(first(&base, &nothing(), &change).is_empty());
    }

    #[test]
    fn a_modified_trait_item_reaches_the_callers_of_its_implementations_in_two_steps() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        let block = builder.child(module, "krate::<impl Trait for A>", SymbolKind::Impl);
        let implementation =
            builder.child(block, "krate::<impl Trait for A>::run", SymbolKind::Method);
        let direct = builder.child(module, "krate::direct", SymbolKind::Function);
        builder.edge(implementation, run, EdgeKind::Implements, EdgeDetail::None);
        builder.candidate(direct, implementation, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let change = member(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            Delta::Modified,
        );

        let steps = first(&nothing(), &head, &change);

        let caller = step_to(&steps, &nothing(), &head, "krate::direct");
        assert_eq!(caller.reason, Reason::UsesChangedTraitItem);
        assert_eq!(caller.class, ImpactClass::Adaptation);
        assert_eq!(caller.certainty, Certainty::Candidate);
        let via = caller.via.expect("the caller hangs off the implementation");
        assert_eq!(head.edges[via.edge as usize].from, implementation);
        assert_eq!(via.certainty, Certainty::Resolved);
    }

    #[test]
    fn references_are_every_edge_kind_but_contains() {
        let expected: Vec<EdgeKind> = EdgeKind::ALL
            .iter()
            .copied()
            .filter(|&kind| kind != EdgeKind::Contains)
            .collect();
        assert_eq!(super::REFERENCES, expected.as_slice());
    }
}
