//! The impact search: from the first steps of every change to the items they
//! reach, with the evidence for each.
//!
//! The search is a breadth-first search over the incoming edges of the head
//! graph, run on two layers at once: the shortest path that takes resolved steps
//! only, and the shortest path of any certainty. Every state keeps a pointer to
//! the state it came from, so evidence paths are built at the end, and only for
//! the paths that are reported.
//!
//! An item's best path is the one of the highest class, then the highest
//! certainty, then the fewest steps. The depth setting limits which items are
//! listed and nothing else: the search itself is unbounded, so test selection
//! sees every test the changes can reach.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use blastr_model::{
    CancelToken, Cancelled, Certainty, Change, ChangeDetail, ChangeKind, Confidence, Edge,
    EvidencePath, EvidenceStep, ImpactClass, ImpactItem, ImpactLimits, ImpactTags, Location,
    RelPath, Span, SymbolId, SymbolPath,
};

use super::rules::{self, Graphs, Reach, Side};
use super::{ImpactInput, Propagation};

/// How many paths are kept for each item.
const KEPT_PATHS: usize = 3;

/// Computes what the changes reach.
pub(crate) fn propagate(
    input: &ImpactInput<'_>,
    cancel: &CancelToken,
) -> Result<Propagation, Cancelled> {
    let graphs = Graphs::new(input.base, input.head);
    let mut search = Search::new(input, &graphs);
    search.seed(cancel)?;
    search.run(cancel)?;
    Ok(search.finish())
}

/// One of the two layers of the search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// Paths of resolved steps only.
    Resolved = 0,
    /// Paths of any certainty.
    Any = 1,
}

/// A change's first step, as a path to a head item.
struct Seed {
    /// Index of the change.
    change: u32,
    /// The step itself.
    reach: Reach,
    /// The item it reaches.
    item: SymbolId,
    /// The head symbols the step starts from; no path of this seed comes back
    /// through them.
    anchors: Vec<SymbolId>,
    /// The certainty of the whole first step, `via` included.
    certainty: Certainty,
    /// How many steps it takes: one, or two with a `via`.
    steps: u32,
}

/// How a state was first reached.
#[derive(Debug, Clone, Copy)]
enum Parent {
    /// By a change's first step.
    Seed(u32),
    /// By a behaviour step from a state of another item.
    Step {
        /// The item the step starts from.
        from: SymbolId,
        /// The layer of that item's state.
        layer: Layer,
        /// The head edge followed.
        edge: u32,
        /// How certain the step is.
        certainty: Certainty,
    },
}

/// The shortest path to an item on one layer.
#[derive(Debug, Clone, Copy)]
struct Label {
    /// How many steps it takes.
    steps: u32,
    /// Its certainty: resolved only when every step is.
    certainty: Certainty,
    /// The seed it starts at.
    seed: u32,
    /// Where it came from.
    parent: Parent,
}

/// One way a path arrives at an item.
#[derive(Debug, Clone, Copy)]
enum Arrival {
    /// A change's first step reaches it.
    Seed(u32),
    /// A behaviour step from another reached item.
    Step {
        /// That item.
        from: SymbolId,
        /// The head edge.
        edge: u32,
        /// How certain the step is.
        certainty: Certainty,
    },
}

/// The best path one arrival offers.
#[derive(Debug, Clone, Copy)]
struct Offer {
    /// The class it gives the item.
    class: ImpactClass,
    /// Its certainty.
    certainty: Certainty,
    /// How many steps it takes.
    steps: u32,
    /// For a step, the layer of the state it extends.
    layer: Layer,
    /// The arrival it belongs to.
    arrival: Arrival,
}

impl Offer {
    /// The ranking key: higher class, then higher certainty, then fewer steps.
    fn key(&self) -> (u8, u8, u32) {
        (
            class_rank(self.class),
            certainty_rank(self.certainty),
            self.steps,
        )
    }
}

/// The state of one search.
struct Search<'a> {
    /// What the search reads.
    input: &'a ImpactInput<'a>,
    /// The two graphs and their lookup tables.
    graphs: &'a Graphs<'a>,
    /// The first steps of every change.
    seeds: Vec<Seed>,
    /// The label of every head symbol on each layer.
    labels: Vec<[Option<Label>; 2]>,
    /// The ways paths arrive at every head symbol.
    arrivals: Vec<Vec<Arrival>>,
    /// The states waiting to be expanded, by their number of steps.
    queue: Vec<Vec<(SymbolId, Layer)>>,
}

impl<'a> Search<'a> {
    /// A search that has not looked at the changes yet.
    fn new(input: &'a ImpactInput<'a>, graphs: &'a Graphs<'a>) -> Search<'a> {
        let count = input.head.symbols.len();
        Search {
            input,
            graphs,
            seeds: Vec::new(),
            labels: vec![[None, None]; count],
            arrivals: vec![Vec::new(); count],
            queue: Vec::new(),
        }
    }

    /// Takes the first step of every change.
    fn seed(&mut self, cancel: &CancelToken) -> Result<(), Cancelled> {
        let renamed = renamed_changes(self.input.changes);
        for (index, change) in self.input.changes.iter().enumerate() {
            cancel.check()?;
            for reach in rules::first_steps(change, self.graphs) {
                if let Some(seed) = self.make_seed(index as u32, reach, &renamed) {
                    let id = self.seeds.len() as u32;
                    let label = Label {
                        steps: seed.steps,
                        certainty: seed.certainty,
                        seed: id,
                        parent: Parent::Seed(id),
                    };
                    let item = seed.item;
                    self.seeds.push(seed);
                    self.arrivals[item.0 as usize].push(Arrival::Seed(id));
                    self.offer(item, Layer::Any, label);
                    if label.certainty == Certainty::Resolved {
                        self.offer(item, Layer::Resolved, label);
                    }
                }
            }
        }
        Ok(())
    }

    /// The seed of one first step, when its item exists on the head side.
    fn make_seed(
        &self,
        change: u32,
        reach: Reach,
        renamed: &BTreeMap<SymbolPath, SymbolPath>,
    ) -> Option<Seed> {
        let graph = self.graphs.graph(reach.side);
        let edge = graph.edges.get(reach.edge as usize)?;
        let item = self.head_counterpart(reach.side, edge.from, renamed)?;
        let mut anchors = vec![edge.to];
        let mut certainty = reach.certainty;
        if let Some(via) = reach.via {
            let step = graph.edges.get(via.edge as usize)?;
            anchors.push(step.to);
            certainty = both(certainty, via.certainty);
        }
        let anchors = anchors
            .into_iter()
            .filter_map(|anchor| self.head_counterpart(reach.side, anchor, renamed))
            .collect();
        Some(Seed {
            change,
            reach,
            item,
            anchors,
            certainty,
            steps: 1 + u32::from(reach.via.is_some()),
        })
    }

    /// The head symbol of a symbol read on `side`: the same identity, or the
    /// head side of a rename.
    fn head_counterpart(
        &self,
        side: Side,
        id: SymbolId,
        renamed: &BTreeMap<SymbolPath, SymbolPath>,
    ) -> Option<SymbolId> {
        let head = &self.graphs.head_index;
        match side {
            Side::Head => self.input.head.symbols.get(id.0 as usize).map(|_| id),
            Side::Base => {
                let path = &self.input.base.symbols.get(id.0 as usize)?.path;
                head.by_path(path)
                    .or_else(|| head.by_path(renamed.get(path)?))
            }
        }
    }

    /// Gives `item` the label on `layer` when it has none or a longer one, and
    /// queues the state.
    fn offer(&mut self, item: SymbolId, layer: Layer, label: Label) {
        let slot = &mut self.labels[item.0 as usize][layer as usize];
        if slot.is_some_and(|old| old.steps <= label.steps) {
            return;
        }
        *slot = Some(label);
        let steps = label.steps as usize;
        if self.queue.len() <= steps {
            self.queue.resize_with(steps + 1, Vec::new);
        }
        self.queue[steps].push((item, layer));
    }

    /// Expands every state, fewest steps first.
    fn run(&mut self, cancel: &CancelToken) -> Result<(), Cancelled> {
        let mut steps = 0;
        while steps < self.queue.len() {
            let bucket = std::mem::take(&mut self.queue[steps]);
            for (item, layer) in bucket {
                cancel.check()?;
                let Some(label) = self.labels[item.0 as usize][layer as usize] else {
                    continue;
                };
                // A state that a shorter seed replaced was queued twice.
                if label.steps as usize == steps {
                    self.expand(item, layer, label);
                }
            }
            steps += 1;
        }
        Ok(())
    }

    /// Follows the behaviour steps out of one state.
    fn expand(&mut self, item: SymbolId, layer: Layer, label: Label) {
        for reach in rules::onward(item, self.graphs) {
            let Some(edge) = self.input.head.edges.get(reach.edge as usize) else {
                continue;
            };
            let next = edge.from;
            if layer == Layer::Any {
                self.arrivals[next.0 as usize].push(Arrival::Step {
                    from: item,
                    edge: reach.edge,
                    certainty: reach.certainty,
                });
            }
            if self.seeds[label.seed as usize].anchors.contains(&next) {
                continue;
            }
            if layer == Layer::Resolved && reach.certainty != Certainty::Resolved {
                continue;
            }
            let parent = Parent::Step {
                from: item,
                layer,
                edge: reach.edge,
                certainty: reach.certainty,
            };
            self.offer(
                next,
                layer,
                Label {
                    steps: label.steps + 1,
                    certainty: both(label.certainty, reach.certainty),
                    seed: label.seed,
                    parent,
                },
            );
        }
    }

    /// The paths `arrival` offers `item` within `limit` steps.
    ///
    /// A step offers two when the item it starts from has both a resolved
    /// path and a shorter candidate one: the first ranks higher, the second
    /// may come from another change and is worth showing as evidence too.
    fn offers_of(&self, item: SymbolId, arrival: Arrival, limit: u32) -> Vec<Offer> {
        match arrival {
            Arrival::Seed(id) => {
                let seed = &self.seeds[id as usize];
                let offer = Offer {
                    class: seed.reach.class,
                    certainty: seed.certainty,
                    steps: seed.steps,
                    layer: Layer::Any,
                    arrival,
                };
                (seed.steps <= limit).then_some(offer).into_iter().collect()
            }
            Arrival::Step {
                from, certainty, ..
            } => {
                let [resolved, any] = self.labels[from.0 as usize];
                // A resolved shortest path is the resolved layer's path again.
                let any = any.filter(|label| label.certainty == Certainty::Candidate);
                [(Layer::Resolved, resolved), (Layer::Any, any)]
                    .into_iter()
                    .filter_map(|(layer, label)| {
                        let label = label?;
                        let blocked = self.seeds[label.seed as usize].anchors.contains(&item);
                        (!blocked && label.steps < limit).then_some(Offer {
                            class: ImpactClass::Behaviour,
                            certainty: both(label.certainty, certainty),
                            steps: label.steps + 1,
                            layer,
                            arrival,
                        })
                    })
                    .collect()
            }
        }
    }

    /// The paths every arrival offers `item` within `limit` steps, best first.
    ///
    /// An item a macro or a derive produced has no text of its own to edit, so
    /// it never takes the class of an adaptation; it still carries behaviour on.
    fn offers(&self, item: SymbolId, limit: u32) -> Vec<Offer> {
        let from_macro = self
            .input
            .head
            .symbols
            .get(item.0 as usize)
            .is_some_and(|symbol| symbol.flags.from_macro);
        let mut offers: Vec<Offer> = self.arrivals[item.0 as usize]
            .iter()
            .flat_map(|&arrival| self.offers_of(item, arrival, limit))
            .filter(|offer| !(from_macro && offer.class == ImpactClass::Adaptation))
            .collect();
        offers.sort_by_key(Offer::key);
        offers
    }

    /// The steps of the path `offer` describes, starting next to the change.
    fn path(&self, item: SymbolId, offer: &Offer) -> EvidencePath {
        let mut reversed = Vec::new();
        let mut parent = match offer.arrival {
            Arrival::Seed(id) => Parent::Seed(id),
            Arrival::Step {
                from,
                edge,
                certainty,
            } => Parent::Step {
                from,
                layer: offer.layer,
                edge,
                certainty,
            },
        };
        let mut current = item;
        let seed = loop {
            match parent {
                Parent::Seed(id) => break id,
                Parent::Step {
                    from,
                    layer,
                    edge,
                    certainty,
                } => {
                    reversed.extend(self.head_step(current, edge, certainty));
                    parent = self.labels[from.0 as usize][layer as usize]
                        .expect("a state is only a parent once it has a label")
                        .parent;
                    current = from;
                }
            }
        };
        let seed = &self.seeds[seed as usize];
        let mut steps = self.seed_steps(seed);
        steps.extend(reversed.into_iter().rev());
        EvidencePath {
            change: seed.change,
            reason: seed.reach.reason,
            class: offer.class,
            steps,
        }
    }

    /// One behaviour step on the head side, reaching `item`.
    fn head_step(&self, item: SymbolId, edge: u32, certainty: Certainty) -> Option<EvidenceStep> {
        let head = self.input.head;
        let edge = head.edges.get(edge as usize)?;
        Some(EvidenceStep {
            from: head.symbols.get(item.0 as usize)?.path.clone(),
            to: head.symbols.get(edge.to.0 as usize)?.path.clone(),
            edge: edge.kind,
            certainty,
            detail: edge.detail,
            site: edge.site.clone(),
        })
    }

    /// The steps of a seed: the `via` step, if any, then the step that reaches
    /// the item, which names it by its head identity.
    fn seed_steps(&self, seed: &Seed) -> Vec<EvidenceStep> {
        let side = seed.reach.side;
        let graph = self.graphs.graph(side);
        let path = |id: SymbolId| graph.symbols.get(id.0 as usize).map(|s| s.path.clone());
        let mut steps = Vec::new();
        if let Some(via) = seed.reach.via
            && let Some(edge) = graph.edges.get(via.edge as usize)
            && let (Some(from), Some(to)) = (path(edge.from), path(edge.to))
        {
            let site = match side {
                Side::Head => edge.site.clone(),
                Side::Base => match self.graphs.head_index.by_path(&from) {
                    Some(head) => self.head_site(head, edge),
                    None => edge.site.clone(),
                },
            };
            steps.push(EvidenceStep {
                from,
                to,
                edge: edge.kind,
                certainty: via.certainty,
                detail: edge.detail,
                site,
            });
        }
        let item = self.input.head.symbols.get(seed.item.0 as usize);
        if let Some(edge) = graph.edges.get(seed.reach.edge as usize)
            && let (Some(item), Some(to)) = (item, path(edge.to))
        {
            let site = match side {
                Side::Head => edge.site.clone(),
                Side::Base => self.head_site(seed.item, edge),
            };
            steps.push(EvidenceStep {
                from: item.path.clone(),
                to,
                edge: edge.kind,
                certainty: seed.reach.certainty,
                detail: edge.detail,
                site,
            });
        }
        steps
    }

    /// Where the reference of the base-side `edge` is written in the head
    /// symbol `from`.
    ///
    /// The evidence is shown in the files of the head side, where the lines of
    /// the base side may have moved. The reference is the head edge of the same
    /// kind to a target of the same name, or an unresolved reference of that
    /// name inside `from`, since its target may be gone; failing both, `from`
    /// itself.
    fn head_site(&self, from: SymbolId, edge: &Edge) -> Location {
        let head = self.input.head;
        let Some(symbol) = head.symbols.get(from.0 as usize) else {
            return edge.site.clone();
        };
        let Some(target) = self.input.base.symbols.get(edge.to.0 as usize) else {
            return symbol.location.clone();
        };
        let name = last_segment(&target.path.path);
        let same = self
            .graphs
            .head_index
            .edges_from(from)
            .iter()
            .filter_map(|&index| head.edges.get(index as usize))
            .find(|candidate| {
                candidate.kind == edge.kind
                    && head
                        .symbols
                        .get(candidate.to.0 as usize)
                        .is_some_and(|to| last_segment(&to.path.path) == name)
            });
        if let Some(same) = same {
            return same.site.clone();
        }
        let span = &symbol.location.span;
        head.unresolved
            .iter()
            .find(|reference| {
                reference.location.file == symbol.location.file
                    && span.start <= reference.location.span.start
                    && reference.location.span.end <= span.end
                    && last_segment(&reference.text) == name
            })
            .map_or_else(
                || symbol.location.clone(),
                |reference| reference.location.clone(),
            )
    }

    /// Turns the search into its result.
    fn finish(self) -> Propagation {
        let params = self.input.params;
        let head = self.input.head;
        let changed = changed_symbols(self.input.changes);

        let mut items: Vec<ImpactItem> = Vec::new();
        let mut tests = HashSet::new();
        let mut below_confidence = 0u32;
        let mut ignored = 0u32;
        for (index, symbol) in head.symbols.iter().enumerate() {
            let id = SymbolId(index as u32);
            if self.labels[index][Layer::Any as usize].is_none() {
                continue;
            }
            let wanted = |offer: &Offer| {
                confidence(offer.class, offer.certainty) >= params.min_confidence
                    && !self.input.ignore.is_ignored(&symbol.location.file)
            };
            if symbol.flags.is_test && self.offers(id, u32::MAX).first().is_some_and(wanted) {
                tests.insert(id);
            }
            let offers = self.offers(id, params.step_limit());
            let Some(best) = offers.first() else {
                continue;
            };
            let confidence = confidence(best.class, best.certainty);
            if confidence < params.min_confidence {
                below_confidence += 1;
                continue;
            }
            if self.input.ignore.is_ignored(&symbol.location.file) {
                ignored += 1;
                continue;
            }
            items.push(ImpactItem {
                symbol: symbol.path.clone(),
                location: symbol.location.clone(),
                class: best.class,
                confidence,
                distance: best.steps,
                paths: offers
                    .iter()
                    .take(KEPT_PATHS)
                    .map(|offer| self.path(id, offer))
                    .collect(),
                path_count: offers.len() as u32,
                tags: ImpactTags {
                    is_test: symbol.flags.is_test,
                    is_main: symbol.flags.is_main,
                    is_pub_api: symbol.flags.is_pub_api,
                    changed: changed.contains(&symbol.path),
                },
            });
        }
        items.sort_by(|left, right| {
            (
                class_rank(left.class),
                right.confidence,
                left.distance,
                &left.symbol,
            )
                .cmp(&(
                    class_rank(right.class),
                    left.confidence,
                    right.distance,
                    &right.symbol,
                ))
        });

        let mut candidate_steps: BTreeSet<StepKey> = BTreeSet::new();
        for path in items.iter().flat_map(|item| &item.paths) {
            for step in &path.steps {
                if step.certainty == Certainty::Candidate {
                    candidate_steps.insert(candidate_step(step));
                }
            }
        }
        let coarse_rules = self
            .seeds
            .iter()
            .filter(|seed| seed.reach.coarse)
            .map(|seed| seed.item)
            .collect::<BTreeSet<_>>()
            .len() as u32;

        let unresolved_in_reach = count_unresolved_in_reach(self.input.changes, &items, head);
        Propagation {
            items,
            tests,
            limits: ImpactLimits {
                unresolved_total: head.unresolved.len() as u32,
                unresolved_in_reach,
                candidate_steps: candidate_steps.len() as u32,
                coarse_rules,
                below_confidence,
                ignored,
            },
        }
    }
}

/// The certainty of two steps taken one after the other.
fn both(left: Certainty, right: Certainty) -> Certainty {
    if left == Certainty::Resolved && right == Certainty::Resolved {
        Certainty::Resolved
    } else {
        Certainty::Candidate
    }
}

/// Adaptation ranks before behaviour.
fn class_rank(class: ImpactClass) -> u8 {
    match class {
        ImpactClass::Adaptation => 0,
        ImpactClass::Behaviour => 1,
    }
}

/// A resolved path ranks before a candidate one.
fn certainty_rank(certainty: Certainty) -> u8 {
    match certainty {
        Certainty::Resolved => 0,
        Certainty::Candidate => 1,
    }
}

/// The confidence a path of this class and certainty gives its item.
fn confidence(class: ImpactClass, certainty: Certainty) -> Confidence {
    match (certainty, class) {
        (Certainty::Candidate, _) => Confidence::Possible,
        (Certainty::Resolved, ImpactClass::Adaptation) => Confidence::Definite,
        (Certainty::Resolved, ImpactClass::Behaviour) => Confidence::Likely,
    }
}

/// The unresolved references written inside a changed symbol or a listed item.
///
/// Items nest or are disjoint, so the regions of each file are merged into
/// disjoint spans first, and each reference is looked up by binary search.
fn count_unresolved_in_reach(
    changes: &[Change],
    items: &[ImpactItem],
    head: &blastr_model::RevisionGraph,
) -> u32 {
    let mut regions: HashMap<&RelPath, Vec<(u32, u32)>> = HashMap::new();
    let locations = changes
        .iter()
        .filter_map(|change| change.new.as_ref())
        .chain(items.iter().map(|item| &item.location));
    for Location { file, span } in locations {
        regions
            .entry(file)
            .or_default()
            .push((span.start, span.end));
    }
    for spans in regions.values_mut() {
        spans.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(spans.len());
        for &(start, end) in spans.iter() {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        *spans = merged;
    }
    let inside = |file: &RelPath, span: &Span| {
        let Some(spans) = regions.get(file) else {
            return false;
        };
        let after = spans.partition_point(|&(start, _)| start <= span.start);
        after > 0 && span.end <= spans[after - 1].1
    };
    head.unresolved
        .iter()
        .filter(|reference| inside(&reference.location.file, &reference.location.span))
        .count() as u32
}

/// The last segment of a path: the name of what it names.
fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// The head identities of the changes that make an item count as changed.
fn changed_symbols(changes: &[Change]) -> BTreeSet<&SymbolPath> {
    changes
        .iter()
        .filter(|change| !matches!(change.kind, ChangeKind::NonSemantic | ChangeKind::Manifest))
        .map(|change| &change.symbol)
        .collect()
}

/// The head identities of renamed symbols, by their base identity.
fn renamed_changes(changes: &[Change]) -> BTreeMap<SymbolPath, SymbolPath> {
    changes
        .iter()
        .filter_map(|change| match &change.detail {
            ChangeDetail::Renamed { from } => Some((from.clone(), change.symbol.clone())),
            _ => None,
        })
        .collect()
}

/// Identifies a candidate step: the same reference to the same target.
type StepKey = (SymbolPath, SymbolPath, &'static str, RelPath, u32, u32);

/// The key that counts one candidate step.
fn candidate_step(step: &EvidenceStep) -> StepKey {
    (
        step.from.clone(),
        step.to.clone(),
        step.edge.as_str(),
        step.site.file.clone(),
        step.site.span.start,
        step.site.span.end,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::rules::testkit::{GraphBuilder, change, detailed, member};
    use blastr_model::{
        AnalysisParams, ChangeDetail, ChangeKind, Delta, EdgeDetail, EdgeKind, IgnoreSet, Reason,
        RevisionGraph, SymbolKind, SymbolPath, Visibility,
    };

    /// An empty graph, for changes that read one side only.
    fn nothing() -> RevisionGraph {
        RevisionGraph::default()
    }

    /// Runs the propagation over two graphs.
    fn run(
        base: &RevisionGraph,
        head: &RevisionGraph,
        changes: &[Change],
        params: &AnalysisParams,
        ignore: &IgnoreSet,
    ) -> Propagation {
        let input = ImpactInput {
            base,
            head,
            changes,
            params,
            ignore,
        };
        propagate(&input, &CancelToken::new()).expect("the run is not cancelled")
    }

    /// Runs with the default parameters and no ignored files.
    fn analyse(base: &RevisionGraph, head: &RevisionGraph, changes: &[Change]) -> Propagation {
        run(
            base,
            head,
            changes,
            &AnalysisParams::default(),
            &IgnoreSet::empty(),
        )
    }

    /// The items as (path, confidence, distance), in order.
    fn summary(propagation: &Propagation) -> Vec<(&str, Confidence, u32)> {
        propagation
            .items
            .iter()
            .map(|item| (item.symbol.path.as_str(), item.confidence, item.distance))
            .collect()
    }

    /// The item at `path`, which must be listed.
    fn item<'p>(propagation: &'p Propagation, path: &str) -> &'p ImpactItem {
        propagation
            .items
            .iter()
            .find(|item| item.symbol.path == path)
            .unwrap_or_else(|| panic!("`{path}` is not listed"))
    }

    #[test]
    fn a_call_chain_is_listed_nearest_first() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let callee = builder.child(module, "krate::f", SymbolKind::Function);
        let first = builder.child(module, "krate::i", SymbolKind::Function);
        let second = builder.child(module, "krate::h", SymbolKind::Function);
        let third = builder.child(module, "krate::g", SymbolKind::Function);
        builder.edge(first, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(second, first, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(third, second, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(
            summary(&result),
            vec![
                ("krate::i", Confidence::Likely, 1),
                ("krate::h", Confidence::Likely, 2),
                ("krate::g", Confidence::Likely, 3),
            ]
        );
        assert!(
            result
                .items
                .iter()
                .all(|item| item.class == ImpactClass::Behaviour)
        );
        let path = &item(&result, "krate::g").paths[0];
        let steps: Vec<(&str, &str)> = path
            .steps
            .iter()
            .map(|step| (step.from.path.as_str(), step.to.path.as_str()))
            .collect();
        assert_eq!(
            steps,
            vec![
                ("krate::i", "krate::f"),
                ("krate::h", "krate::i"),
                ("krate::g", "krate::h"),
            ]
        );
        assert_eq!(path.reason, Reason::CallsChanged);
    }

    #[test]
    fn a_candidate_step_caps_the_confidence() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(
            summary(&result),
            vec![("krate::holder", Confidence::Possible, 1)]
        );
        assert_eq!(result.limits.candidate_steps, 1);
    }

    #[test]
    fn an_item_reached_by_an_adaptation_and_a_behaviour_path_is_an_adaptation() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let signature = builder.child(module, "krate::f", SymbolKind::Function);
        let body = builder.child(module, "krate::k", SymbolKind::Function);
        let caller = builder.child(module, "krate::g", SymbolKind::Function);
        builder.edge(caller, signature, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(caller, body, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![
            change(ChangeKind::Body, "krate::k", SymbolKind::Function),
            change(ChangeKind::Signature, "krate::f", SymbolKind::Function),
        ];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(result.items.len(), 1);
        let item = &result.items[0];
        assert_eq!(item.class, ImpactClass::Adaptation);
        assert_eq!(item.confidence, Confidence::Definite);
        assert_eq!(item.distance, 1);
        assert_eq!(item.path_count, 2);
        assert_eq!(item.paths.len(), 2);
        assert_eq!(
            item.paths[0].class,
            ImpactClass::Adaptation,
            "the best path comes first"
        );
    }

    #[test]
    fn items_are_ordered_by_class_confidence_distance_and_name() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let signature = builder.child(module, "krate::f", SymbolKind::Function);
        let body = builder.child(module, "krate::k", SymbolKind::Function);
        let adapting = builder.child(module, "krate::z_adapts", SymbolKind::Function);
        let near = builder.child(module, "krate::b_near", SymbolKind::Function);
        let far = builder.child(module, "krate::a_far", SymbolKind::Function);
        let holder = builder.child(module, "krate::a_holder", SymbolKind::Function);
        builder.edge(adapting, signature, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(near, body, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(far, near, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, body, EdgeKind::TakesAsValue, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![
            change(ChangeKind::Signature, "krate::f", SymbolKind::Function),
            change(ChangeKind::Body, "krate::k", SymbolKind::Function),
        ];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(
            summary(&result),
            vec![
                ("krate::z_adapts", Confidence::Definite, 1),
                ("krate::b_near", Confidence::Likely, 1),
                ("krate::a_far", Confidence::Likely, 2),
                ("krate::a_holder", Confidence::Possible, 1),
            ]
        );
    }

    #[test]
    fn the_depth_limits_the_list_but_not_the_tests() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let callee = builder.child(module, "krate::f", SymbolKind::Function);
        let first = builder.child(module, "krate::g", SymbolKind::Function);
        let test = builder.child(module, "krate::test", SymbolKind::Function);
        builder.symbol(test).flags.is_test = true;
        builder.edge(first, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(test, first, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];
        let params = AnalysisParams {
            depth: 1,
            ..AnalysisParams::default()
        };

        let result = run(&nothing(), &head, &changes, &params, &IgnoreSet::empty());

        assert_eq!(summary(&result), vec![("krate::g", Confidence::Likely, 1)]);
        assert_eq!(result.tests, HashSet::from([test]));

        // A depth of zero lists every distance.
        let unlimited = analyse(&nothing(), &head, &changes);
        assert_eq!(
            summary(&unlimited),
            vec![
                ("krate::g", Confidence::Likely, 1),
                ("krate::test", Confidence::Likely, 2),
            ]
        );
    }

    #[test]
    fn a_resolved_path_beats_a_shorter_candidate_one_within_the_depth() {
        // `hub` is reached by a resolved chain of eight steps and by a chain of
        // five whose last step is a candidate. The test calls `hub`: nine
        // resolved steps away, six through the candidate.
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let callee = builder.child(module, "krate::f", SymbolKind::Function);
        let long: Vec<SymbolId> = (1..=7)
            .map(|step| builder.child(module, &format!("krate::long{step}"), SymbolKind::Function))
            .collect();
        builder.edge(long[0], callee, EdgeKind::Calls, EdgeDetail::None);
        for pair in long.windows(2) {
            builder.edge(pair[1], pair[0], EdgeKind::Calls, EdgeDetail::None);
        }
        let short: Vec<SymbolId> = (1..=4)
            .map(|step| builder.child(module, &format!("krate::short{step}"), SymbolKind::Function))
            .collect();
        builder.edge(short[0], callee, EdgeKind::Calls, EdgeDetail::None);
        for pair in short.windows(2) {
            builder.edge(pair[1], pair[0], EdgeKind::Calls, EdgeDetail::None);
        }
        let hub = builder.child(module, "krate::hub", SymbolKind::Function);
        builder.edge(hub, long[6], EdgeKind::Calls, EdgeDetail::None);
        builder.candidate(hub, short[3], EdgeKind::TakesAsValue, EdgeDetail::None);
        let test = builder.child(module, "krate::test", SymbolKind::Function);
        builder.symbol(test).flags.is_test = true;
        builder.edge(test, hub, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];

        let eight = AnalysisParams {
            depth: 8,
            ..AnalysisParams::default()
        };
        let result = run(&nothing(), &head, &changes, &eight, &IgnoreSet::empty());

        let hub_item = item(&result, "krate::hub");
        assert_eq!(hub_item.distance, 8);
        assert_eq!(hub_item.confidence, Confidence::Likely);
        assert_eq!(hub_item.path_count, 2);
        assert_eq!(hub_item.paths[1].steps.len(), 5);
        // Within a depth of eight only the candidate path reaches it.
        let test_item = item(&result, "krate::test");
        assert_eq!(test_item.distance, 6);
        assert_eq!(test_item.confidence, Confidence::Possible);
        assert!(result.tests.contains(&test));

        let params = AnalysisParams {
            depth: 9,
            ..AnalysisParams::default()
        };
        let deeper = run(&nothing(), &head, &changes, &params, &IgnoreSet::empty());
        let test_item = item(&deeper, "krate::test");
        assert_eq!(test_item.distance, 9);
        assert_eq!(test_item.confidence, Confidence::Likely);
    }

    #[test]
    fn the_confidence_filter_counts_what_it_hides() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        builder.symbol(holder).flags.is_test = true;
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];
        let params = AnalysisParams {
            min_confidence: Confidence::Likely,
            ..AnalysisParams::default()
        };

        let result = run(&nothing(), &head, &changes, &params, &IgnoreSet::empty());

        assert_eq!(
            summary(&result),
            vec![("krate::caller", Confidence::Likely, 1)]
        );
        assert_eq!(result.limits.below_confidence, 1);
        assert!(result.tests.is_empty(), "the filter applies to tests too");
    }

    #[test]
    fn the_ignore_set_counts_the_files_it_hides() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let generated = builder.child(module, "krate::generated", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.file(generated, "src/gen/out.rs");
        builder.edge(generated, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];
        let ignore = IgnoreSet::new(&["src/gen/**".to_owned()]).expect("the pattern compiles");

        let result = run(
            &nothing(),
            &head,
            &changes,
            &AnalysisParams::default(),
            &ignore,
        );

        assert_eq!(
            summary(&result),
            vec![("krate::caller", Confidence::Likely, 1)]
        );
        assert_eq!(result.limits.ignored, 1);
    }

    #[test]
    fn the_result_does_not_depend_on_the_run() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        let dispatcher = builder.child(module, "krate::dispatcher", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        builder.edge(
            dispatcher,
            function,
            EdgeKind::MayDispatchTo,
            EdgeDetail::None,
        );
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];

        let first = analyse(&nothing(), &head, &changes);
        let second = analyse(&nothing(), &head, &changes);

        assert_eq!(first, second);
        assert_eq!(
            summary(&first),
            vec![
                ("krate::caller", Confidence::Likely, 1),
                ("krate::dispatcher", Confidence::Possible, 1),
                ("krate::holder", Confidence::Possible, 1),
            ]
        );
    }

    #[test]
    fn a_removed_change_reaches_a_caller_through_the_base_graph() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let gone = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, gone, EdgeKind::Calls, EdgeDetail::None);
        let base = builder.finish();

        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        builder.child(module, "krate::caller", SymbolKind::Function);
        let head = builder.finish();
        let changes = vec![change(
            ChangeKind::Removed,
            "krate::f",
            SymbolKind::Function,
        )];

        let result = analyse(&base, &head, &changes);

        assert_eq!(
            summary(&result),
            vec![("krate::caller", Confidence::Definite, 1)]
        );
        let item = &result.items[0];
        assert_eq!(item.class, ImpactClass::Adaptation);
        let step = &item.paths[0].steps[0];
        assert_eq!(step.from.path, "krate::caller");
        assert_eq!(step.to.path, "krate::f");
        assert_eq!(step.edge, EdgeKind::Calls);
    }

    /// A location on `line` of `src/lib.rs`, whose bytes are the line number.
    fn on_line(line: u32) -> Location {
        Location {
            file: RelPath::new("src/lib.rs"),
            span: Span {
                start: line,
                end: line + 1,
                start_line: line,
                end_line: line,
            },
        }
    }

    #[test]
    fn a_base_side_step_is_located_where_the_head_writes_the_reference() {
        // `Failed(Failure)`: the field's type moved to another module, and
        // the lines above the field changed, so the base line is not the head
        // line.
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let gone = builder.child(module, "krate::Failure", SymbolKind::Struct);
        let field = builder.child(module, "krate::Loadable::Failed::0", SymbolKind::Field);
        builder.edge(field, gone, EdgeKind::UsesType, EdgeDetail::None);
        let mut base = builder.finish();
        base.edges[0].site = on_line(92);

        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let moved = builder.child(module, "krate::model::Failure", SymbolKind::Struct);
        let field = builder.child(module, "krate::Loadable::Failed::0", SymbolKind::Field);
        builder.symbol(field).location = on_line(70);
        builder.edge(field, moved, EdgeKind::UsesType, EdgeDetail::None);
        let mut head = builder.finish();
        head.edges[0].site = on_line(70);
        let changes = vec![change(
            ChangeKind::Removed,
            "krate::Failure",
            SymbolKind::Struct,
        )];

        let result = analyse(&base, &head, &changes);

        let step = &item(&result, "krate::Loadable::Failed::0").paths[0].steps[0];
        assert_eq!(step.to.path, "krate::Failure");
        assert_eq!(step.site, on_line(70));
    }

    #[test]
    fn a_base_side_step_to_a_removed_function_is_located_at_the_unresolved_call() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let gone = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, gone, EdgeKind::Calls, EdgeDetail::None);
        let mut base = builder.finish();
        base.edges[0].site = on_line(5);

        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.symbol(caller).location.span = Span {
            start: 0,
            end: 100,
            start_line: 1,
            end_line: 20,
        };
        let mut head = builder.finish();
        head.unresolved.push(blastr_model::UnresolvedRef {
            location: on_line(9),
            text: "f".to_owned(),
        });
        let changes = vec![change(
            ChangeKind::Removed,
            "krate::f",
            SymbolKind::Function,
        )];

        let result = analyse(&base, &head, &changes);

        assert_eq!(
            item(&result, "krate::caller").paths[0].steps[0].site,
            on_line(9)
        );
    }

    #[test]
    fn a_base_side_step_reports_the_head_identity_of_a_renamed_caller() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let gone = builder.child(module, "krate::f", SymbolKind::Function);
        let old = builder.child(module, "krate::old", SymbolKind::Function);
        builder.edge(old, gone, EdgeKind::Calls, EdgeDetail::None);
        let base = builder.finish();

        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        builder.child(module, "krate::new", SymbolKind::Function);
        let head = builder.finish();

        let changes = vec![
            detailed(
                ChangeKind::Renamed,
                "krate::new",
                SymbolKind::Function,
                ChangeDetail::Renamed {
                    from: SymbolPath::new("krate::old", SymbolKind::Function),
                },
            ),
            change(ChangeKind::Removed, "krate::f", SymbolKind::Function),
        ];

        let result = analyse(&base, &head, &changes);

        assert_eq!(result.items.len(), 1);
        let item = &result.items[0];
        assert_eq!(item.symbol.path, "krate::new");
        assert_eq!(item.paths[0].steps[0].from.path, "krate::new");
        assert_eq!(item.paths[0].steps[0].to.path, "krate::f");
    }

    #[test]
    fn a_call_cycle_terminates_without_coming_back_to_the_change() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let first = builder.child(module, "krate::a", SymbolKind::Function);
        let second = builder.child(module, "krate::b", SymbolKind::Function);
        builder.edge(first, second, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(second, first, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::a", SymbolKind::Function)];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(summary(&result), vec![("krate::b", Confidence::Likely, 1)]);
    }

    #[test]
    fn an_item_that_is_itself_changed_is_tagged() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let callee = builder.child(module, "krate::f", SymbolKind::Function);
        let middle = builder.child(module, "krate::g", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(middle, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(caller, middle, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![
            change(ChangeKind::Body, "krate::f", SymbolKind::Function),
            change(ChangeKind::Body, "krate::g", SymbolKind::Function),
        ];

        let result = analyse(&nothing(), &head, &changes);

        assert!(item(&result, "krate::g").tags.changed);
        assert!(!item(&result, "krate::caller").tags.changed);
    }

    #[test]
    fn an_item_keeps_three_paths_and_counts_them_all() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let callee = builder.child(module, "krate::f", SymbolKind::Function);
        let once = builder.child(module, "krate::g1", SymbolKind::Function);
        let twice = builder.child(module, "krate::g2", SymbolKind::Function);
        let thrice = builder.child(module, "krate::g3", SymbolKind::Function);
        let middle = builder.child(module, "krate::m", SymbolKind::Function);
        builder.edge(once, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(twice, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(thrice, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(middle, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(middle, once, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(middle, twice, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(middle, thrice, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];

        let result = analyse(&nothing(), &head, &changes);

        let middle = item(&result, "krate::m");
        assert_eq!(middle.path_count, 4);
        assert_eq!(middle.distance, 1);
        let lengths: Vec<usize> = middle.paths.iter().map(|path| path.steps.len()).collect();
        assert_eq!(lengths, vec![1, 2, 2], "the best path first");
    }

    #[test]
    fn a_formatting_only_change_reaches_nothing() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(
            ChangeKind::NonSemantic,
            "krate::f",
            SymbolKind::Function,
        )];

        let result = analyse(&nothing(), &head, &changes);

        assert!(result.items.is_empty());
        assert!(result.tests.is_empty());
        assert_eq!(result.limits, ImpactLimits::default());
    }

    #[test]
    fn a_narrowed_visibility_counts_its_first_steps_as_coarse() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let inner = builder.child(module, "krate::inner", SymbolKind::Module);
        let deeper = builder.child(inner, "krate::inner::deeper", SymbolKind::Module);
        let hidden = builder.child(deeper, "krate::inner::deeper::hidden", SymbolKind::Struct);
        let cousin = builder.child(inner, "krate::inner::cousin", SymbolKind::Function);
        let outside = builder.child(module, "krate::outside", SymbolKind::Function);
        builder.edge(cousin, hidden, EdgeKind::UsesType, EdgeDetail::None);
        builder.edge(outside, hidden, EdgeKind::UsesType, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![detailed(
            ChangeKind::Visibility,
            "krate::inner::deeper::hidden",
            SymbolKind::Struct,
            ChangeDetail::Visibility {
                from: Visibility::Public,
                to: Visibility::Restricted,
            },
        )];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(
            summary(&result),
            vec![("krate::outside", Confidence::Definite, 1)]
        );
        assert_eq!(result.limits.coarse_rules, 1);
    }

    #[test]
    fn a_use_of_a_const_counts_one_candidate_step() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let constant = builder.child(module, "krate::LIMIT", SymbolKind::Const);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.candidate(reader, constant, EdgeKind::UsesValue, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(
            ChangeKind::ConstValue,
            "krate::LIMIT",
            SymbolKind::Const,
        )];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(
            summary(&result),
            vec![("krate::reader", Confidence::Possible, 1)]
        );
        assert_eq!(result.limits.candidate_steps, 1);
    }

    #[test]
    fn a_removal_reaches_a_removed_field_of_a_variant() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let choice = builder.child(module, "krate::Choice", SymbolKind::Enum);
        let first = builder.child(choice, "krate::Choice::First", SymbolKind::Variant);
        let payload = builder.child(first, "krate::Choice::First::value", SymbolKind::Field);
        let reader = builder.child(module, "krate::reader", SymbolKind::Function);
        builder.edge(reader, payload, EdgeKind::ReadsField, EdgeDetail::None);
        let base = builder.finish();

        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        builder.child(module, "krate::Choice", SymbolKind::Enum);
        builder.child(module, "krate::reader", SymbolKind::Function);
        let head = builder.finish();
        let changes = vec![member(
            ChangeKind::Variant,
            "krate::Choice::First",
            SymbolKind::Variant,
            Delta::Removed,
        )];

        let result = analyse(&base, &head, &changes);

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].symbol.path, "krate::reader");
    }

    #[test]
    fn a_two_step_first_step_is_reported_with_both_steps() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let trait_ = builder.child(module, "krate::Trait", SymbolKind::Trait);
        let run = builder.child(trait_, "krate::Trait::run", SymbolKind::Method);
        let block = builder.child(module, "krate::<impl Trait for A>", SymbolKind::Impl);
        let implementation =
            builder.child(block, "krate::<impl Trait for A>::run", SymbolKind::Method);
        let direct = builder.child(module, "krate::direct", SymbolKind::Function);
        builder.edge(implementation, run, EdgeKind::Implements, EdgeDetail::None);
        builder.edge(direct, implementation, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![member(
            ChangeKind::TraitItem,
            "krate::Trait::run",
            SymbolKind::Method,
            Delta::Modified,
        )];

        let result = analyse(&nothing(), &head, &changes);

        let direct = item(&result, "krate::direct");
        assert_eq!(direct.class, ImpactClass::Adaptation);
        assert_eq!(direct.distance, 2);
        let steps: Vec<(&str, &str)> = direct.paths[0]
            .steps
            .iter()
            .map(|step| (step.from.path.as_str(), step.to.path.as_str()))
            .collect();
        assert_eq!(
            steps,
            vec![
                ("krate::<impl Trait for A>::run", "krate::Trait::run"),
                ("krate::direct", "krate::<impl Trait for A>::run"),
            ]
        );
    }

    #[test]
    fn unresolved_references_count_when_they_lie_inside_a_listed_item() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        let mut head = builder.finish();
        head.symbols[caller.0 as usize].location.span = Span {
            start: 100,
            end: 200,
            start_line: 5,
            end_line: 9,
        };
        let reference = |start: u32, file: &str| blastr_model::UnresolvedRef {
            text: "missing".to_owned(),
            location: Location {
                file: RelPath::new(file),
                span: Span {
                    start,
                    end: start + 5,
                    start_line: 6,
                    end_line: 6,
                },
            },
        };
        head.unresolved = vec![
            reference(120, "src/lib.rs"),
            reference(198, "src/lib.rs"),
            reference(120, "src/other.rs"),
        ];
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(result.limits.unresolved_total, 3);
        assert_eq!(
            result.limits.unresolved_in_reach, 1,
            "one lies across the end of the item, one in another file"
        );
    }
}
