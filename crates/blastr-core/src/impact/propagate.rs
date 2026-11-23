//! The impact search: from the first steps of every change to the ranked items
//! they reach, with an evidence path for each and the counters that say what the
//! limits left out.
//!
//! The search is best first on the score of a path, so every item is reached
//! first by its strongest evidence. Score and distance are two things to keep,
//! not one: a weaker path that is also shorter reaches callers the strongest
//! path cannot reach within the depth limit, so an item is expanded again
//! whenever a path shorter than every path it was already expanded with arrives,
//! as long as that path still scores above the floor. Each item therefore keeps
//! a set of (score, distance) labels — the best-scoring one is what is reported,
//! the shortest one is what reaches farthest — and the labels of one item are at
//! most `depth` long, because every re-expansion is strictly shorter than the
//! last. The result never depends on hash order.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
};

use blastr_model::{
    CancelToken, Cancelled, Certainty, Change, ChangeDetail, ChangeKind, Confidence, EvidencePath,
    EvidenceStep, ImpactClass, ImpactItem, ImpactLimits, ImpactTags, Location, RankingParams,
    Reason, RelPath, RevisionGraph, SymbolId, SymbolPath,
};

use super::rules::{self, Graphs, Reach, Side};
use super::{ImpactInput, Propagation};

/// How many paths are kept for each item.
const KEPT_PATHS: usize = 3;

/// The slot of a node's best adaptation path.
const BEST_ADAPTATION: usize = 0;

/// The slot of a node's best behaviour path.
const BEST_BEHAVIOUR: usize = 1;

/// Computes what the changes reach, best first.
pub(crate) fn propagate<'a>(
    input: &'a ImpactInput<'a>,
    cancel: &CancelToken,
) -> Result<Propagation, Cancelled> {
    let mut search = Search::new(input);
    search.seed(cancel)?;
    search.run(cancel)?;
    Ok(search.finish())
}

/// A path while the search grows it.
struct Growing {
    /// Index of the change the path starts at.
    change: u32,
    /// Why the first step applies.
    reason: Reason,
    /// The class this path gives its item.
    class: ImpactClass,
    /// The steps so far, starting next to the change.
    steps: Vec<EvidenceStep>,
    /// The product of the weights of the steps.
    product: f32,
    /// The head symbols the path already contains.
    visited: Vec<SymbolId>,
}

impl Growing {
    /// The score of the path under `ranking`.
    fn score(&self, ranking: &RankingParams) -> f32 {
        let weight = match self.class {
            ImpactClass::Adaptation => ranking.adaptation_weight,
            ImpactClass::Behaviour => ranking.behaviour_weight,
        };
        let after_first = self.steps.len().saturating_sub(1) as i32;
        weight * self.product * ranking.decay.powi(after_first)
    }

    /// The path as it is reported.
    fn evidence(&self, score: f32) -> EvidencePath {
        EvidencePath {
            change: self.change,
            reason: self.reason,
            class: self.class,
            steps: self.steps.clone(),
            score,
        }
    }
}

/// What the search knows about one reached item.
struct Node {
    /// The item's head identity.
    symbol: SymbolPath,
    /// The best path of each class, in the [`BEST_ADAPTATION`] and
    /// [`BEST_BEHAVIOUR`] slots.
    best: [Option<EvidencePath>; 2],
    /// The best paths, at most [`KEPT_PATHS`], highest score first.
    kept: Vec<EvidencePath>,
    /// How many paths reached the item.
    path_count: u32,
    /// The fewest steps of any path the item was expanded with: the shortest
    /// label of the item that was followed, and hence the farthest its callers
    /// were looked for. `None` when it has not been expanded yet.
    shortest: Option<u32>,
}

impl Node {
    /// A node that has not been reached yet.
    fn new(symbol: SymbolPath) -> Node {
        Node {
            symbol,
            best: [None, None],
            kept: Vec::new(),
            path_count: 0,
            shortest: None,
        }
    }

    /// Whether a path of `steps` steps says something new about the item's
    /// reach: it is shorter than every path the item was expanded with, so its
    /// own callers sit closer to the change than the ones already followed.
    ///
    /// Paths arrive from the queue best score first, so a label that is not
    /// shorter is dominated by an expanded one that scores at least as much and
    /// reaches at least as far.
    fn wants_expansion(&self, steps: u32) -> bool {
        self.shortest.is_none_or(|shortest| steps < shortest)
    }

    /// Notes that the item was expanded with a path of `steps` steps.
    fn expanded_with(&mut self, steps: u32) {
        self.shortest = Some(self.shortest.map_or(steps, |old| old.min(steps)));
    }

    /// Records one more path to the item.
    fn record(&mut self, path: EvidencePath) {
        self.path_count += 1;
        let slot = match path.class {
            ImpactClass::Adaptation => BEST_ADAPTATION,
            ImpactClass::Behaviour => BEST_BEHAVIOUR,
        };
        let better = self.best[slot]
            .as_ref()
            .is_none_or(|best| path.score.total_cmp(&best.score).is_gt());
        if better {
            self.best[slot] = Some(path.clone());
        }
        let position = self
            .kept
            .iter()
            .position(|kept| path.score.total_cmp(&kept.score).is_gt());
        match position {
            Some(index) => self.kept.insert(index, path),
            None => self.kept.push(path),
        }
        self.kept.truncate(KEPT_PATHS);
    }
}

/// A path waiting to be followed further.
struct QueueEntry {
    /// The score of the path.
    score: f32,
    /// How many steps the path has.
    steps: u32,
    /// The item's head identity, which breaks ties between equal scores.
    symbol: SymbolPath,
    /// The item the path reaches.
    item: SymbolId,
    /// The path itself.
    path: Growing,
}

impl QueueEntry {
    /// Orders entries best first: higher score, then fewer steps, then path.
    fn order(&self, other: &QueueEntry) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.steps.cmp(&self.steps))
            .then_with(|| other.symbol.cmp(&self.symbol))
    }
}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &QueueEntry) -> bool {
        self.order(other) == Ordering::Equal
    }
}

impl Eq for QueueEntry {}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &QueueEntry) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &QueueEntry) -> Ordering {
        self.order(other)
    }
}

/// The state of one propagation run.
struct Search<'a> {
    /// What the run reads.
    input: &'a ImpactInput<'a>,
    /// The two graphs and their lookup tables.
    graphs: Graphs<'a>,
    /// One slot per head symbol, filled when the symbol is reached.
    nodes: Vec<Option<Node>>,
    /// The paths waiting to be followed further.
    heap: BinaryHeap<QueueEntry>,
    /// The head identities of renamed symbols, by their base identity.
    renamed: BTreeMap<SymbolPath, SymbolPath>,
    /// Items that only a path longer than the depth limit would reach.
    beyond: BTreeSet<SymbolId>,
    /// Items that only a path below the score floor would reach.
    below: BTreeSet<SymbolId>,
    /// Items a coarse first step reached.
    coarse: BTreeSet<SymbolId>,
}

impl<'a> Search<'a> {
    /// A search that has not looked at the changes yet.
    fn new(input: &'a ImpactInput<'a>) -> Search<'a> {
        Search {
            input,
            graphs: Graphs::new(input.base, input.head),
            nodes: (0..input.head.symbols.len()).map(|_| None).collect(),
            heap: BinaryHeap::new(),
            renamed: renamed_changes(input.changes),
            beyond: BTreeSet::new(),
            below: BTreeSet::new(),
            coarse: BTreeSet::new(),
        }
    }

    /// Takes the first step of every change.
    fn seed(&mut self, cancel: &CancelToken) -> Result<(), Cancelled> {
        let changes = self.input.changes;
        for (index, change) in changes.iter().enumerate() {
            cancel.check()?;
            for reach in rules::first_steps(change, &self.graphs) {
                self.seed_step(index, reach);
            }
        }
        Ok(())
    }

    /// Turns one first step into a path to its item.
    fn seed_step(&mut self, index: usize, reach: Reach) {
        let graph = self.graphs.graph(reach.side);
        let Some(edge) = graph.edges.get(reach.edge as usize) else {
            return;
        };
        let Some((item, item_path)) = self.head_counterpart(reach.side, edge.from) else {
            return;
        };
        let mut steps = Vec::with_capacity(usize::from(reach.via.is_some()) + 1);
        let mut visited = vec![item];
        let mut product = step_weight(reach.certainty, &self.input.params.ranking);
        // The symbol the item's own step arrives at: the changed one, or the
        // intermediate one when the rule takes two steps.
        let mut before = edge.to;
        let mut anchors = vec![edge.to];
        if let Some(via) = reach.via {
            let Some(step) = graph.edges.get(via.edge as usize) else {
                return;
            };
            let (Some(from), Some(to)) = (
                graph.symbols.get(step.from.0 as usize),
                graph.symbols.get(step.to.0 as usize),
            ) else {
                return;
            };
            steps.push(EvidenceStep {
                from: from.path.clone(),
                to: to.path.clone(),
                edge: step.kind,
                certainty: via.certainty,
                detail: step.detail,
                site: step.site.clone(),
            });
            product *= step_weight(via.certainty, &self.input.params.ranking);
            before = step.from;
            anchors = vec![step.to, step.from];
        }
        let Some(seed) = graph.symbols.get(before.0 as usize) else {
            return;
        };
        let seed_path = seed.path.clone();
        let detail = edge.detail;
        let kind = edge.kind;
        let site = edge.site.clone();
        for anchor in anchors {
            if let Some((head, _)) = self.head_counterpart(reach.side, anchor)
                && !visited.contains(&head)
            {
                visited.push(head);
            }
        }
        steps.push(EvidenceStep {
            from: item_path,
            to: seed_path,
            edge: kind,
            certainty: reach.certainty,
            detail,
            site,
        });
        let path = Growing {
            change: index as u32,
            reason: reach.reason,
            class: reach.class,
            steps,
            product,
            visited,
        };
        if reach.coarse {
            self.coarse.insert(item);
        }
        self.add(item, path);
    }

    /// Follows behaviour from every item the search has reached.
    fn run(&mut self, cancel: &CancelToken) -> Result<(), Cancelled> {
        while let Some(entry) = self.heap.pop() {
            cancel.check()?;
            let expanded = match self.nodes.get_mut(entry.item.0 as usize) {
                Some(Some(node)) if node.wants_expansion(entry.steps) => {
                    node.expanded_with(entry.steps);
                    true
                }
                _ => false,
            };
            if !expanded {
                continue;
            }
            for reach in rules::onward(entry.item, &self.graphs) {
                self.grow(&entry, reach);
            }
        }
        Ok(())
    }

    /// Extends a path with one behaviour step and records the result.
    fn grow(&mut self, parent: &QueueEntry, reach: Reach) {
        let head = self.input.head;
        let Some(edge) = head.edges.get(reach.edge as usize) else {
            return;
        };
        let item = edge.from;
        if parent.path.visited.contains(&item) {
            return;
        }
        let (Some(caller), Some(callee)) = (
            head.symbols.get(item.0 as usize),
            head.symbols.get(edge.to.0 as usize),
        ) else {
            return;
        };
        let mut steps = parent.path.steps.clone();
        steps.push(EvidenceStep {
            from: caller.path.clone(),
            to: callee.path.clone(),
            edge: edge.kind,
            certainty: reach.certainty,
            detail: edge.detail,
            site: edge.site.clone(),
        });
        let mut visited = parent.path.visited.clone();
        visited.push(item);
        let path = Growing {
            change: parent.path.change,
            reason: parent.path.reason,
            class: ImpactClass::Behaviour,
            steps,
            product: parent.path.product * step_weight(reach.certainty, &self.input.params.ranking),
            visited,
        };
        self.add(item, path);
    }

    /// Records a path to `item` and queues it when the limits keep it and the
    /// path could reach farther than the paths already followed.
    fn add(&mut self, item: SymbolId, path: Growing) {
        let Some(symbol) = self.input.head.symbols.get(item.0 as usize) else {
            return;
        };
        let symbol = symbol.path.clone();
        let score = path.score(&self.input.params.ranking);
        let steps = path.steps.len() as u32;
        if steps > self.input.params.depth {
            self.beyond.insert(item);
            return;
        }
        if score
            .total_cmp(&self.input.params.ranking.score_floor)
            .is_lt()
        {
            self.below.insert(item);
            return;
        }
        let evidence = path.evidence(score);
        let Some(slot) = self.nodes.get_mut(item.0 as usize) else {
            return;
        };
        if slot.is_none() {
            *slot = Some(Node::new(symbol.clone()));
        }
        let Some(node) = slot.as_mut() else {
            return;
        };
        let queue = node.wants_expansion(steps);
        node.record(evidence);
        if queue {
            self.heap.push(QueueEntry {
                score,
                steps,
                symbol,
                item,
                path,
            });
        }
    }

    /// The head identity and path of a symbol read on `side`.
    fn head_counterpart(&self, side: Side, id: SymbolId) -> Option<(SymbolId, SymbolPath)> {
        let item = match side {
            Side::Head => id,
            Side::Base => {
                let path = &self.input.base.symbols.get(id.0 as usize)?.path;
                self.head_of_base(path)?
            }
        };
        let symbol = self.input.head.symbols.get(item.0 as usize)?;
        Some((item, symbol.path.clone()))
    }

    /// The head symbol of a base symbol: the same identity, or the head side of
    /// a rename.
    fn head_of_base(&self, path: &SymbolPath) -> Option<SymbolId> {
        if let Some(id) = self.graphs.head_index.by_path(path) {
            return Some(id);
        }
        let renamed = self.renamed.get(path)?;
        self.graphs.head_index.by_path(renamed)
    }

    /// Whether the item at `id` was reached.
    fn reached(&self, id: SymbolId) -> bool {
        self.nodes
            .get(id.0 as usize)
            .is_some_and(|slot| slot.is_some())
    }

    /// Turns the search into its result.
    fn finish(self) -> Propagation {
        let params = self.input.params;
        let head = self.input.head;
        let changed = changed_symbols(self.input.changes);

        let beyond_depth = self.beyond.iter().filter(|&&id| !self.reached(id)).count() as u32;
        let below_floor = self
            .below
            .iter()
            .filter(|&&id| !self.reached(id) && !self.beyond.contains(&id))
            .count() as u32;
        let coarse_rules = self.coarse.iter().filter(|&&id| self.reached(id)).count() as u32;

        let mut items: Vec<ImpactItem> = Vec::new();
        let mut below_confidence = 0u32;
        let mut ignored = 0u32;
        for (index, slot) in self.nodes.iter().enumerate() {
            let Some(node) = slot else {
                continue;
            };
            let Some(symbol) = head.symbols.get(index) else {
                continue;
            };
            // An item a macro or a derive produced has no text of its own: it
            // is written at the invocation, and it is written again from the
            // macro once the item the invocation names changes. There is no
            // place to edit, so it is never a *needs editing* item; it still
            // carries propagation on to the macro and to its users.
            let adaptable = node.best[BEST_ADAPTATION].is_some() && !symbol.flags.from_macro;
            let class = if adaptable {
                ImpactClass::Adaptation
            } else {
                ImpactClass::Behaviour
            };
            let slot = if class == ImpactClass::Adaptation {
                BEST_ADAPTATION
            } else {
                BEST_BEHAVIOUR
            };
            let Some(best) = node.best[slot].as_ref() else {
                continue;
            };
            let item = ImpactItem {
                symbol: node.symbol.clone(),
                location: symbol.location.clone(),
                class,
                confidence: path_confidence(best),
                score: best.score,
                distance: best.steps.len() as u32,
                paths: node.kept.clone(),
                path_count: node.path_count,
                tags: ImpactTags {
                    is_test: symbol.flags.is_test,
                    is_main: symbol.flags.is_main,
                    is_pub_api: symbol.flags.is_pub_api,
                    changed: changed.contains(&symbol.path),
                },
            };
            if item.confidence < params.min_confidence {
                below_confidence += 1;
                continue;
            }
            if self.input.ignore.is_ignored(&item.location.file) {
                ignored += 1;
                continue;
            }
            items.push(item);
        }
        items.sort_by(compare_items);

        let mut candidate_steps: BTreeSet<StepKey> = BTreeSet::new();
        for item in &items {
            for path in &item.paths {
                for step in &path.steps {
                    if step.certainty == Certainty::Candidate {
                        candidate_steps.insert(candidate_step(step));
                    }
                }
            }
        }

        let unresolved_total = head.unresolved.len() as u32;
        let unresolved_in_reach = count_unresolved_in_reach(self.input.changes, &items, head);

        Propagation {
            items,
            limits: ImpactLimits {
                unresolved_total,
                unresolved_in_reach,
                candidate_steps: candidate_steps.len() as u32,
                coarse_rules,
                beyond_depth,
                below_floor,
                below_confidence,
                ignored,
            },
        }
    }
}

/// The weight one step contributes under `ranking`.
fn step_weight(certainty: Certainty, ranking: &RankingParams) -> f32 {
    match certainty {
        Certainty::Resolved => 1.0,
        Certainty::Candidate => ranking.candidate_weight,
    }
}

/// The unresolved references written inside a changed symbol or a listed item.
fn count_unresolved_in_reach(
    changes: &[Change],
    items: &[ImpactItem],
    head: &RevisionGraph,
) -> u32 {
    let mut regions: Vec<&Location> = changes
        .iter()
        .filter_map(|change| change.new.as_ref())
        .collect();
    regions.extend(items.iter().map(|item| &item.location));
    head.unresolved
        .iter()
        .filter(|reference| {
            regions
                .iter()
                .any(|&region| contains(region, &reference.location))
        })
        .count() as u32
}

/// The confidence a reported path gives its item.
fn path_confidence(path: &EvidencePath) -> Confidence {
    if path
        .steps
        .iter()
        .any(|step| step.certainty == Certainty::Candidate)
    {
        return Confidence::Possible;
    }
    match path.class {
        ImpactClass::Adaptation => Confidence::Definite,
        ImpactClass::Behaviour => Confidence::Likely,
    }
}

/// The documented order of the items: score, then distance, then path.
fn compare_items(left: &ImpactItem, right: &ImpactItem) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.distance.cmp(&right.distance))
        .then_with(|| left.symbol.cmp(&right.symbol))
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
    let mut renamed = BTreeMap::new();
    for change in changes {
        if let ChangeDetail::Renamed { from } = &change.detail {
            renamed.insert(from.clone(), change.symbol.clone());
        }
    }
    renamed
}

/// Whether `inner` lies inside `outer` in the same file.
fn contains(outer: &Location, inner: &Location) -> bool {
    outer.file == inner.file
        && inner.span.start >= outer.span.start
        && inner.span.end <= outer.span.end
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
        AnalysisParams, ChangeDetail, ChangeKind, Delta, EdgeDetail, EdgeKind, IgnoreSet,
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

    /// The items as (path, score, distance) triples.
    fn summary(propagation: &Propagation) -> Vec<(String, f32, u32)> {
        propagation
            .items
            .iter()
            .map(|item| (item.symbol.path.clone(), item.score, item.distance))
            .collect()
    }

    #[test]
    fn a_three_level_call_chain_decays_by_the_ranking() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let callee = builder.child(module, "krate::f", SymbolKind::Function);
        let first = builder.child(module, "krate::g", SymbolKind::Function);
        let second = builder.child(module, "krate::h", SymbolKind::Function);
        let third = builder.child(module, "krate::i", SymbolKind::Function);
        builder.edge(first, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(second, first, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(third, second, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];

        let result = analyse(&nothing(), &head, &changes);

        let scores = summary(&result);
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].0, "krate::g");
        assert!((scores[0].1 - 0.8).abs() < 1e-5);
        assert_eq!(scores[0].2, 1);
        assert_eq!(scores[1].0, "krate::h");
        assert!((scores[1].1 - 0.64).abs() < 1e-5);
        assert_eq!(scores[1].2, 2);
        assert_eq!(scores[2].0, "krate::i");
        assert!((scores[2].1 - 0.512).abs() < 1e-5);
        assert_eq!(scores[2].2, 3);
        assert!(
            result
                .items
                .iter()
                .all(|item| item.class == ImpactClass::Behaviour)
        );
        assert!(
            result
                .items
                .iter()
                .all(|item| item.confidence == Confidence::Likely)
        );
    }

    #[test]
    fn a_candidate_step_caps_the_confidence_and_weighs_less() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(result.items.len(), 1);
        let item = &result.items[0];
        assert_eq!(item.symbol.path, "krate::holder");
        assert_eq!(item.confidence, Confidence::Possible);
        assert!((item.score - 0.4).abs() < 1e-5);
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
            change(ChangeKind::Signature, "krate::f", SymbolKind::Function),
            change(ChangeKind::Body, "krate::k", SymbolKind::Function),
        ];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(result.items.len(), 1);
        let item = &result.items[0];
        assert_eq!(item.class, ImpactClass::Adaptation);
        assert_eq!(item.confidence, Confidence::Definite);
        assert!((item.score - 1.0).abs() < 1e-5);
        assert_eq!(item.distance, 1);
        assert_eq!(item.path_count, 2);
        assert_eq!(item.paths.len(), 2);
    }

    #[test]
    fn the_depth_limit_counts_the_items_it_cuts_off() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let callee = builder.child(module, "krate::f", SymbolKind::Function);
        let first = builder.child(module, "krate::g", SymbolKind::Function);
        let second = builder.child(module, "krate::h", SymbolKind::Function);
        builder.edge(first, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(second, first, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];
        let params = AnalysisParams {
            depth: 1,
            ..AnalysisParams::default()
        };

        let result = run(&nothing(), &head, &changes, &params, &IgnoreSet::empty());

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].symbol.path, "krate::g");
        assert_eq!(result.limits.beyond_depth, 1);
        assert_eq!(result.limits.below_floor, 0);
    }

    #[test]
    fn the_score_floor_counts_the_items_it_leaves_out() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let callee = builder.child(module, "krate::f", SymbolKind::Function);
        let first = builder.child(module, "krate::g", SymbolKind::Function);
        let second = builder.child(module, "krate::h", SymbolKind::Function);
        builder.edge(first, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(second, first, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];
        let params = AnalysisParams {
            ranking: RankingParams {
                score_floor: 0.7,
                ..RankingParams::default()
            },
            ..AnalysisParams::default()
        };

        let result = run(&nothing(), &head, &changes, &params, &IgnoreSet::empty());

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].symbol.path, "krate::g");
        assert_eq!(result.limits.below_floor, 1);
        assert_eq!(result.limits.beyond_depth, 0);
    }

    #[test]
    fn the_confidence_filter_counts_what_it_hides() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let function = builder.child(module, "krate::f", SymbolKind::Function);
        let caller = builder.child(module, "krate::caller", SymbolKind::Function);
        let holder = builder.child(module, "krate::holder", SymbolKind::Function);
        builder.edge(caller, function, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(holder, function, EdgeKind::TakesAsValue, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];
        let params = AnalysisParams {
            min_confidence: Confidence::Likely,
            ..AnalysisParams::default()
        };

        let result = run(&nothing(), &head, &changes, &params, &IgnoreSet::empty());

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].symbol.path, "krate::caller");
        assert_eq!(result.limits.below_confidence, 1);
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

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].symbol.path, "krate::caller");
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
        let paths: Vec<&str> = first
            .items
            .iter()
            .map(|item| item.symbol.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["krate::caller", "krate::dispatcher", "krate::holder"],
            "score, then confidence and path order the items"
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

        assert_eq!(result.items.len(), 1);
        let item = &result.items[0];
        assert_eq!(item.symbol.path, "krate::caller");
        assert_eq!(item.class, ImpactClass::Adaptation);
        assert_eq!(item.confidence, Confidence::Definite);
        assert_eq!(item.distance, 1);
        assert!((item.score - 1.0).abs() < 1e-5);
        let step = &item.paths[0].steps[0];
        assert_eq!(step.from.path, "krate::caller");
        assert_eq!(step.to.path, "krate::f");
        assert_eq!(step.edge, EdgeKind::Calls);
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
    fn a_call_cycle_terminates() {
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let first = builder.child(module, "krate::a", SymbolKind::Function);
        let second = builder.child(module, "krate::b", SymbolKind::Function);
        builder.edge(first, second, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(second, first, EdgeKind::Calls, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::a", SymbolKind::Function)];

        let result = analyse(&nothing(), &head, &changes);

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].symbol.path, "krate::b");
        assert_eq!(result.limits.beyond_depth, 0);
        assert_eq!(result.limits.below_floor, 0);
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

        let middle = result
            .items
            .iter()
            .find(|item| item.symbol.path == "krate::g")
            .expect("the changed function is reached by the other change");
        assert!(middle.tags.changed);
        let caller = result
            .items
            .iter()
            .find(|item| item.symbol.path == "krate::caller")
            .expect("the caller is reached");
        assert!(!caller.tags.changed);
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

        let middle = result
            .items
            .iter()
            .find(|item| item.symbol.path == "krate::m")
            .expect("the middle function is reached");
        assert_eq!(middle.path_count, 4);
        assert_eq!(middle.paths.len(), 3);
        assert_eq!(middle.distance, 1);
        assert!((middle.score - 0.8).abs() < 1e-5);
    }

    #[test]
    fn a_shorter_weaker_path_reaches_the_callers_the_strongest_path_cannot() {
        // `hub` is reached by a long resolved chain of eight steps, which scores
        // best, and by a shorter chain of five steps whose last step is a
        // candidate. The test calls `hub`, so it sits at distance six through the
        // short chain and at nine through the long one: the depth limit of eight
        // cuts the long one and keeps the short one.
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
        // The defaults: depth 8, decay 0.8, candidate weight 0.5.
        let result = analyse(&nothing(), &head, &changes);

        // The item keeps reporting its best path: eight steps, all resolved.
        let hub_item = result
            .items
            .iter()
            .find(|item| item.symbol.path == "krate::hub")
            .expect("the hub is reached");
        assert_eq!(hub_item.distance, 8);
        assert!((hub_item.score - 0.8 * 0.8_f32.powi(7)).abs() < 1e-5);
        assert_eq!(hub_item.confidence, Confidence::Likely);
        // The test is beyond the depth limit through that best path and inside it
        // through the shorter, weaker one, so only the shorter one reaches it.
        let test_item = result
            .items
            .iter()
            .find(|item| item.symbol.path == "krate::test")
            .expect("the test is reached through the shorter path");
        assert_eq!(test_item.distance, 6);
        assert!((test_item.score - 0.8 * 0.5 * 0.8_f32.powi(5)).abs() < 1e-5);
        assert_eq!(test_item.confidence, Confidence::Possible);
        assert_eq!(result.limits.beyond_depth, 0);
        assert_eq!(result.limits.below_floor, 0);
        assert_eq!(result.limits.candidate_steps, 1);
    }

    #[test]
    fn an_item_reached_only_by_a_short_path_is_expanded_once() {
        // The same shape without the long chain: the hub is only reached by the
        // short candidate path, so the test is reached at distance six.
        let mut builder = GraphBuilder::new();
        let module = builder.add("krate", SymbolKind::Module);
        let callee = builder.child(module, "krate::f", SymbolKind::Function);
        let first = builder.child(module, "krate::g", SymbolKind::Function);
        let second = builder.child(module, "krate::h", SymbolKind::Function);
        let hub = builder.child(module, "krate::hub", SymbolKind::Function);
        builder.edge(first, callee, EdgeKind::Calls, EdgeDetail::None);
        builder.edge(second, first, EdgeKind::Calls, EdgeDetail::None);
        builder.candidate(hub, second, EdgeKind::TakesAsValue, EdgeDetail::None);
        let head = builder.finish();
        let changes = vec![change(ChangeKind::Body, "krate::f", SymbolKind::Function)];

        let result = analyse(&nothing(), &head, &changes);

        let paths: Vec<(&str, u32)> = result
            .items
            .iter()
            .map(|item| (item.symbol.path.as_str(), item.distance))
            .collect();
        assert_eq!(
            paths,
            vec![("krate::g", 1), ("krate::h", 2), ("krate::hub", 3)]
        );
        assert_eq!(result.items[2].path_count, 1);
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

        let paths: Vec<&str> = result
            .items
            .iter()
            .map(|item| item.symbol.path.as_str())
            .collect();
        assert_eq!(paths, vec!["krate::outside"]);
        assert_eq!(result.limits.coarse_rules, 1);
        assert!(
            result
                .items
                .iter()
                .all(|item| item.class == ImpactClass::Adaptation)
        );
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

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].confidence, Confidence::Possible);
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
}
