//! Mapping a compiler error to the symbol it belongs to.
//!
//! The compiler reports a byte range in a file; Blastr reports symbols. The
//! bridge between them is the graph of the **edited** revision: every symbol
//! there knows the file and byte range of its whole definition, in exactly the
//! text the compiler read, because the harness hands the same edited text to
//! rust-analyzer and to `cargo check`.
//!
//! The enclosing symbol of an error is the symbol with the smallest range that
//! still contains the first byte of the error's primary span. Smallest, because
//! ranges nest — a method sits inside an `impl`, which sits inside a module, all
//! in the same file — and the innermost one is the item that has to be edited.
//! Symbols with an empty range (the crate root) are never chosen: they contain
//! nothing.
//!
//! An error whose file is not a file of the project (a dependency, the standard
//! library) or whose range is not inside any symbol (an error about a file as a
//! whole) is not mapped, and is counted separately rather than guessed at.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use blastr_model::{RelPath, RevisionGraph, SymbolPath};

/// Where one compiler error belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// The error is inside this symbol.
    Symbol(SymbolPath),
    /// The error is in a file of the project but not inside any symbol.
    FileOnly(RelPath),
    /// The error is in a file that is not part of the analysed project.
    Outside,
}

/// One symbol's span, for the span index.
#[derive(Debug, Clone)]
struct SpanEntry {
    /// First byte of the definition.
    start: u32,
    /// One past its last byte.
    end: u32,
    /// The symbol the span belongs to.
    path: SymbolPath,
}

/// The symbols of a graph by file, sorted by where they start.
#[derive(Debug, Default)]
pub struct SpanIndex {
    /// Per file, the symbols that have a span of their own.
    by_file: BTreeMap<RelPath, Vec<SpanEntry>>,
}

impl SpanIndex {
    /// Builds the index of one graph.
    pub fn new(graph: &RevisionGraph) -> SpanIndex {
        let mut by_file: BTreeMap<RelPath, Vec<SpanEntry>> = BTreeMap::new();
        for symbol in &graph.symbols {
            let span = symbol.location.span;
            // An empty range contains nothing: the crate root module has one,
            // and so do symbols that only a macro names.
            if span.end <= span.start {
                continue;
            }
            by_file
                .entry(symbol.location.file.clone())
                .or_default()
                .push(SpanEntry {
                    start: span.start,
                    end: span.end,
                    path: symbol.path.clone(),
                });
        }
        // Sorting here keeps every later answer independent of graph order.
        for list in by_file.values_mut() {
            list.sort_by_key(|entry| (entry.start, entry.end));
        }
        SpanIndex { by_file }
    }

    /// The innermost symbol of `file` whose range contains the byte range
    /// `start..end`. An empty range is matched when it lies inside the symbol,
    /// so that an error pointing at a position still maps.
    pub fn enclosing(&self, file: &RelPath, start: u32, end: u32) -> Option<&SymbolPath> {
        let candidates = self.by_file.get(file)?;
        let mut best: Option<(u32, &SymbolPath)> = None;
        for entry in candidates {
            let contains = if start == end {
                entry.start <= start && start < entry.end
            } else {
                entry.start <= start && end <= entry.end
            };
            if !contains {
                continue;
            }
            let width = entry.end - entry.start;
            if best.is_none_or(|(best_width, _)| width < best_width) {
                best = Some((width, &entry.path));
            }
        }
        best.map(|(_, path)| path)
    }
}

/// Turns an absolute file path printed by the compiler into the path the graph
/// uses: relative to `root`, with `/` separators.
pub fn rel_path(root: &Path, file: &Path) -> Option<RelPath> {
    if let Some(relative) = relative_to(&normalize(root), &normalize(file)) {
        return Some(relative);
    }
    // The two paths can spell the same directory differently: symlinks or
    // shortened Windows names. Canonicalise before giving up.
    let root = std::fs::canonicalize(root).ok()?;
    let file = std::fs::canonicalize(file).ok()?;
    relative_to(&normalize(&root), &normalize(&file))
}

/// `file` relative to `root`, with `/` separators, when it is below it.
fn relative_to(root: &Path, file: &Path) -> Option<RelPath> {
    let relative = file.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(RelPath::new(parts.join("/")))
}

/// A path without `.` components or a Windows verbatim prefix.
fn normalize(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut out = PathBuf::new();
    for component in Path::new(text).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Where an error of `span_index` belongs, knowing the root the compiler ran in.
pub fn place(span_index: &SpanIndex, root: &Path, file: &Path, start: u32, end: u32) -> Placement {
    let Some(relative) = rel_path(root, file) else {
        return Placement::Outside;
    };
    match span_index.enclosing(&relative, start, end) {
        Some(path) => Placement::Symbol(path.clone()),
        None => Placement::FileOnly(relative),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::{
        CrateId, Edge, Location, Span, Symbol, SymbolFlags, SymbolKind, Visibility,
    };

    /// A symbol at `file` `start..end`.
    fn symbol(path: &str, file: &str, start: u32, end: u32) -> Symbol {
        Symbol {
            path: SymbolPath::new(path, SymbolKind::Function),
            krate: CrateId(0),
            location: Location {
                file: RelPath::new(file),
                span: Span {
                    start,
                    end,
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

    /// A graph of the symbols, without edges.
    fn graph(symbols: Vec<Symbol>) -> RevisionGraph {
        RevisionGraph {
            crates: Vec::new(),
            symbols,
            edges: Vec::<Edge>::new(),
            unresolved: Vec::new(),
            inactive: Vec::new(),
            restricted: false,
        }
    }

    #[test]
    fn the_innermost_symbol_wins() {
        let graph = graph(vec![
            symbol("krate::module", "src/lib.rs", 0, 400),
            symbol("krate::Outer", "src/lib.rs", 10, 300),
            symbol("krate::Outer::inner", "src/lib.rs", 50, 120),
            symbol("krate::other", "src/lib.rs", 320, 390),
        ]);
        let index = SpanIndex::new(&graph);
        let file = RelPath::new("src/lib.rs");
        assert_eq!(
            index
                .enclosing(&file, 60, 70)
                .map(|path| path.path.as_str()),
            Some("krate::Outer::inner")
        );
        assert_eq!(
            index
                .enclosing(&file, 200, 210)
                .map(|path| path.path.as_str()),
            Some("krate::Outer")
        );
        assert_eq!(
            index
                .enclosing(&file, 330, 340)
                .map(|path| path.path.as_str()),
            Some("krate::other")
        );
    }

    #[test]
    fn an_error_outside_every_symbol_maps_to_nothing() {
        let graph = graph(vec![symbol("krate::a", "src/lib.rs", 100, 200)]);
        let index = SpanIndex::new(&graph);
        assert!(
            index
                .enclosing(&RelPath::new("src/lib.rs"), 20, 30)
                .is_none()
        );
        assert!(
            index
                .enclosing(&RelPath::new("src/other.rs"), 120, 130)
                .is_none()
        );
    }

    #[test]
    fn an_empty_error_range_lands_in_its_symbol() {
        let graph = graph(vec![symbol("krate::a", "src/lib.rs", 100, 200)]);
        let index = SpanIndex::new(&graph);
        let file = RelPath::new("src/lib.rs");
        assert_eq!(
            index
                .enclosing(&file, 200, 200)
                .map(|path| path.path.as_str()),
            None,
            "the end of a symbol is not inside it"
        );
        assert_eq!(
            index
                .enclosing(&file, 199, 199)
                .map(|path| path.path.as_str()),
            Some("krate::a")
        );
    }

    #[test]
    fn placements_tell_outside_from_unmapped() {
        let graph = graph(vec![symbol("krate::a", "src/lib.rs", 100, 200)]);
        let index = SpanIndex::new(&graph);
        let root = Path::new("/project");
        assert_eq!(
            place(&index, root, Path::new("/project/src/lib.rs"), 120, 130),
            Placement::Symbol(SymbolPath::new("krate::a", SymbolKind::Function))
        );
        assert_eq!(
            place(&index, root, Path::new("/project/src/lib.rs"), 10, 20),
            Placement::FileOnly(RelPath::new("src/lib.rs"))
        );
        assert_eq!(
            place(&index, root, Path::new("/elsewhere/lib.rs"), 120, 130),
            Placement::Outside
        );
    }

    #[test]
    fn paths_are_made_relative_to_the_project() {
        let root = Path::new("/project");
        assert_eq!(
            rel_path(root, Path::new("/project/src/lib.rs")),
            Some(RelPath::new("src/lib.rs"))
        );
        assert_eq!(
            rel_path(root, Path::new("/project/./src/../src/lib.rs")),
            Some(RelPath::new("src/lib.rs"))
        );
        assert_eq!(rel_path(root, Path::new("/project")), None);
    }
}
