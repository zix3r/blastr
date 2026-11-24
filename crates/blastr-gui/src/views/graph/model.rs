//! What the graph shows: one change and the items it reaches, through the
//! steps of their evidence paths, as layers from left to right.
//!
//! A node with many children shows the first few and a "+N more" node that
//! shows the rest when clicked.

use std::collections::{HashMap, HashSet};

use blastr_model::{Analysis, Certainty, Confidence, ImpactClass, SymbolPath, wording};

/// How many children of one node are shown before the rest are folded.
pub(crate) const CHILDREN: usize = 6;

/// How many nodes the graph shows at most before folding everything else.
pub(crate) const MAX_NODES: usize = 150;

/// What a node stands for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NodeKind {
    /// The change.
    Change,
    /// An impact item.
    Item {
        class: ImpactClass,
        confidence: Confidence,
        is_test: bool,
    },
    /// A symbol on a path that the lists do not show.
    Step,
    /// The children of `parent` that are folded.
    More { parent: SymbolPath, hidden: usize },
}

/// One box of the graph.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Node {
    /// The symbol, except for a "+N more" node.
    pub symbol: Option<SymbolPath>,
    pub name: String,
    pub subtitle: String,
    pub kind: NodeKind,
    /// The column it is drawn in.
    pub layer: usize,
    /// Whether it lies on the selected item's path.
    pub on_path: bool,
}

/// A reference from one node to the next, by their indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Edge {
    pub from: usize,
    pub to: usize,
    /// Whether the step could not be resolved to one target.
    pub candidate: bool,
    /// Whether it lies on the selected item's path.
    pub highlight: bool,
}

/// The graph of one change.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Model {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// How many items the change reaches, shown or not.
    pub reached: usize,
}

impl Model {
    /// The node indices of each layer, in order.
    pub(crate) fn layers(&self) -> Vec<Vec<usize>> {
        let depth = self
            .nodes
            .iter()
            .map(|node| node.layer + 1)
            .max()
            .unwrap_or(0);
        let mut layers = vec![Vec::new(); depth];
        for (index, node) in self.nodes.iter().enumerate() {
            layers[node.layer].push(index);
        }
        layers
    }
}

/// Builds the graph of the change at `change`, listing items at or above
/// `min_confidence`. `expanded` are the nodes whose children are all shown;
/// the path of `selected` is highlighted and never folded.
pub(crate) fn build(
    analysis: &Analysis,
    change: usize,
    min_confidence: Confidence,
    expanded: &HashSet<SymbolPath>,
    selected: Option<&SymbolPath>,
) -> Model {
    let Some(root) = analysis.diff.changes.get(change) else {
        return Model::default();
    };
    // The children of every symbol, in the order the items are ranked, and
    // whether the step to each is a candidate.
    let mut children: HashMap<&SymbolPath, Vec<(&SymbolPath, bool)>> = HashMap::new();
    let mut highlighted: Vec<&SymbolPath> = Vec::new();
    let mut reached = 0;
    for item in &analysis.items {
        if item.confidence < min_confidence {
            continue;
        }
        let Some(path) = item
            .paths
            .iter()
            .find(|path| path.change as usize == change)
        else {
            continue;
        };
        reached += 1;
        let mut parent = &root.symbol;
        let mut chain = vec![parent];
        for step in &path.steps {
            let list = children.entry(parent).or_default();
            if !list.iter().any(|(known, _)| *known == &step.from) {
                list.push((&step.from, step.certainty == Certainty::Candidate));
            }
            parent = &step.from;
            chain.push(parent);
        }
        if selected == Some(&item.symbol) {
            highlighted = chain;
        }
    }

    // The column of each symbol: its distance from the change.
    let mut layer: HashMap<&SymbolPath, usize> = HashMap::from([(&root.symbol, 0)]);
    let mut queue = std::collections::VecDeque::from([&root.symbol]);
    while let Some(symbol) = queue.pop_front() {
        let next = layer[symbol] + 1;
        for (child, _) in children.get(symbol).into_iter().flatten() {
            if !layer.contains_key(child) {
                layer.insert(child, next);
                queue.push_back(child);
            }
        }
    }

    let items: HashMap<&SymbolPath, &blastr_model::ImpactItem> = analysis
        .items
        .iter()
        .map(|item| (&item.symbol, item))
        .collect();
    let mut model = Model {
        reached,
        ..Model::default()
    };
    let mut index: HashMap<&SymbolPath, usize> = HashMap::new();
    let add = |model: &mut Model, symbol: &SymbolPath, kind: NodeKind, layer: usize| {
        let short = wording::short_name(&symbol.path);
        model.nodes.push(Node {
            symbol: Some(symbol.clone()),
            name: short.name,
            subtitle: short.module,
            kind,
            layer,
            on_path: highlighted.contains(&symbol),
        });
        model.nodes.len() - 1
    };
    let root_index = add(&mut model, &root.symbol, NodeKind::Change, 0);
    index.insert(&root.symbol, root_index);

    // Shows the nodes breadth first, folding the children past the limit.
    let mut queue = std::collections::VecDeque::from([&root.symbol]);
    while let Some(parent) = queue.pop_front() {
        let from = index[parent];
        let next = layer[parent] + 1;
        let list: Vec<&(&SymbolPath, bool)> = children
            .get(parent)
            .into_iter()
            .flatten()
            .filter(|(child, _)| layer[child] == next)
            .collect();
        let open = expanded.contains(parent);
        let mut hidden = 0;
        for (shown, (child, candidate)) in list.iter().enumerate() {
            let known = index.get(child).copied();
            let forced = highlighted.contains(child);
            let room = open || shown < CHILDREN;
            let to = match known {
                Some(to) => to,
                None if forced || (room && model.nodes.len() < MAX_NODES) => {
                    let kind = match items.get(child) {
                        Some(item) => NodeKind::Item {
                            class: item.class,
                            confidence: item.confidence,
                            is_test: item.tags.is_test,
                        },
                        None => NodeKind::Step,
                    };
                    let to = add(&mut model, child, kind, next);
                    index.insert(child, to);
                    queue.push_back(child);
                    to
                }
                None => {
                    hidden += 1;
                    continue;
                }
            };
            let highlight = model.nodes[from].on_path && model.nodes[to].on_path;
            model.edges.push(Edge {
                from,
                to,
                candidate: *candidate,
                highlight,
            });
        }
        if hidden > 0 {
            model.nodes.push(Node {
                symbol: None,
                name: format!("+{hidden} more"),
                subtitle: String::new(),
                kind: NodeKind::More {
                    parent: parent.clone(),
                    hidden,
                },
                layer: next,
                on_path: false,
            });
            let to = model.nodes.len() - 1;
            model.edges.push(Edge {
                from,
                to,
                candidate: false,
                highlight: false,
            });
        }
    }
    model
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        AnalysisParams, Change, ChangeDetail, ChangeKind, Comparison, DiffResult, EdgeDetail,
        EdgeKind, EvidencePath, EvidenceStep, ImpactItem, ImpactLimits, ImpactTags, Location,
        Reason, RelPath, Source, Span, SymbolKind, TestSelection,
    };

    use super::*;

    fn symbol(name: &str) -> SymbolPath {
        SymbolPath::new(format!("k::{name}"), SymbolKind::Function)
    }

    fn site() -> Location {
        Location {
            file: RelPath::new("src/lib.rs"),
            span: Span {
                start: 0,
                end: 1,
                start_line: 1,
                end_line: 1,
            },
        }
    }

    /// An item reached from change `change` through `chain`, nearest first.
    fn item(change: u32, chain: &[&str]) -> ImpactItem {
        let mut to = symbol(if change == 0 { "changed" } else { "other" });
        let steps = chain
            .iter()
            .map(|name| {
                let step = EvidenceStep {
                    from: symbol(name),
                    to: to.clone(),
                    edge: EdgeKind::Calls,
                    certainty: Certainty::Resolved,
                    detail: EdgeDetail::None,
                    site: site(),
                };
                to = symbol(name);
                step
            })
            .collect();
        ImpactItem {
            symbol: symbol(chain.last().expect("a step")),
            location: site(),
            class: ImpactClass::Behaviour,
            confidence: Confidence::Likely,
            distance: chain.len() as u32,
            paths: vec![EvidencePath {
                change,
                reason: Reason::CallsChanged,
                class: ImpactClass::Behaviour,
                steps,
            }],
            path_count: 1,
            tags: ImpactTags::default(),
        }
    }

    fn analysis(items: Vec<ImpactItem>) -> Analysis {
        let change = |name: &str| Change {
            kind: ChangeKind::Body,
            symbol: symbol(name),
            old: None,
            new: Some(site()),
            detail: ChangeDetail::None,
        };
        Analysis {
            diff: DiffResult {
                comparison: Comparison {
                    base: Source::Worktree,
                    head: Source::Worktree,
                },
                base_key: None,
                head_key: None,
                changed_files: Vec::new(),
                changes: vec![change("changed"), change("other")],
                limits: Vec::new(),
            },
            items,
            tests: TestSelection::default(),
            limits: ImpactLimits::default(),
            params: AnalysisParams::default(),
        }
    }

    fn names(model: &Model) -> Vec<(String, usize)> {
        model
            .nodes
            .iter()
            .map(|node| (node.name.clone(), node.layer))
            .collect()
    }

    #[test]
    fn only_the_change_and_what_it_reaches_are_shown() {
        let analysis = analysis(vec![
            item(0, &["a"]),
            item(0, &["a", "b"]),
            item(1, &["elsewhere"]),
        ]);
        let model = build(&analysis, 0, Confidence::Possible, &HashSet::new(), None);
        assert_eq!(
            names(&model),
            vec![("changed".into(), 0), ("a".into(), 1), ("b".into(), 2)]
        );
        assert_eq!(model.edges.len(), 2);
        assert_eq!(model.reached, 2);
    }

    #[test]
    fn many_children_fold_into_one_node_that_expands() {
        let names_of: Vec<String> = (0..10).map(|index| format!("n{index}")).collect();
        let items = names_of.iter().map(|name| item(0, &[name])).collect();
        let analysis = analysis(items);
        let model = build(&analysis, 0, Confidence::Possible, &HashSet::new(), None);
        assert_eq!(model.nodes.len(), 1 + CHILDREN + 1);
        let more = model.nodes.last().expect("a node");
        assert_eq!(more.name, "+4 more");
        assert!(matches!(more.kind, NodeKind::More { hidden: 4, .. }));

        let expanded = HashSet::from([symbol("changed")]);
        let model = build(&analysis, 0, Confidence::Possible, &expanded, None);
        assert_eq!(model.nodes.len(), 11);
    }

    #[test]
    fn the_selected_path_is_highlighted_and_never_folded() {
        let names_of: Vec<String> = (0..10).map(|index| format!("n{index}")).collect();
        let mut items: Vec<ImpactItem> = names_of.iter().map(|name| item(0, &[name])).collect();
        items.push(item(0, &["n9", "deep"]));
        let analysis = analysis(items);
        let model = build(
            &analysis,
            0,
            Confidence::Possible,
            &HashSet::new(),
            Some(&symbol("deep")),
        );
        let deep = model
            .nodes
            .iter()
            .position(|node| node.name == "deep")
            .expect("the selected item is shown");
        assert!(model.nodes[deep].on_path);
        let highlighted: Vec<&Edge> = model.edges.iter().filter(|edge| edge.highlight).collect();
        assert_eq!(highlighted.len(), 2);
    }
}
