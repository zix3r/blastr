//! A builder for the revision graphs the diff tests compare.

use blastr_model::{
    ChangedFile, CrateId, CrateInfo, FileStatus, Location, RelPath, RevisionGraph, Span, Symbol,
    SymbolFlags, SymbolId, SymbolKind, SymbolPath, TargetKind, Visibility,
};

/// Builds a [`RevisionGraph`] one symbol at a time.
pub(crate) struct GraphBuilder {
    graph: RevisionGraph,
    krate: CrateId,
}

impl GraphBuilder {
    /// A builder whose graph holds one crate named `krate`.
    pub(crate) fn new() -> Self {
        let mut graph = RevisionGraph::default();
        graph.crates.push(CrateInfo {
            name: "krate".to_owned(),
            package: "krate".to_owned(),
            target_kind: TargetKind::Lib,
            target_name: "krate".to_owned(),
            root_file: RelPath::new("src/lib.rs"),
        });
        GraphBuilder {
            graph,
            krate: CrateId(0),
        }
    }

    /// Adds a symbol in `file` spanning `start..end` and returns a handle to it.
    pub(crate) fn add(
        &mut self,
        path: &str,
        kind: SymbolKind,
        file: &str,
        start: u32,
        end: u32,
    ) -> Sym<'_> {
        let id = SymbolId(self.graph.symbols.len() as u32);
        self.graph.symbols.push(Symbol {
            path: SymbolPath::new(path, kind),
            krate: self.krate,
            location: Location {
                file: RelPath::new(file),
                span: Span {
                    start,
                    end,
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
        Sym {
            graph: &mut self.graph,
            id,
        }
    }

    /// The graph built so far.
    pub(crate) fn finish(self) -> RevisionGraph {
        self.graph
    }
}

/// One symbol while it is being described.
pub(crate) struct Sym<'a> {
    graph: &'a mut RevisionGraph,
    id: SymbolId,
}

impl<'a> Sym<'a> {
    fn symbol(&mut self) -> &mut Symbol {
        &mut self.graph.symbols[self.id.0 as usize]
    }

    /// The id of the symbol, for use as a parent.
    pub(crate) fn id(&self) -> SymbolId {
        self.id
    }

    /// The hash of the interface, which never covers the item's own name:
    /// a renamed item keeps it. Leave it 0 for a `macro_rules!` macro.
    pub(crate) fn sig(mut self, hash: u64) -> Self {
        self.symbol().signature_hash = hash;
        self
    }

    /// Sets both the interface hash (see [`Sym::sig`]) and the body hash.
    pub(crate) fn hashes(mut self, signature: u64, body: u64) -> Self {
        self.symbol().signature_hash = signature;
        self.symbol().body_hash = body;
        self
    }

    /// The hash of the body.
    pub(crate) fn body(mut self, hash: u64) -> Self {
        self.symbol().body_hash = hash;
        self
    }

    /// The sketch of the body.
    pub(crate) fn sketch(mut self, sketch: &[u32]) -> Self {
        self.symbol().body_sketch = sketch.to_vec();
        self
    }

    /// The hash of the attributes.
    pub(crate) fn attr(mut self, hash: u64) -> Self {
        self.symbol().attr_hash = hash;
        self
    }

    /// The declared visibility.
    pub(crate) fn vis(mut self, visibility: Visibility) -> Self {
        self.symbol().visibility = visibility;
        self
    }

    /// The enclosing symbol.
    pub(crate) fn parent(mut self, parent: SymbolId) -> Self {
        self.symbol().parent = Some(parent);
        self
    }

    /// Marks the symbol as declared inside a trait.
    pub(crate) fn in_trait(mut self) -> Self {
        self.symbol().flags.in_trait = true;
        self
    }

    /// Marks the symbol as produced by a macro or derive.
    pub(crate) fn macro_generated(mut self) -> Self {
        self.symbol().flags.from_macro = true;
        self
    }
}

/// A modified file.
pub(crate) fn modified(path: &str) -> ChangedFile {
    ChangedFile {
        path: RelPath::new(path),
        old_path: None,
        status: FileStatus::Modified,
    }
}

/// A file that appeared.
pub(crate) fn added(path: &str) -> ChangedFile {
    ChangedFile {
        path: RelPath::new(path),
        old_path: None,
        status: FileStatus::Added,
    }
}

/// A file that disappeared.
pub(crate) fn deleted(path: &str) -> ChangedFile {
    ChangedFile {
        path: RelPath::new(path),
        old_path: None,
        status: FileStatus::Deleted,
    }
}

/// A file that moved.
pub(crate) fn renamed_file(from: &str, to: &str) -> ChangedFile {
    ChangedFile {
        path: RelPath::new(to),
        old_path: Some(RelPath::new(from)),
        status: FileStatus::Renamed,
    }
}
