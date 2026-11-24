//! Finding any symbol of the project by name.

use blastr_model::{Location, RevisionGraph, SymbolKind, SymbolPath};

/// One symbol that can be searched for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The symbol.
    pub symbol: SymbolPath,
    /// Where it is defined.
    pub location: Location,
    /// The path in lower case, for matching.
    lower: String,
    /// Byte offset in `lower` at which the last segment starts.
    name_start: usize,
}

impl Entry {
    /// The lower-case name: the last segment of the path.
    fn name(&self) -> &str {
        &self.lower[self.name_start..]
    }
}

/// A search hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Index of the entry in the index.
    pub entry: usize,
    /// How well it matches; lower is better.
    rank: u8,
}

/// Every symbol of one side of a comparison, ready to be searched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolIndex {
    entries: Vec<Entry>,
}

impl SymbolIndex {
    /// Builds the index from a graph. Modules and impl blocks are left out:
    /// nothing is said about them that is not said about their items.
    pub fn from_graph(graph: &RevisionGraph) -> SymbolIndex {
        let mut entries: Vec<Entry> = graph
            .symbols
            .iter()
            .filter(|symbol| !matches!(symbol.path.kind, SymbolKind::Module | SymbolKind::Impl))
            .map(|symbol| entry(symbol.path.clone(), symbol.location.clone()))
            .collect();
        entries.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        entries.dedup_by(|a, b| a.symbol == b.symbol);
        SymbolIndex { entries }
    }

    /// Whether the index holds no symbol.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry at `index`.
    pub fn get(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    /// The entry of exactly this symbol.
    pub fn find(&self, symbol: &SymbolPath) -> Option<&Entry> {
        self.entries
            .binary_search_by(|entry| entry.symbol.cmp(symbol))
            .ok()
            .and_then(|index| self.entries.get(index))
    }

    /// The best `limit` matches of `query`, best first.
    ///
    /// Every word of the query must occur in the path, case ignored. A match
    /// in the name beats a match in the modules, and a name that starts with
    /// the query beats one that only contains it.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let query = query.trim().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let Some(first) = words.first() else {
            return Vec::new();
        };
        let mut hits: Vec<Hit> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| words.iter().all(|word| entry.lower.contains(word)))
            .map(|(index, entry)| {
                let name = entry.name();
                let rank = if name == *first {
                    0
                } else if name.starts_with(first) {
                    1
                } else if name.contains(first) {
                    2
                } else {
                    3
                };
                Hit { entry: index, rank }
            })
            .collect();
        hits.sort_by(|a, b| {
            let (left, right) = (&self.entries[a.entry], &self.entries[b.entry]);
            a.rank
                .cmp(&b.rank)
                .then(left.lower.len().cmp(&right.lower.len()))
                .then(left.symbol.cmp(&right.symbol))
        });
        hits.truncate(limit);
        hits
    }
}

/// The searchable entry of one symbol.
fn entry(symbol: SymbolPath, location: Location) -> Entry {
    let lower = symbol.path.to_lowercase();
    let name_start = name_start(&lower);
    Entry {
        symbol,
        location,
        lower,
        name_start,
    }
}

/// Byte offset of the last path segment, ignoring `::` inside angle brackets.
fn name_start(path: &str) -> usize {
    let bytes = path.as_bytes();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes.get(index + 1) == Some(&b':') => {
                index += 2;
                start = index;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    start
}

/// Splits a path into what comes before its last segment (with the trailing
/// `::`) and the last segment.
pub fn split_name(path: &str) -> (&str, &str) {
    path.split_at(name_start(path))
}

#[cfg(test)]
mod tests {
    use blastr_model::{CrateId, RelPath, Span, Symbol, SymbolFlags, Visibility};

    use super::*;

    fn symbol(path: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            path: SymbolPath::new(path, kind),
            krate: CrateId(0),
            location: Location {
                file: RelPath::new("src/lib.rs"),
                span: Span {
                    start: 0,
                    end: 1,
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

    fn index() -> SymbolIndex {
        let mut graph = RevisionGraph::default();
        for (path, kind) in [
            ("billing", SymbolKind::Module),
            ("billing::invoice::tests::totals", SymbolKind::Function),
            ("api::checkout::total_price", SymbolKind::Function),
            ("billing::tax::total_with_vat", SymbolKind::Function),
            ("billing::total::calculate_total", SymbolKind::Function),
            ("billing::total", SymbolKind::Module),
            ("billing::Total", SymbolKind::Struct),
            ("billing::<impl fmt::Display for Total>", SymbolKind::Impl),
            (
                "billing::<impl fmt::Display for Total>::fmt",
                SymbolKind::Method,
            ),
        ] {
            graph.symbols.push(symbol(path, kind));
        }
        SymbolIndex::from_graph(&graph)
    }

    fn names(index: &SymbolIndex, query: &str) -> Vec<String> {
        index
            .search(query, 10)
            .into_iter()
            .map(|hit| index.get(hit.entry).expect("an entry").symbol.path.clone())
            .collect()
    }

    #[test]
    fn modules_and_impl_blocks_are_not_listed() {
        let index = index();
        assert!(!index.is_empty());
        assert!(
            names(&index, "billing")
                .iter()
                .all(|name| name != "billing")
        );
    }

    #[test]
    fn name_matches_come_before_module_matches() {
        let index = index();
        assert_eq!(
            names(&index, "tot"),
            vec![
                "billing::Total",
                "api::checkout::total_price",
                "billing::tax::total_with_vat",
                "billing::invoice::tests::totals",
                "billing::total::calculate_total",
                "billing::<impl fmt::Display for Total>::fmt",
            ]
        );
    }

    #[test]
    fn an_exact_name_wins_and_case_is_ignored() {
        let index = index();
        assert_eq!(names(&index, "TOTAL")[0], "billing::Total");
        assert_eq!(
            names(&index, "totals")[0],
            "billing::invoice::tests::totals"
        );
    }

    #[test]
    fn every_word_must_match() {
        let index = index();
        assert_eq!(
            names(&index, "total api"),
            vec!["api::checkout::total_price"]
        );
        assert!(names(&index, "total nowhere").is_empty());
        assert!(names(&index, "   ").is_empty());
    }

    #[test]
    fn the_limit_is_respected() {
        assert_eq!(index().search("t", 2).len(), 2);
    }

    #[test]
    fn a_symbol_is_found_by_identity() {
        let index = index();
        let found = index.find(&SymbolPath::new("billing::Total", SymbolKind::Struct));
        assert!(found.is_some());
        assert_eq!(
            index.find(&SymbolPath::new("billing::Total", SymbolKind::Enum)),
            None
        );
    }

    #[test]
    fn the_name_is_the_last_segment_outside_angle_brackets() {
        assert_eq!(split_name("a::b::c"), ("a::b::", "c"));
        assert_eq!(split_name("c"), ("", "c"));
        assert_eq!(
            split_name("a::<impl x::T for y::S>::run"),
            ("a::<impl x::T for y::S>::", "run")
        );
        assert_eq!(
            split_name("a::<impl x::T for y::S>"),
            ("a::", "<impl x::T for y::S>")
        );
    }
}
