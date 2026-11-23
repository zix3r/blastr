//! Proof that the overlay path and the two-load path agree.
//!
//! The overlay reaches the head graph by loading the project once and applying
//! the changed files to it; the two-load path loads both revisions from
//! scratch. Both must produce exactly the same graphs: the same crates, and for
//! every symbol its path, kind, disambiguator, crate, location, visibility,
//! flags, hashes, sketch, parent and impl info, plus the same edges, unresolved
//! references and inactive ranges.
//!
//! `blastr_git` cannot create commits, so the two revisions are two directories
//! written into a temporary directory: the overlay side goes through
//! [`Engine::overlay_dirs`], which is the code path [`Engine::analyze`] uses for a
//! repository, and the full side through [`Engine::diff_dirs`].

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use blastr_core::Engine;
use blastr_model::{
    AnalysisOptions, CancelToken, EdgeKind, Progress, RevisionGraph, SymbolKind, SymbolPath,
};

/// The crate of the base revision, as `(path, text)` pairs.
const BASE: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[package]\nname = \"overlay_app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    ),
    (
        "src/lib.rs",
        "mod extra;\nmod legacy;\n\n\
         pub fn total() -> i64 {\n    used(2) + 3\n}\n\n\
         pub fn used(value: i64) -> i64 {\n    value * 2\n}\n\n\
         pub fn legacy_name() -> &'static str {\n    legacy::name()\n}\n\n\
         pub fn untouched() -> i64 {\n    7\n}\n",
    ),
    (
        "src/extra.rs",
        "pub fn boosted(value: i64) -> i64 {\n    value + 1\n}\n",
    ),
    (
        "src/legacy.rs",
        "pub fn name() -> &'static str {\n    \"legacy\"\n}\n",
    ),
];

/// The crate of the head revision: `total` got a different body and calls the
/// added module, `used` gained whitespace only, `legacy` was deleted.
const HEAD: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[package]\nname = \"overlay_app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    ),
    (
        "src/lib.rs",
        "mod added;\nmod extra;\n\n\
         pub fn total() -> i64 {\n    used(2) + added::added_value(1)\n}\n\n\
         pub fn used(value: i64) -> i64 {\n    value   *   2\n}\n\n\
         pub fn untouched() -> i64 {\n    7\n}\n",
    ),
    (
        "src/extra.rs",
        "pub fn boosted(value: i64) -> i64 {\n    value + 1\n}\n",
    ),
    (
        "src/added.rs",
        "pub fn added_value(value: i64) -> i64 {\n    value + 100\n}\n",
    ),
];

/// Runs the two paths over one pair of directories and compares the graphs.
#[test]
fn overlay_and_two_loads_extract_the_same_graphs() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let base = dir.path().join("base");
    let head = dir.path().join("head");
    write_project(&base, BASE);
    write_project(&head, HEAD);

    let engine = engine(dir.path());
    let options = AnalysisOptions::default();
    let cancel = CancelToken::new();

    // First, because a load writes a `Cargo.lock` into the directory it loads.
    let overlay_phases = Phases::default();
    let (overlay_base, overlay_head) = engine
        .overlay_dirs(&base, &head, &options, &overlay_phases, &cancel)
        .expect("the overlay runs")
        .expect("both directories are the same project");

    // The overlay loads the project once: no `load head` phase.
    assert!(overlay_phases.has("load base"), "{overlay_phases:?}");
    assert!(overlay_phases.has("extract base"), "{overlay_phases:?}");
    assert!(overlay_phases.has("extract head"), "{overlay_phases:?}");
    assert!(!overlay_phases.has("load head"), "{overlay_phases:?}");

    let load_phases = Phases::default();
    let full = engine
        .diff_dirs(&base, &head, &options, &load_phases, &cancel)
        .expect("the two loads run");
    assert!(load_phases.has("load base"), "{load_phases:?}");
    assert!(load_phases.has("load head"), "{load_phases:?}");

    assert_same_graph(&overlay_base, &full.base, "base");
    assert_same_graph(&overlay_head, &full.head, "head");

    // The comparison above would also pass on two empty graphs, so check that the
    // overlay really saw the changed, added, deleted and whitespace-only text.
    assert_eq!(overlay_base.symbols.len(), full.base.symbols.len());
    assert_eq!(overlay_head.symbols.len(), full.head.symbols.len());

    let added = path("overlay_app::added::added_value");
    assert!(
        !has_symbol(&overlay_base, &added),
        "the added module is new"
    );
    assert!(has_symbol(&overlay_head, &added), "the head has the module");

    let legacy = path("overlay_app::legacy::name");
    assert!(
        has_symbol(&overlay_base, &legacy),
        "the base has the module"
    );
    assert!(
        !has_symbol(&overlay_head, &legacy),
        "the head no longer has the deleted module"
    );

    let total = path("overlay_app::total");
    let base_total = symbol(&overlay_base, &total);
    let head_total = symbol(&overlay_head, &total);
    assert_ne!(
        base_total.body_hash, head_total.body_hash,
        "the changed body must be visible"
    );

    let used = path("overlay_app::used");
    let base_used = symbol(&overlay_base, &used);
    let head_used = symbol(&overlay_head, &used);
    assert_eq!(
        (base_used.signature_hash, base_used.body_hash),
        (head_used.signature_hash, head_used.body_hash),
        "a whitespace-only change is not a change"
    );

    // The new call in `total` is resolved against the added module.
    let calls_from_total: Vec<&str> = overlay_head
        .edges
        .iter()
        .filter(|edge| {
            edge.kind == EdgeKind::Calls
                && overlay_head.symbols[edge.from.0 as usize].path.path == total.path
        })
        .map(|edge| overlay_head.symbols[edge.to.0 as usize].path.path.as_str())
        .collect();
    assert!(
        calls_from_total.contains(&added.path.as_str()),
        "`total` calls `added_value`; calls from `total`: {calls_from_total:?}"
    );
}

/// Checks that a different `Cargo.toml` sends the engine down the full path.
#[test]
fn a_different_manifest_takes_the_full_path() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let base = dir.path().join("base");
    let head = dir.path().join("head");
    write_project(&base, BASE);
    // The same crate with one more line in its manifest: the project model of
    // the base side no longer describes the head side.
    let mut changed = Vec::from(HEAD);
    changed[0] = (
        "Cargo.toml",
        "[package]\nname = \"overlay_app\"\nversion = \"0.2.0\"\nedition = \"2024\"\n",
    );
    write_project(&head, &changed);

    let engine = engine(dir.path());
    let options = AnalysisOptions::default();
    let cancel = CancelToken::new();

    let overlay = engine
        .overlay_dirs(&base, &head, &options, &Phases::default(), &cancel)
        .expect("the overlay attempt runs");
    assert!(
        overlay.is_none(),
        "the overlay must not be used when `Cargo.toml` differs"
    );

    // The fallback the engine takes instead loads both sides.
    let phases = Phases::default();
    let full = engine
        .diff_dirs(&base, &head, &options, &phases, &cancel)
        .expect("the two loads run");
    assert!(phases.has("load base"), "{phases:?}");
    assert!(phases.has("load head"), "{phases:?}");
    assert!(!full.base.symbols.is_empty(), "the base graph is not empty");
    assert!(!full.head.symbols.is_empty(), "the head graph is not empty");
}

/// Compares two graphs field by field, so that a failure names the part that differs.
fn assert_same_graph(actual: &RevisionGraph, expected: &RevisionGraph, side: &str) {
    assert_eq!(actual.crates, expected.crates, "{side}: crates differ");
    assert_eq!(actual.symbols, expected.symbols, "{side}: symbols differ");
    assert_eq!(actual.edges, expected.edges, "{side}: edges differ");
    assert_eq!(
        actual.unresolved, expected.unresolved,
        "{side}: unresolved references differ"
    );
    assert_eq!(
        actual.inactive, expected.inactive,
        "{side}: inactive ranges differ"
    );
    assert_eq!(
        actual.restricted, expected.restricted,
        "{side}: the execution mode differs"
    );
    assert_eq!(actual, expected, "{side}: the graphs differ");
}

/// Canonical path text with the kind it must have.
fn path(text: &str) -> SymbolPath {
    SymbolPath::new(text, SymbolKind::Function)
}

/// True when `graph` holds a symbol whose path text is `path`'s.
fn has_symbol(graph: &RevisionGraph, path: &SymbolPath) -> bool {
    graph
        .symbols
        .iter()
        .any(|symbol| symbol.path.path == path.path)
}

/// The symbol at `path`, which must exist.
fn symbol<'a>(graph: &'a RevisionGraph, path: &SymbolPath) -> &'a blastr_model::Symbol {
    graph
        .symbols
        .iter()
        .find(|symbol| symbol.path.path == path.path)
        .expect("the symbol exists")
}

/// Writes the files of one revision below `root`.
fn write_project(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let file = root.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).expect("the directory is created");
        }
        fs::write(&file, text).expect("the file is written");
    }
}

/// An engine whose cache and target directory live in `dir`, so that the tests
/// do not share one.
fn engine(dir: &Path) -> Engine {
    let paths = blastr_store::Paths {
        cache_dir: dir.join("cache"),
        config_dir: dir.join("config"),
    };
    let store = blastr_store::Store::open_in_memory().expect("an in-memory store");
    Engine::with_store_and_paths(store, paths)
}

/// Records the progress phases the engine reports.
#[derive(Debug, Default)]
struct Phases(Mutex<Vec<String>>);

impl Phases {
    /// True when `phase` was reported.
    fn has(&self, phase: &str) -> bool {
        self.0
            .lock()
            .expect("the lock is not poisoned")
            .iter()
            .any(|reported| reported == phase)
    }
}

impl Progress for Phases {
    fn report(&self, phase: &str, _done: u64, _total: Option<u64>) {
        self.0
            .lock()
            .expect("the lock is not poisoned")
            .push(phase.to_owned());
    }
}
