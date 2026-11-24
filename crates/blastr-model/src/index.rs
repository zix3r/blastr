//! Lookup tables over a [`RevisionGraph`].

use std::collections::HashMap;

use crate::{RelPath, RevisionGraph, SymbolId, SymbolPath};

/// An empty slice returned by lookups that find nothing.
const NO_U32: &[u32] = &[];
/// An empty slice returned by lookups that find nothing.
const NO_SYMBOL_IDS: &[SymbolId] = &[];

/// Lookup tables built once from a graph. Borrowing nothing, so it can be
/// kept next to the graph it was built from.
#[derive(Debug, Clone, Default)]
pub struct GraphIndex {
    /// Exact identity to symbol id; the first symbol with that identity wins.
    by_path: HashMap<SymbolPath, SymbolId>,
    /// Canonical path text to every symbol carrying it.
    by_text: HashMap<String, Vec<SymbolId>>,
    /// Edge indices leaving a symbol.
    edges_from: Vec<Vec<u32>>,
    /// Edge indices arriving at a symbol.
    edges_to: Vec<Vec<u32>>,
    /// Child symbol ids of a symbol.
    children: Vec<Vec<SymbolId>>,
    /// Symbols located in a file.
    by_file: HashMap<RelPath, Vec<SymbolId>>,
}

impl GraphIndex {
    /// Builds the tables.
    pub fn new(graph: &RevisionGraph) -> Self {
        let symbol_count = graph.symbols.len();

        let mut by_path: HashMap<SymbolPath, SymbolId> = HashMap::new();
        let mut by_text: HashMap<String, Vec<SymbolId>> = HashMap::new();
        let mut children: Vec<Vec<SymbolId>> = vec![Vec::new(); symbol_count];
        let mut by_file: HashMap<RelPath, Vec<SymbolId>> = HashMap::new();

        for (index, symbol) in graph.symbols.iter().enumerate() {
            let id = SymbolId(index as u32);

            by_path.entry(symbol.path.clone()).or_insert(id);
            by_text
                .entry(symbol.path.path.clone())
                .or_default()
                .push(id);
            if let Some(parent) = symbol.parent
                && let Some(parent_children) = children.get_mut(parent.0 as usize)
            {
                parent_children.push(id);
            }
            by_file
                .entry(symbol.location.file.clone())
                .or_default()
                .push(id);
        }

        let mut edges_from: Vec<Vec<u32>> = vec![Vec::new(); symbol_count];
        let mut edges_to: Vec<Vec<u32>> = vec![Vec::new(); symbol_count];

        for (index, edge) in graph.edges.iter().enumerate() {
            let index = index as u32;
            if let Some(from) = edges_from.get_mut(edge.from.0 as usize) {
                from.push(index);
            }
            if let Some(to) = edges_to.get_mut(edge.to.0 as usize) {
                to.push(index);
            }
        }

        for list in by_text.values_mut().chain(by_file.values_mut()) {
            list.sort_unstable();
        }
        for list in children.iter_mut() {
            list.sort_unstable();
        }
        for list in edges_from.iter_mut() {
            list.sort_unstable();
        }
        for list in edges_to.iter_mut() {
            list.sort_unstable();
        }

        Self {
            by_path,
            by_text,
            edges_from,
            edges_to,
            children,
            by_file,
        }
    }

    /// The symbol with exactly this identity.
    pub fn by_path(&self, path: &SymbolPath) -> Option<SymbolId> {
        self.by_path.get(path).copied()
    }

    /// Every symbol whose canonical path text equals `path`, whatever its
    /// kind and disambiguator, in ascending id order.
    pub fn find(&self, path: &str) -> &[SymbolId] {
        self.by_text.get(path).map_or(NO_SYMBOL_IDS, Vec::as_slice)
    }

    /// Indices into `RevisionGraph::edges` of the edges leaving `id`.
    pub fn edges_from(&self, id: SymbolId) -> &[u32] {
        self.edges_from
            .get(id.0 as usize)
            .map_or(NO_U32, Vec::as_slice)
    }

    /// Indices into `RevisionGraph::edges` of the edges arriving at `id`.
    pub fn edges_to(&self, id: SymbolId) -> &[u32] {
        self.edges_to
            .get(id.0 as usize)
            .map_or(NO_U32, Vec::as_slice)
    }

    /// Symbols whose parent is `id`, in ascending id order.
    pub fn children(&self, id: SymbolId) -> &[SymbolId] {
        self.children
            .get(id.0 as usize)
            .map_or(NO_SYMBOL_IDS, Vec::as_slice)
    }

    /// Symbols located in `file`, in ascending id order.
    pub fn symbols_in_file(&self, file: &RelPath) -> &[SymbolId] {
        self.by_file.get(file).map_or(NO_SYMBOL_IDS, Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Certainty, CrateId, Edge, EdgeDetail, EdgeKind, Location, Span, Symbol, SymbolFlags,
        SymbolKind, Visibility,
    };

    /// A location in `file` at the given byte range.
    fn location(file: &str, start: u32, end: u32) -> Location {
        Location {
            file: RelPath::new(file),
            span: Span {
                start,
                end,
                start_line: 1,
                end_line: 1,
            },
        }
    }

    /// A symbol with identity `path`, kind `kind` and the given parent.
    fn symbol(path: &str, kind: SymbolKind, file: &str, parent: Option<SymbolId>) -> Symbol {
        Symbol {
            path: SymbolPath::new(path, kind),
            krate: CrateId(0),
            location: location(file, 0, 10),
            visibility: Visibility::Public,
            signature_hash: 0,
            body_hash: 0,
            attr_hash: 0,
            body_sketch: Vec::new(),
            parent,
            flags: SymbolFlags::default(),
            impl_info: None,
        }
    }

    /// An edge from `from` to `to`.
    fn edge(from: u32, to: u32, kind: EdgeKind) -> Edge {
        Edge {
            from: SymbolId(from),
            to: SymbolId(to),
            kind,
            certainty: Certainty::Resolved,
            site: location("src/lib.rs", 1, 2),
            detail: EdgeDetail::None,
        }
    }

    /// Three symbols in two files: a root module with a function and a type.
    fn sample_graph() -> RevisionGraph {
        let root = SymbolId(0);
        RevisionGraph {
            crates: Vec::new(),
            symbols: vec![
                symbol("mycrate", SymbolKind::Module, "src/lib.rs", None),
                symbol(
                    "mycrate::main",
                    SymbolKind::Function,
                    "src/lib.rs",
                    Some(root),
                ),
                symbol(
                    "mycrate::Point",
                    SymbolKind::Struct,
                    "src/point.rs",
                    Some(root),
                ),
            ],
            edges: vec![
                edge(1, 2, EdgeKind::UsesType),
                edge(0, 1, EdgeKind::Contains),
            ],
            unresolved: Vec::new(),
            inactive: Vec::new(),
            restricted: false,
            failed_queries: 0,
        }
    }

    #[test]
    fn looks_up_symbols_by_identity_and_text() {
        let graph = sample_graph();
        let index = GraphIndex::new(&graph);

        let by_path = index.by_path(&SymbolPath::new("mycrate::main", SymbolKind::Function));
        assert_eq!(by_path, Some(SymbolId(1)));
        assert_eq!(
            index.by_path(&SymbolPath::new("mycrate::main", SymbolKind::Struct)),
            None
        );

        assert_eq!(
            index.find("mycrate::main"),
            &[SymbolId(1)],
            "find ignores kind"
        );
        assert_eq!(index.find("mycrate"), &[SymbolId(0)]);
        assert!(index.find("mycrate::missing").is_empty());
    }

    #[test]
    fn finds_edges_in_both_directions() {
        let graph = sample_graph();
        let index = GraphIndex::new(&graph);

        assert_eq!(index.edges_from(SymbolId(1)), &[0]);
        assert_eq!(index.edges_to(SymbolId(2)), &[0]);
        assert_eq!(index.edges_from(SymbolId(0)), &[1]);
        assert_eq!(index.edges_to(SymbolId(1)), &[1]);

        assert!(index.edges_from(SymbolId(9)).is_empty());
        assert!(index.edges_to(SymbolId(9)).is_empty());
    }

    #[test]
    fn finds_children_in_ascending_order() {
        let graph = sample_graph();
        let index = GraphIndex::new(&graph);

        assert_eq!(index.children(SymbolId(0)), &[SymbolId(1), SymbolId(2)]);
        assert!(index.children(SymbolId(1)).is_empty());
        assert!(index.children(SymbolId(9)).is_empty());
    }

    #[test]
    fn finds_symbols_in_a_file() {
        let graph = sample_graph();
        let index = GraphIndex::new(&graph);

        assert_eq!(
            index.symbols_in_file(&RelPath::new("src/lib.rs")),
            &[SymbolId(0), SymbolId(1)]
        );
        assert_eq!(
            index.symbols_in_file(&RelPath::new("src/point.rs")),
            &[SymbolId(2)]
        );
        assert!(
            index
                .symbols_in_file(&RelPath::new("src/none.rs"))
                .is_empty()
        );
    }

    #[test]
    fn empty_graph_gives_empty_results() {
        let index = GraphIndex::new(&RevisionGraph::default());

        assert!(
            index
                .by_path(&SymbolPath::new("mycrate", SymbolKind::Module))
                .is_none()
        );
        assert!(index.find("mycrate").is_empty());
        assert!(index.children(SymbolId(0)).is_empty());
        assert!(
            index
                .symbols_in_file(&RelPath::new("src/lib.rs"))
                .is_empty()
        );
    }
}
