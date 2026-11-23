//! What the graph shows: the changes, the affected symbols they reach, and the
//! references between them.
//!
//! The visible subgraph is deliberately small and deterministic. Layer zero is
//! the change itself; every further layer shows only its best-ranked
//! [`PER_LAYER`] symbols, so a wall of near-identical test symbols cannot swamp
//! the change. What a layer leaves out collapses into a single `+N more` pill,
//! and a click on that pill raises the layer's limit by [`PER_LAYER_STEP`]. The
//! selected symbol and the symbols of its best evidence path are always kept,
//! and so are the neighbours the reader expanded.
//!
//! Every drawn symbol knows how many of its neighbours are not drawn, and every
//! drawn reference knows whether it lies on the selected item's best path.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use blastr_model::wording;
use blastr_model::{
    Analysis, Certainty, Change, ChangeDetail, ChangeKind, Confidence, Delta, ImpactClass,
    RevisionGraph, SymbolKind, SymbolPath,
};

/// The most nodes the graph draws at once.
pub(crate) const NODE_CAP: usize = 300;

/// How many symbols one layer shows before the rest collapse into one pill.
pub(crate) const PER_LAYER: usize = 8;

/// How many symbols a double-click on a pill adds to its layer.
pub(crate) const PER_LAYER_STEP: usize = 12;

/// How many neighbours one expansion adds.
const EXPANSION: usize = 6;

/// What a node is, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeClass {
    /// Something that changed: the cause.
    Changed,
    /// Something that has to be edited.
    NeedsEditing,
    /// Something that may behave differently.
    MayBehave,
    /// A neighbour added by expanding a node.
    Expanded,
}

/// One drawn symbol.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Node {
    /// The symbol.
    pub symbol: SymbolPath,
    /// The item's own name: the last segment of [`wording::short_name`]'s
    /// name, as in `next`, `deserialize_any` or `test_error`.
    pub name: String,
    /// The type whose item this is, when the short name has one:
    /// `StreamDeserializer` for `StreamDeserializer::next`.
    pub owner: Option<String>,
    /// The module the symbol lives in, from [`wording::short_name`].
    pub module: String,
    /// What it is.
    pub class: NodeClass,
    /// How sure the evidence is.
    pub confidence: Confidence,
    /// Steps from the nearest change.
    pub distance: u32,
    /// How many neighbours of this symbol are not drawn.
    pub hidden: usize,
    /// Whether the symbol is a test.
    pub is_test: bool,
    /// Whether the symbol was already edited in the same diff.
    pub edited: bool,
    /// The chip the node carries: a change kind such as `BODY`, or `EDITED`.
    pub chip: Option<String>,
    /// `file:line` of the definition or of the reference that reaches it.
    pub site: String,
    /// The tooltip line: `the change`, or `2 steps from next`.
    pub origin: String,
    /// Whether this is the symbol the user selected.
    pub selected: bool,
}

impl Node {
    /// The whole short name, as the tooltip writes it:
    /// `StreamDeserializer::next`, or the bare item name when it has no owner.
    pub(crate) fn label(&self) -> String {
        match &self.owner {
            Some(owner) => format!("{owner}::{}", self.name),
            None => self.name.clone(),
        }
    }

    /// The node's second line: the owner and the module, as in
    /// `Deserializer · serde_json::de`. Either part is left out when empty.
    pub(crate) fn subtitle(&self) -> String {
        match (&self.owner, self.module.is_empty()) {
            (Some(owner), false) => format!("{owner} · {}", self.module),
            (Some(owner), true) => owner.clone(),
            (None, false) => self.module.clone(),
            (None, true) => String::new(),
        }
    }
}

/// A layer's remainder: the `+N more` pill under its nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Collapsed {
    /// The layer, which is the distance of the symbols it hides.
    pub layer: usize,
    /// How many symbols it hides.
    pub hidden: usize,
}

/// One drawn reference, pointing from the change outwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Edge {
    /// The index of the node nearer the change.
    pub from: usize,
    /// The index of the node further from the change.
    pub to: usize,
    /// Whether at least one of the references is a candidate.
    pub candidate: bool,
    /// Whether the reference lies on the selected item's best evidence path.
    pub highlight: bool,
}

/// The visible subgraph.
#[derive(Debug, Clone, Default)]
pub(crate) struct Model {
    /// The nodes, causes first.
    pub nodes: Vec<Node>,
    /// The layers whose remaining symbols are hidden behind one pill.
    pub collapsed: Vec<Collapsed>,
    /// The references between them, oriented away from the change.
    pub edges: Vec<Edge>,
    /// How many symbols the whole subgraph holds before the per-layer limit.
    pub all_nodes: usize,
    /// Whether the overall node cap dropped symbols.
    pub capped: bool,
}

impl Model {
    /// The nodes that lie in each distance layer, from zero up.
    pub(crate) fn layers(&self) -> Vec<Vec<usize>> {
        if self.nodes.is_empty() {
            return Vec::new();
        }
        let mut depth = 0;
        for node in &self.nodes {
            depth = depth.max(node.distance);
        }
        let mut layers = vec![Vec::new(); usize::try_from(depth).unwrap_or(0) + 1];
        for (index, node) in self.nodes.iter().enumerate() {
            if let Some(layer) = layers.get_mut(usize::try_from(node.distance).unwrap_or(0)) {
                layer.push(index);
            }
        }
        layers
    }
}

/// What the graph should show.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Request<'a> {
    /// The result.
    pub analysis: &'a Analysis,
    /// The newer side's reference graph, when it has one.
    pub graph: Option<&'a RevisionGraph>,
    /// The symbols whose hidden neighbours are pulled in.
    pub expanded: &'a [SymbolPath],
    /// The one selection every view shares.
    pub selected: Option<&'a SymbolPath>,
    /// How many symbols each layer adds beyond [`PER_LAYER`], by layer.
    pub limits: &'a BTreeMap<usize, usize>,
}

/// Builds the visible subgraph.
pub(crate) fn build(request: Request<'_>) -> Model {
    let Request {
        analysis,
        graph,
        expanded,
        selected,
        limits,
    } = request;

    let mut nodes: Vec<Node> = Vec::new();
    let mut index: HashMap<SymbolPath, usize> = HashMap::new();

    for change in &analysis.diff.changes {
        if change.symbol.kind == SymbolKind::Module || index.contains_key(&change.symbol) {
            continue;
        }
        let site = change
            .new
            .as_ref()
            .or(change.old.as_ref())
            .map(wording::site_text)
            .unwrap_or_default();
        let short = wording::short_name(&change.symbol.path);
        let (name, owner) = split_short(&short.name);
        index.insert(change.symbol.clone(), nodes.len());
        nodes.push(Node {
            name,
            owner,
            module: short.module,
            symbol: change.symbol.clone(),
            class: NodeClass::Changed,
            confidence: Confidence::Definite,
            distance: 0,
            hidden: 0,
            is_test: false,
            edited: false,
            chip: Some(change_chip(change)),
            site,
            origin: steps_text(0, None),
            selected: selected == Some(&change.symbol),
        });
    }

    for item in &analysis.items {
        if index.contains_key(&item.symbol) {
            continue;
        }
        let class = match item.class {
            ImpactClass::Adaptation => NodeClass::NeedsEditing,
            ImpactClass::Behaviour => NodeClass::MayBehave,
        };
        let short = wording::short_name(&item.symbol.path);
        let (name, owner) = split_short(&short.name);
        let origin = wording::best_path(item)
            .and_then(|path| analysis.diff.changes.get(path.change as usize))
            .map(|change| wording::short_name(&change.symbol.path).name);
        index.insert(item.symbol.clone(), nodes.len());
        nodes.push(Node {
            name,
            owner,
            module: short.module,
            symbol: item.symbol.clone(),
            class,
            confidence: item.confidence,
            distance: item.distance,
            hidden: 0,
            is_test: item.tags.is_test,
            edited: item.tags.changed,
            chip: item.tags.changed.then(|| "EDITED".to_owned()),
            site: wording::site_text(&item.location),
            origin: steps_text(item.distance, origin.as_deref()),
            selected: selected == Some(&item.symbol),
        });
    }

    // Expansion: neighbours of the opened nodes, taken from the newer graph.
    if let Some(graph) = graph {
        let by_id = |id: u32| graph.symbols.get(id as usize);
        for symbol in expanded {
            let Some(&from) = index.get(symbol) else {
                continue;
            };
            let parent_distance = nodes[from].distance;
            let parent_name = nodes[from].label();
            let mut added = 0usize;
            for edge in &graph.edges {
                if added >= EXPANSION {
                    break;
                }
                let (Some(source), Some(target)) = (by_id(edge.from.0), by_id(edge.to.0)) else {
                    continue;
                };
                if source.path != nodes[from].symbol {
                    continue;
                }
                if index.contains_key(&target.path) {
                    added += 1;
                    continue;
                }
                if target.path.kind == SymbolKind::Module {
                    continue;
                }
                let short = wording::short_name(&target.path.path);
                let (name, owner) = split_short(&short.name);
                index.insert(target.path.clone(), nodes.len());
                nodes.push(Node {
                    name,
                    owner,
                    module: short.module,
                    symbol: target.path.clone(),
                    class: NodeClass::Expanded,
                    confidence: Confidence::Possible,
                    distance: parent_distance + 1,
                    hidden: 0,
                    is_test: target.flags.is_test,
                    edited: false,
                    chip: None,
                    site: wording::site_text(&target.location),
                    origin: steps_text(1, Some(&parent_name)),
                    selected: selected == Some(&target.path),
                });
                added += 1;
            }
        }
    }

    let all_nodes = nodes.len();

    // The symbols the picture always keeps, whatever its layer's limit is.
    let mut forced: BTreeSet<usize> = BTreeSet::new();
    for symbol in expanded {
        if let Some(&position) = index.get(symbol) {
            forced.insert(position);
        }
    }
    if let Some(selected) = selected {
        if let Some(&position) = index.get(selected) {
            forced.insert(position);
        }
        if let Some(item) = analysis.items.iter().find(|item| &item.symbol == selected)
            && let Some(path) = wording::best_path(item)
        {
            for step in &path.steps {
                for symbol in [&step.from, &step.to] {
                    if let Some(&position) = index.get(symbol) {
                        forced.insert(position);
                    }
                }
            }
        }
    }

    // Per-layer limits: the forced symbols first, then the ranked order.
    let depth = nodes.iter().map(|node| node.distance).max().unwrap_or(0);
    let mut keep = vec![false; nodes.len()];
    let mut collapsed: Vec<Collapsed> = Vec::new();
    for layer in 0..=depth {
        let members: Vec<usize> = (0..nodes.len())
            .filter(|&position| nodes[position].distance == layer)
            .collect();
        let extra = limits
            .get(&usize::try_from(layer).unwrap_or(0))
            .copied()
            .unwrap_or(0);
        let limit = PER_LAYER.saturating_add(extra);
        let mut kept = 0usize;
        for &position in &members {
            if forced.contains(&position) {
                keep[position] = true;
                kept += 1;
            }
        }
        for &position in &members {
            if kept >= limit {
                break;
            }
            if !keep[position] {
                keep[position] = true;
                kept += 1;
            }
        }
        let hidden = members.len().saturating_sub(kept);
        if hidden > 0 {
            collapsed.push(Collapsed {
                layer: usize::try_from(layer).unwrap_or(0),
                hidden,
            });
        }
    }

    // References of the best path of each item, oriented away from the change.
    let ordered = |left: usize, right: usize| (left.min(right), left.max(right));
    let highlight_pairs: BTreeSet<(usize, usize)> = selected
        .and_then(|symbol| analysis.items.iter().find(|item| &item.symbol == symbol))
        .and_then(wording::best_path)
        .map(|path| {
            path.steps
                .iter()
                .filter_map(|step| {
                    let from = *index.get(&step.from)?;
                    let to = *index.get(&step.to)?;
                    (from != to).then(|| ordered(from, to))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut pairs: BTreeMap<(usize, usize), (bool, bool)> = BTreeMap::new();
    for item in &analysis.items {
        let Some(path) = wording::best_path(item) else {
            continue;
        };
        for step in &path.steps {
            let (Some(&from), Some(&to)) = (index.get(&step.from), index.get(&step.to)) else {
                continue;
            };
            if from == to || !keep[from] || !keep[to] {
                continue;
            }
            let (near, far) = if nodes[from].distance <= nodes[to].distance {
                (from, to)
            } else {
                (to, from)
            };
            if nodes[near].distance == nodes[far].distance {
                continue;
            }
            let candidate = step.certainty == Certainty::Candidate;
            let highlight = highlight_pairs.contains(&ordered(from, to));
            pairs
                .entry((near, far))
                .and_modify(|seen| {
                    seen.0 &= candidate;
                    seen.1 |= highlight;
                })
                .or_insert((candidate, highlight));
        }
    }

    // Renumber the kept symbols; the rest never reaches the drawing.
    let mut drawn: HashMap<usize, usize> = HashMap::new();
    let mut visible: Vec<Node> = Vec::new();
    let mut drawn_symbols: HashMap<SymbolPath, usize> = HashMap::new();
    for (position, node) in nodes.into_iter().enumerate() {
        if !keep[position] {
            continue;
        }
        drawn.insert(position, visible.len());
        drawn_symbols.insert(node.symbol.clone(), visible.len());
        visible.push(node);
    }

    // Hidden neighbours: references out of a drawn node to an undrawn symbol.
    if let Some(graph) = graph {
        for edge in &graph.edges {
            let (Some(source), Some(target)) = (
                graph.symbols.get(edge.from.0 as usize),
                graph.symbols.get(edge.to.0 as usize),
            ) else {
                continue;
            };
            let Some(&position) = drawn_symbols.get(&source.path) else {
                continue;
            };
            if drawn_symbols.contains_key(&target.path) {
                continue;
            }
            visible[position].hidden += 1;
        }
    }

    let capped = visible.len() > NODE_CAP;
    if capped {
        visible.truncate(NODE_CAP);
    }
    let edges: Vec<Edge> = pairs
        .into_iter()
        .filter_map(|((near, far), (candidate, highlight))| {
            let from = *drawn.get(&near)?;
            let to = *drawn.get(&far)?;
            (from < visible.len() && to < visible.len()).then_some(Edge {
                from,
                to,
                candidate,
                highlight,
            })
        })
        .collect();

    Model {
        nodes: visible,
        collapsed,
        edges,
        all_nodes,
        capped,
    }
}

/// Splits a short name into the item's own name and the owner before it.
///
/// `StreamDeserializer::next` gives `next` and `StreamDeserializer`, while
/// `parse_whitespace` gives `parse_whitespace` and no owner.
fn split_short(name: &str) -> (String, Option<String>) {
    match name.rsplit_once("::") {
        Some((owner, item)) if !owner.is_empty() && !item.is_empty() => {
            (item.to_owned(), Some(owner.to_owned()))
        }
        _ => (name.to_owned(), None),
    }
}

/// The short chip a change node carries inside its box.
fn change_chip(change: &Change) -> String {
    let member = match &change.detail {
        ChangeDetail::Member(Delta::Added) => "+",
        ChangeDetail::Member(Delta::Removed) => "-",
        ChangeDetail::Member(Delta::Modified) => "~",
        _ => "",
    };
    let label = match change.kind {
        ChangeKind::NonSemantic => "FMT",
        ChangeKind::Body => "BODY",
        ChangeKind::Signature => "SIG",
        ChangeKind::Visibility => "VIS",
        ChangeKind::Added => "NEW",
        ChangeKind::Removed => "GONE",
        ChangeKind::Renamed => "RENAMED",
        ChangeKind::Field => "FIELD",
        ChangeKind::Variant => "VAR",
        ChangeKind::TraitItem => "TRAIT",
        ChangeKind::Impl => "IMPL",
        ChangeKind::ConstValue => "VALUE",
        ChangeKind::TypeAlias => "TYPE",
        ChangeKind::MacroDef => "MACRO",
        ChangeKind::Attribute => "ATTR",
        ChangeKind::Manifest => "MANIFEST",
    };
    format!("{member}{label}")
}

/// How far a node is from the change, in the words the tooltip uses.
fn steps_text(distance: u32, change: Option<&str>) -> String {
    match (distance, change) {
        (0, _) => "the change".to_owned(),
        (1, Some(change)) => format!("1 step from {change}"),
        (distance, Some(change)) => format!("{distance} steps from {change}"),
        (_, None) => "reached by the change".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        AnalysisParams, Change, ChangeDetail, ChangeKind, Comparison, DiffResult, EvidencePath,
        EvidenceStep, ImpactItem, ImpactLimits, ImpactTags, Location, Reason, RelPath, Source,
        Span, TestSelection,
    };

    use super::*;

    fn location(line: u32) -> Location {
        Location {
            file: RelPath::new("src/lib.rs"),
            span: Span {
                start: 0,
                end: 1,
                start_line: line,
                end_line: line,
            },
        }
    }

    fn symbol(path: &str) -> SymbolPath {
        SymbolPath::new(path, SymbolKind::Function)
    }

    fn change(path: &str) -> Change {
        Change {
            kind: ChangeKind::Body,
            symbol: symbol(path),
            old: None,
            new: Some(location(1)),
            detail: ChangeDetail::None,
        }
    }

    fn item(path: &str, distance: u32, class: ImpactClass) -> ImpactItem {
        let step = EvidenceStep {
            from: symbol(path),
            to: symbol("k::changed"),
            edge: blastr_model::EdgeKind::Calls,
            certainty: Certainty::Resolved,
            detail: blastr_model::EdgeDetail::None,
            site: location(3),
        };
        ImpactItem {
            symbol: symbol(path),
            location: location(5),
            class,
            confidence: Confidence::Definite,
            score: 0.5,
            distance,
            paths: vec![EvidencePath {
                change: 0,
                reason: Reason::CallsChanged,
                class,
                steps: vec![step],
                score: 0.5,
            }],
            path_count: 1,
            tags: ImpactTags::default(),
        }
    }

    fn analysis(items: Vec<ImpactItem>) -> Analysis {
        Analysis {
            diff: DiffResult {
                comparison: Comparison {
                    base: Source::Worktree,
                    head: Source::Worktree,
                },
                base_key: None,
                head_key: None,
                changed_files: Vec::new(),
                changes: vec![change("k::changed")],
                limits: Vec::new(),
            },
            items,
            tests: TestSelection::default(),
            limits: ImpactLimits::default(),
            params: AnalysisParams::default(),
        }
    }

    fn no_limits() -> BTreeMap<usize, usize> {
        BTreeMap::new()
    }

    fn request<'a>(
        analysis: &'a Analysis,
        selected: Option<&'a SymbolPath>,
        limits: &'a BTreeMap<usize, usize>,
    ) -> Request<'a> {
        Request {
            analysis,
            graph: None,
            expanded: &[],
            selected,
            limits,
        }
    }

    #[test]
    fn a_short_name_splits_into_item_and_owner() {
        assert_eq!(
            split_short("StreamDeserializer::next"),
            ("next".to_owned(), Some("StreamDeserializer".to_owned()))
        );
        assert_eq!(
            split_short("parse_whitespace"),
            ("parse_whitespace".to_owned(), None)
        );
        assert_eq!(
            split_short("Outer::Inner::run"),
            ("run".to_owned(), Some("Outer::Inner".to_owned()))
        );
    }

    #[test]
    fn the_node_label_is_the_item_name_and_the_second_line_names_the_owner() {
        let path = "serde_json::de::<impl Iterator for StreamDeserializer<'de, R, T>>::next";
        let analysis = analysis(vec![item(path, 1, ImpactClass::Behaviour)]);
        let model = build(request(&analysis, None, &no_limits()));
        let node = &model.nodes[1];
        assert_eq!(node.name, "next");
        assert_eq!(node.owner.as_deref(), Some("StreamDeserializer"));
        assert_eq!(node.subtitle(), "StreamDeserializer · serde_json::de");
        assert_eq!(node.label(), "StreamDeserializer::next");
    }

    #[test]
    fn a_free_function_has_no_owner_in_its_subtitle() {
        let analysis = analysis(vec![item(
            "k::tests::test_error",
            1,
            ImpactClass::Behaviour,
        )]);
        let model = build(request(&analysis, None, &no_limits()));
        let node = &model.nodes[1];
        assert_eq!(node.name, "test_error");
        assert_eq!(node.owner, None);
        assert_eq!(node.subtitle(), "k::tests");
        assert_eq!(node.label(), "test_error");
    }

    #[test]
    fn a_layer_shows_eight_nodes_by_default() {
        assert_eq!(PER_LAYER, 8);
    }

    #[test]
    fn the_change_is_the_first_layer_and_the_items_follow() {
        let analysis = analysis(vec![
            item("k::one", 1, ImpactClass::Adaptation),
            item("k::two", 2, ImpactClass::Behaviour),
        ]);
        let model = build(request(&analysis, None, &no_limits()));
        assert_eq!(model.nodes.len(), 3);
        assert_eq!(model.nodes[0].class, NodeClass::Changed);
        assert_eq!(model.nodes[1].distance, 1);
        assert_eq!(model.nodes[2].distance, 2);
        let layers = model.layers();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec![0]);
        assert!(model.collapsed.is_empty());
    }

    #[test]
    fn references_point_away_from_the_change() {
        let analysis = analysis(vec![item("k::one", 1, ImpactClass::Adaptation)]);
        let model = build(request(&analysis, None, &no_limits()));
        assert_eq!(
            model.edges,
            vec![Edge {
                from: 0,
                to: 1,
                candidate: false,
                highlight: false,
            }]
        );
    }

    #[test]
    fn a_layer_shows_a_bounded_number_of_nodes_and_one_pill() {
        let items: Vec<ImpactItem> = (0..156)
            .map(|index| item(&format!("k::f{index}"), 1, ImpactClass::Behaviour))
            .collect();
        let analysis = analysis(items);
        let model = build(request(&analysis, None, &no_limits()));
        let layer_one = model.nodes.iter().filter(|node| node.distance == 1).count();
        assert_eq!(layer_one, PER_LAYER);
        assert_eq!(model.nodes.len(), PER_LAYER + 1);
        assert_eq!(
            model.collapsed,
            vec![Collapsed {
                layer: 1,
                hidden: 156 - PER_LAYER,
            }]
        );
        assert_eq!(model.all_nodes, 157);
        assert_eq!(model.nodes[1].name, "f0");
    }

    #[test]
    fn a_raised_limit_shows_more_of_the_layer() {
        let items: Vec<ImpactItem> = (0..156)
            .map(|index| item(&format!("k::f{index}"), 1, ImpactClass::Behaviour))
            .collect();
        let analysis = analysis(items);
        let mut limits = BTreeMap::new();
        limits.insert(1usize, PER_LAYER_STEP);
        let model = build(request(&analysis, None, &limits));
        let layer_one = model.nodes.iter().filter(|node| node.distance == 1).count();
        assert_eq!(layer_one, PER_LAYER + PER_LAYER_STEP);
        assert_eq!(
            model.collapsed.first().map(|collapsed| collapsed.hidden),
            Some(156 - PER_LAYER - PER_LAYER_STEP)
        );
    }

    #[test]
    fn the_selected_symbol_is_kept_whatever_its_rank() {
        let items: Vec<ImpactItem> = (0..156)
            .map(|index| item(&format!("k::f{index}"), 1, ImpactClass::Behaviour))
            .collect();
        let analysis = analysis(items);
        let selected = symbol("k::f150");
        let model = build(request(&analysis, Some(&selected), &no_limits()));
        let node = model
            .nodes
            .iter()
            .find(|node| node.symbol == selected)
            .expect("the selected symbol is drawn");
        assert!(node.selected);
        assert_eq!(
            model.nodes.iter().filter(|node| node.distance == 1).count(),
            PER_LAYER
        );
    }

    #[test]
    fn the_selected_path_is_highlighted() {
        let analysis = analysis(vec![
            item("k::one", 1, ImpactClass::Adaptation),
            item("k::two", 1, ImpactClass::Behaviour),
        ]);
        let selected = symbol("k::one");
        let model = build(request(&analysis, Some(&selected), &no_limits()));
        let highlighted: Vec<(usize, usize)> = model
            .edges
            .iter()
            .filter(|edge| edge.highlight)
            .map(|edge| (edge.from, edge.to))
            .collect();
        assert_eq!(highlighted, vec![(0, 1)]);
    }

    #[test]
    fn the_chip_says_what_changed() {
        let mut added = change("k::Enum");
        added.kind = ChangeKind::Variant;
        added.detail = ChangeDetail::Member(Delta::Added);
        assert_eq!(change_chip(&added), "+VAR");
        let body = change("k::f");
        assert_eq!(change_chip(&body), "BODY");
    }

    #[test]
    fn the_overall_cap_still_holds() {
        let items: Vec<ImpactItem> = (0..NODE_CAP + 40)
            .map(|index| {
                let distance = u32::try_from(index % 40).unwrap_or(0) + 1;
                item(&format!("k::f{index}"), distance, ImpactClass::Behaviour)
            })
            .collect();
        let analysis = analysis(items);
        let model = build(request(&analysis, None, &no_limits()));
        assert_eq!(model.nodes.len(), NODE_CAP);
        assert!(model.capped);
        assert!(model.all_nodes > NODE_CAP);
    }

    #[test]
    fn the_same_input_gives_the_same_model() {
        let analysis = analysis(vec![
            item("k::one", 1, ImpactClass::Adaptation),
            item("k::two", 1, ImpactClass::Behaviour),
        ]);
        let first = build(request(&analysis, None, &no_limits()));
        let second = build(request(&analysis, None, &no_limits()));
        assert_eq!(first.nodes, second.nodes);
        assert_eq!(first.edges, second.edges);
        assert_eq!(first.collapsed, second.collapsed);
    }
}
