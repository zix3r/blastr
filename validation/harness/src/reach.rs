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

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::{
        Certainty, CrateId, Edge, EdgeDetail, Location, RelPath, Span, Symbol, SymbolFlags,
        SymbolKind, SymbolPath, Visibility,
    };

    /// A symbol with an empty location.
    fn symbol(path: &str, kind: SymbolKind) -> Symbol {
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
}
