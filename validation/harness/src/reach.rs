//! Plain reverse reachability: the baseline Blastr's list is compared with.
//!
//! "Everything that transitively depends on the change" is what a reference
//! search would hand back: follow every edge backwards from the changed symbol,
//! whatever the change was and whatever the edge means. `contains` edges are
//! left out, as Blastr leaves them out: they say which module an item is in,
//! not that the module depends on it.

use std::collections::BTreeSet;

use blastr_model::{EdgeKind, GraphIndex, RevisionGraph, SymbolId};

/// Every symbol that depends on `seed`, however indirectly, including the
/// symbol the search started from.
pub fn closure(graph: &RevisionGraph, seeds: &[SymbolId]) -> BTreeSet<SymbolId> {
    let index = GraphIndex::new(graph);
    let mut seen: BTreeSet<SymbolId> = seeds.iter().copied().collect();
    let mut queue: Vec<SymbolId> = seeds.to_vec();
    while let Some(current) = queue.pop() {
        for edge_index in index.edges_to(current) {
            let Some(edge) = graph.edges.get(*edge_index as usize) else {
                continue;
            };
            if edge.kind == EdgeKind::Contains {
                continue;
            }
            if seen.insert(edge.from) {
                queue.push(edge.from);
            }
        }
    }
    seen
}

/// The symbols that depend on `seed`, without the seed itself.
pub fn dependents(graph: &RevisionGraph, seeds: &[SymbolId]) -> BTreeSet<SymbolId> {
    let mut found = closure(graph, seeds);
    for seed in seeds {
        found.remove(seed);
    }
    found
}

/// Why a test a mutation makes fail is or is not in plain reverse reachability.
///
/// For every missed test offer, the shortest chain from the test to `seed`
/// along the graph's own edges, whatever they mean. A chain the plain search
/// has and Blastr's selection does not points at a rule that does not follow
/// one of the edge kinds on it; no chain at all points at an edge the extraction
/// never saw. Either way the line names the case's gap.
pub fn explain(graph: &RevisionGraph, seed: Option<SymbolId>, missed: &[String]) -> Vec<String> {
    let Some(seed) = seed else {
        return vec!["the changed function has no node in the edited graph".to_owned()];
    };
    let mut lines = Vec::new();
    for filter in missed {
        let tests: Vec<SymbolId> = graph
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, symbol)| symbol.flags.is_test && test_filter(&symbol.path.path) == *filter)
            .map(|(index, _)| SymbolId(index as u32))
            .collect();
        if tests.is_empty() {
            lines.push(format!(
                "{filter}: no test symbol in the edited graph carries this name"
            ));
            continue;
        }
        for test in tests {
            let name = &graph.symbols[test.0 as usize].path.path;
            match chain(graph, test, seed) {
                Some(steps) => {
                    lines.push(format!("{filter}: plain reverse path from {name}: {steps}"))
                }
                None => lines.push(format!(
                    "{filter}: {name} does not reach the changed function on the graph's edges"
                )),
            }
        }
    }
    lines
}

/// The shortest chain of edges leading from `from` to `to`, rendered.
fn chain(graph: &RevisionGraph, from: SymbolId, to: SymbolId) -> Option<String> {
    if from == to {
        return Some(String::from("the test is the changed function"));
    }
    let index = GraphIndex::new(graph);
    let mut previous: Vec<Option<u32>> = vec![None; graph.symbols.len()];
    let mut seen: BTreeSet<SymbolId> = BTreeSet::new();
    let mut queue: std::collections::VecDeque<SymbolId> = std::collections::VecDeque::new();
    seen.insert(from);
    queue.push_back(from);
    while let Some(current) = queue.pop_front() {
        for edge_index in index.edges_from(current) {
            let Some(edge) = graph.edges.get(*edge_index as usize) else {
                continue;
            };
            if edge.kind == EdgeKind::Contains || !seen.insert(edge.to) {
                continue;
            }
            previous[edge.to.0 as usize] = Some(*edge_index);
            if edge.to == to {
                return Some(render_chain(graph, &previous, from, to));
            }
            queue.push_back(edge.to);
        }
    }
    None
}

/// Walks the predecessors of `to` back to `from` and renders every step.
fn render_chain(
    graph: &RevisionGraph,
    previous: &[Option<u32>],
    from: SymbolId,
    to: SymbolId,
) -> String {
    let mut steps = Vec::new();
    let mut current = to;
    while current != from {
        let Some(edge_index) = previous[current.0 as usize] else {
            break;
        };
        let Some(edge) = graph.edges.get(edge_index as usize) else {
            break;
        };
        steps.push(format!(
            "{} -[{}]-> {}",
            graph.symbols[edge.from.0 as usize].path.path,
            edge.kind,
            graph.symbols[edge.to.0 as usize].path.path
        ));
        current = edge.from;
    }
    steps.reverse();
    steps.join(", then ")
}

/// The name `cargo test` matches a test by: its path without the crate segment.
fn test_filter(path: &str) -> &str {
    match path.split_once("::") {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::{
        Certainty, CrateId, Edge, EdgeDetail, Location, RelPath, Span, Symbol, SymbolFlags,
        SymbolKind, SymbolPath, Visibility,
    };

    /// A symbol with an empty location. A path ending in `!` names a test.
    fn symbol(path: &str, kind: SymbolKind) -> Symbol {
        let mut symbol = plain_symbol(path.trim_end_matches('!'), kind);
        if path.ends_with('!') {
            symbol.flags.is_test = true;
        }
        symbol
    }

    /// A symbol with an empty location and no flags.
    fn plain_symbol(path: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            path: SymbolPath::new(path, kind),
            krate: CrateId(0),
            location: Location {
                file: RelPath::new("src/lib.rs"),
                span: Span {
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 1,
                },
            },
            visibility: Visibility::Public,
            signature_hash: 0,
            body_hash: 0,
            attr_hash: 0,
            body_sketch: Vec::new(),
            parent: None,
            flags: SymbolFlags::default(),
            impl_info: None,
        }
    }

    /// An edge `from` -> `to`.
    fn edge(from: u32, to: u32, kind: EdgeKind) -> Edge {
        Edge {
            from: SymbolId(from),
            to: SymbolId(to),
            kind,
            certainty: Certainty::Resolved,
            site: Location {
                file: RelPath::new("src/lib.rs"),
                span: Span {
                    start: 0,
                    end: 0,
                    start_line: 1,
                    end_line: 1,
                },
            },
            detail: EdgeDetail::None,
        }
    }

    /// `main` calls `middle`, which calls `leaf`; the module contains all three.
    fn chain() -> RevisionGraph {
        RevisionGraph {
            crates: Vec::new(),
            symbols: vec![
                symbol("krate", SymbolKind::Module),
                symbol("krate::main", SymbolKind::Function),
                symbol("krate::middle", SymbolKind::Function),
                symbol("krate::leaf", SymbolKind::Function),
                symbol("krate::unrelated", SymbolKind::Function),
            ],
            edges: vec![
                edge(0, 1, EdgeKind::Contains),
                edge(0, 2, EdgeKind::Contains),
                edge(0, 3, EdgeKind::Contains),
                edge(0, 4, EdgeKind::Contains),
                edge(1, 2, EdgeKind::Calls),
                edge(2, 3, EdgeKind::Calls),
            ],
            unresolved: Vec::new(),
            inactive: Vec::new(),
            restricted: false,
        }
    }

    #[test]
    fn the_closure_follows_every_edge_backwards() {
        let graph = chain();
        let found = closure(&graph, &[SymbolId(3)]);
        let paths: Vec<&str> = found
            .iter()
            .map(|id| graph.symbols[id.0 as usize].path.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["krate::main", "krate::middle", "krate::leaf"],
            "the containing module is not a dependent"
        );
    }

    #[test]
    fn dependents_leave_the_seed_out() {
        let graph = chain();
        let found = dependents(&graph, &[SymbolId(2)]);
        assert!(!found.contains(&SymbolId(2)));
        assert!(found.contains(&SymbolId(1)));
        assert!(
            !found.contains(&SymbolId(3)),
            "what the changed item calls is not a dependent"
        );
    }

    #[test]
    fn a_symbol_nothing_refers_to_has_no_dependents() {
        let graph = chain();
        assert!(dependents(&graph, &[SymbolId(4)]).is_empty());
    }

    #[test]
    fn a_missed_test_is_explained_by_the_chain_to_the_change() {
        let mut graph = chain();
        graph.symbols[1].flags.is_test = true;
        let lines = explain(&graph, Some(SymbolId(3)), &["main".to_owned()]);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("plain reverse path"), "{lines:?}");
        assert!(
            lines[0].contains("krate::main -[calls]-> krate::middle")
                && lines[0].contains("krate::middle -[calls]-> krate::leaf"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_miss_without_a_chain_names_the_gap() {
        let mut graph = chain();
        graph.symbols[1].flags.is_test = true;
        let lines = explain(&graph, Some(SymbolId(4)), &["main".to_owned()]);
        assert!(
            lines[0].contains("does not reach the changed function"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_miss_no_test_symbol_carries_is_named_as_such() {
        let mut graph = chain();
        graph.symbols[1].flags.is_test = true;
        let lines = explain(&graph, Some(SymbolId(3)), &["no_such_test".to_owned()]);
        assert!(lines[0].contains("no test symbol"), "{lines:?}");
        let lines = explain(&graph, None, &["main".to_owned()]);
        assert!(lines[0].contains("no node"), "{lines:?}");
    }
}
