//! Proof that the overlay path and the two-load path agree.
//!
//! The overlay reaches the head graph by loading the project once and applying
//! the changed files to it; the two-load path loads both revisions from
//! scratch. Both must produce exactly the same graphs: the same crates, and for
//! every symbol its path, kind, disambiguator, crate, location, visibility,
//! flags, hashes, sketch, parent and impl info, plus the same edges, unresolved
//! references and inactive ranges.
//!
//! The two revisions are two commits of a temporary repository. One engine
//! analyses the comparison, which takes the overlay path; a second engine with
//! a cache of its own asks for each graph on its own, which loads each side.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use blastr_core::{AnalysisRequest, Engine};
use blastr_model::{
    AnalysisOptions, AnalysisParams, CancelToken, Comparison, EdgeKind, Progress, RevSpec,
    RevisionGraph, SymbolKind, SymbolPath,
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

/// The comparison of the last commit with its parent.
fn last_commit() -> RevSpec {
    RevSpec {
        rev: Some("HEAD~1..HEAD".to_owned()),
        ..RevSpec::default()
    }
}

/// Analyses the last commit of `repo` with `params`, recording the phases, and
/// returns the comparison.
fn analyze(engine: &Engine, repo: &Path, params: &AnalysisParams, phases: &Phases) -> Comparison {
    let request = AnalysisRequest {
        repo_root: repo,
        spec: &last_commit(),
        options: &AnalysisOptions::default(),
        params,
    };
    engine
        .analyze(&request, phases, &CancelToken::new(), &mut |_| {})
        .expect("the analysis runs")
        .diff
        .comparison
}

/// The graphs of both sides of `comparison`, each fetched on its own.
fn graphs(
    engine: &Engine,
    repo: &Path,
    comparison: &Comparison,
    phases: &Phases,
) -> [RevisionGraph; 2] {
    [&comparison.base, &comparison.head].map(|source| {
        let options = AnalysisOptions::default();
        engine
            .graph(repo, source, &options, phases, &CancelToken::new())
            .expect("the graph is fetched")
            .as_ref()
            .clone()
    })
}

/// Runs the two paths over one pair of revisions and compares the graphs.
#[test]
fn overlay_and_two_loads_extract_the_same_graphs() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let repo = dir.path().join("repo");
    repository(&repo, BASE, HEAD);
    let params = AnalysisParams::default();

    // The overlay loads the project once: no `load head` phase.
    let overlay = engine(&dir.path().join("overlay"));
    let phases = Phases::default();
    let comparison = analyze(&overlay, &repo, &params, &phases);
    assert!(phases.has("load base"), "{phases:?}");
    assert!(phases.has("extract base"), "{phases:?}");
    assert!(phases.has("extract head"), "{phases:?}");
    assert!(!phases.has("load head"), "{phases:?}");

    // Both graphs were stored: reading them, or analysing the comparison again
    // under other parameters, loads nothing.
    let cached = Phases::default();
    let [overlay_base, overlay_head] = graphs(&overlay, &repo, &comparison, &cached);
    let deeper = AnalysisParams {
        depth: params.depth + 1,
        ..params.clone()
    };
    analyze(&overlay, &repo, &deeper, &cached);
    assert!(!cached.has("snapshot"), "{cached:?}");
    assert!(cached.has("impact"), "{cached:?}");

    // An engine with an empty cache of its own loads each side on its own.
    let full = engine(&dir.path().join("full"));
    let phases = Phases::default();
    let [full_base, full_head] = graphs(&full, &repo, &comparison, &phases);
    assert!(phases.has("load head"), "{phases:?}");

    assert_same_graph(&overlay_base, &full_base, "base");
    assert_same_graph(&overlay_head, &full_head, "head");

    // The comparison above would also pass on two empty graphs, so check that the
    // overlay really saw the changed, added, deleted and whitespace-only text.
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
    let repo = dir.path().join("repo");
    // The same crate with one more line in its manifest: the project model of
    // the base side no longer describes the head side.
    let mut changed = Vec::from(HEAD);
    changed[0] = (
        "Cargo.toml",
        "[package]\nname = \"overlay_app\"\nversion = \"0.2.0\"\nedition = \"2024\"\n",
    );
    repository(&repo, BASE, &changed);

    let phases = Phases::default();
    let engine = engine(&dir.path().join("cache"));
    analyze(&engine, &repo, &AnalysisParams::default(), &phases);
    assert!(phases.has("load base"), "{phases:?}");
    assert!(phases.has("load head"), "{phases:?}");
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

/// A repository whose last two commits hold `base` and then `head`.
fn repository(dir: &Path, base: &[(&str, &str)], head: &[(&str, &str)]) {
    write_files(dir, base);
    fs::write(dir.join(".gitignore"), "target\nCargo.lock\n").expect("the ignore file is written");
    git(dir, &["init", "--quiet", "."]);
    commit(dir, "base");
    fs::remove_dir_all(dir.join("src")).expect("the sources are removed");
    write_files(dir, head);
    commit(dir, "head");
}

/// Writes the files of one revision below `root`.
fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let file = root.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).expect("the directory is created");
        }
        fs::write(&file, text).expect("the file is written");
    }
}

/// Commits everything in `dir`.
fn commit(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    let identity = ["-c", "user.name=t", "-c", "user.email=t@t"];
    git(dir, &[&identity[..], &["commit", "-qm", message]].concat());
}

/// Runs one Git command in `dir`.
fn git(dir: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(arguments)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {arguments:?} failed");
}

/// An engine whose cache and target directory live in `dir`, so that the tests
/// do not share one.
fn engine(dir: &Path) -> Engine {
    Engine::with_paths(blastr_model::Paths {
        cache_dir: dir.join("cache"),
        config_dir: dir.join("config"),
    })
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
