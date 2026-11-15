//! Runs the engine over the fixture workspaces and checks the result against
//! the expectations in `fixtures/expected/*.toml`.
//!
//! Deliberately one test: loading the two fixture workspaces is slow and the
//! CI runner gives every test its own process, so a static would not share it.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use blastr_core::Engine;
use blastr_model::{
    AnalysisOptions, CancelToken, Certainty, Change, ChangeDetail, ChangeKind, Delta, Edge,
    EdgeDetail, EdgeKind, GraphIndex, NoProgress, RevisionGraph, SymbolId, SymbolKind, Visibility,
};
use serde::Deserialize;

/// Which graph an expectation applies to.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ExpectedSide {
    /// The base graph.
    Before,
    /// The head graph.
    #[default]
    After,
    /// Both graphs.
    Both,
}

/// One `expected/<case>.toml` file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    /// Crate names (with underscores) whose symbols, edges and changes belong to this case.
    crates: Vec<String>,
    /// Whether the `change` entries must be all changes in the case's crates.
    #[serde(default)]
    exact_changes: bool,
    /// Symbols that must exist.
    #[serde(default)]
    symbol: Vec<ExpectedSymbol>,
    /// Symbols that must not exist.
    #[serde(default)]
    no_symbol: Vec<AbsentSymbol>,
    /// Edges that must exist.
    #[serde(default)]
    edge: Vec<ExpectedEdge>,
    /// Edges that must not exist.
    #[serde(default)]
    no_edge: Vec<AbsentEdge>,
    /// Changes that must be reported.
    #[serde(default)]
    change: Vec<ExpectedChange>,
    /// Changes that must not be reported.
    #[serde(default)]
    no_change: Vec<AbsentChange>,
}

/// A symbol that must exist.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedSymbol {
    /// Canonical path text.
    path: String,
    /// Required kind, when given.
    kind: Option<String>,
    /// Side to look on.
    #[serde(default)]
    side: ExpectedSide,
}

/// A symbol that must not exist.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbsentSymbol {
    /// Canonical path text.
    path: String,
    /// Side to look on.
    #[serde(default)]
    side: ExpectedSide,
}

/// An edge that must exist.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedEdge {
    /// Canonical path text of the source symbol.
    from: String,
    /// Canonical path text of the target symbol.
    to: String,
    /// Required kind.
    kind: String,
    /// Required certainty, when given.
    certainty: Option<String>,
    /// Required detail, when given.
    detail: Option<String>,
    /// Side to look on.
    #[serde(default)]
    side: ExpectedSide,
}

/// An edge that must not exist.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbsentEdge {
    /// Canonical path text of the source symbol.
    from: String,
    /// Canonical path text of the target symbol.
    to: String,
    /// Required kind; any kind when omitted.
    kind: Option<String>,
    /// Side to look on.
    #[serde(default)]
    side: ExpectedSide,
}

/// A change that must be reported.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedChange {
    /// Kebab-case change kind.
    kind: String,
    /// Canonical path text of the changed symbol.
    symbol: String,
    /// Required detail, when given.
    detail: Option<String>,
    /// Previous path text, used with `detail = "renamed"`.
    from: Option<String>,
}

/// A change that must not be reported.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbsentChange {
    /// Canonical path text of the symbol.
    symbol: String,
    /// Required kind; any kind when omitted.
    kind: Option<String>,
}

/// One graph and the index built over it.
#[derive(Clone, Copy)]
struct GraphSide<'a> {
    /// Name used in failure messages.
    label: &'static str,
    /// The graph.
    graph: &'a RevisionGraph,
    /// The index over `graph`.
    index: &'a GraphIndex,
}

/// The two graphs and the changes found between them.
struct Context<'a> {
    /// The base side.
    before: GraphSide<'a>,
    /// The head side.
    after: GraphSide<'a>,
    /// Reported changes.
    changes: &'a [Change],
}

impl<'a> Context<'a> {
    /// The sides selected by an expectation.
    fn selected(&self, side: ExpectedSide) -> Vec<GraphSide<'a>> {
        match side {
            ExpectedSide::Before => vec![self.before],
            ExpectedSide::After => vec![self.after],
            ExpectedSide::Both => vec![self.before, self.after],
        }
    }

    /// Checks one case and returns every failure it produced.
    fn check(&self, case_name: &str, case: &Case) -> Vec<String> {
        let mut failures = Vec::new();
        self.check_symbols(case_name, case, &mut failures);
        self.check_absent_symbols(case_name, case, &mut failures);
        self.check_edges(case_name, case, &mut failures);
        self.check_absent_edges(case_name, case, &mut failures);
        self.check_changes(case_name, case, &mut failures);
        self.check_absent_changes(case_name, case, &mut failures);
        self.check_exact_changes(case_name, case, &mut failures);
        failures
    }

    /// Checks the `symbol` entries.
    fn check_symbols(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.symbol {
            let mut kind = None;
            if let Some(name) = expect.kind.as_deref() {
                match parse_enum(name, SymbolKind::ALL, |value| value.as_str()) {
                    Some(parsed) => kind = Some(parsed),
                    None => {
                        failures.push(failure(
                            case_name,
                            &format!("a known symbol kind for `{}`", expect.path),
                            &format!("unknown kind `{name}`"),
                        ));
                        continue;
                    }
                }
            }
            for side in self.selected(expect.side) {
                let ids = side.index.find(&expect.path);
                if ids.is_empty() {
                    failures.push(failure(
                        case_name,
                        &format!("symbol `{}` on the {} side", expect.path, side.label),
                        "no symbol with that path",
                    ));
                    continue;
                }
                if let Some(kind) = kind {
                    let found: Vec<&str> =
                        ids.iter().map(|id| node_kind(side.graph, *id)).collect();
                    if !found.contains(&kind.as_str()) {
                        failures.push(failure(
                            case_name,
                            &format!(
                                "symbol `{}` of kind `{}` on the {} side",
                                expect.path,
                                kind.as_str(),
                                side.label
                            ),
                            &format!("found kinds: {}", join(&found)),
                        ));
                    }
                }
            }
        }
    }

    /// Checks the `no_symbol` entries.
    fn check_absent_symbols(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.no_symbol {
            for side in self.selected(expect.side) {
                if !side.index.find(&expect.path).is_empty() {
                    failures.push(failure(
                        case_name,
                        &format!("no symbol `{}` on the {} side", expect.path, side.label),
                        "the symbol exists",
                    ));
                }
            }
        }
    }

    /// Checks the `edge` entries.
    fn check_edges(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.edge {
            let Some(kind) = parse_enum(&expect.kind, EdgeKind::ALL, |value| value.as_str()) else {
                failures.push(failure(
                    case_name,
                    &format!(
                        "a known edge kind from `{}` to `{}`",
                        expect.from, expect.to
                    ),
                    &format!("unknown kind `{}`", expect.kind),
                ));
                continue;
            };
            let mut certainty = None;
            if let Some(name) = expect.certainty.as_deref() {
                match parse_enum(name, Certainty::ALL, |value| value.as_str()) {
                    Some(parsed) => certainty = Some(parsed),
                    None => {
                        failures.push(failure(
                            case_name,
                            &format!(
                                "a known certainty for the `{}` edge from `{}` to `{}`",
                                kind.as_str(),
                                expect.from,
                                expect.to
                            ),
                            &format!("unknown certainty `{name}`"),
                        ));
                        continue;
                    }
                }
            }
            let mut detail = None;
            if let Some(name) = expect.detail.as_deref() {
                match parse_edge_detail(name) {
                    Some(parsed) => detail = Some(parsed),
                    None => {
                        failures.push(failure(
                            case_name,
                            &format!(
                                "a known detail for the `{}` edge from `{}` to `{}`",
                                kind.as_str(),
                                expect.from,
                                expect.to
                            ),
                            &format!("unknown detail `{name}`"),
                        ));
                        continue;
                    }
                }
            }
            for side in self.selected(expect.side) {
                let candidates = edges_between(&side, &expect.from, &expect.to);
                let matched = candidates.iter().any(|edge| {
                    edge.kind == kind
                        && certainty.is_none_or(|value| edge.certainty == value)
                        && detail.is_none_or(|value| edge.detail == value)
                });
                if !matched {
                    failures.push(failure(
                        case_name,
                        &format!(
                            "a `{}` edge from `{}` to `{}` on the {} side",
                            kind.as_str(),
                            expect.from,
                            expect.to,
                            side.label
                        ),
                        &edge_diagnostic(&side, &expect.from, &candidates),
                    ));
                }
            }
        }
    }

    /// Checks the `no_edge` entries.
    fn check_absent_edges(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.no_edge {
            let mut kind = None;
            if let Some(name) = expect.kind.as_deref() {
                match parse_enum(name, EdgeKind::ALL, |value| value.as_str()) {
                    Some(parsed) => kind = Some(parsed),
                    None => {
                        failures.push(failure(
                            case_name,
                            &format!(
                                "a known edge kind from `{}` to `{}`",
                                expect.from, expect.to
                            ),
                            &format!("unknown kind `{name}`"),
                        ));
                        continue;
                    }
                }
            }
            for side in self.selected(expect.side) {
                let present: Vec<&Edge> = edges_between(&side, &expect.from, &expect.to)
                    .into_iter()
                    .filter(|edge| kind.is_none_or(|value| edge.kind == value))
                    .collect();
                if !present.is_empty() {
                    let expected = format!(
                        "no `{}` edge from `{}` to `{}` on the {} side",
                        kind.map_or("any", |value| value.as_str()),
                        expect.from,
                        expect.to,
                        side.label
                    );
                    let found = present
                        .iter()
                        .map(|edge| format_edge(&side, edge))
                        .collect::<Vec<_>>();
                    failures.push(failure(case_name, &expected, &join(&found)));
                }
            }
        }
    }

    /// Checks the `change` entries.
    fn check_changes(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.change {
            let Some(kind) = parse_enum(&expect.kind, ChangeKind::ALL, |value| value.as_str())
            else {
                failures.push(failure(
                    case_name,
                    &format!("a known change kind for `{}`", expect.symbol),
                    &format!("unknown kind `{}`", expect.kind),
                ));
                continue;
            };
            let candidates: Vec<&Change> = self
                .changes
                .iter()
                .filter(|change| change.kind == kind && change.symbol.path == expect.symbol)
                .collect();
            let expected = format!("a `{}` change for `{}`", kind.as_str(), expect.symbol);
            if candidates.is_empty() {
                failures.push(failure(
                    case_name,
                    &expected,
                    &format!("not reported; reported changes: {}", self.reported(case)),
                ));
                continue;
            }
            let Some(detail) = expect.detail.as_deref() else {
                continue;
            };
            if !is_known_detail(detail) {
                failures.push(failure(
                    case_name,
                    &expected,
                    &format!("unknown detail `{detail}`"),
                ));
                continue;
            }
            if !candidates
                .iter()
                .any(|change| detail_matches(detail, expect.from.as_deref(), &change.detail))
            {
                let found = candidates
                    .iter()
                    .map(|change| format_change(change))
                    .collect::<Vec<_>>();
                failures.push(failure(
                    case_name,
                    &format!("{expected} with detail `{detail}`"),
                    &join(&found),
                ));
            }
        }
    }

    /// Checks the `no_change` entries.
    fn check_absent_changes(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.no_change {
            let mut kind = None;
            if let Some(name) = expect.kind.as_deref() {
                match parse_enum(name, ChangeKind::ALL, |value| value.as_str()) {
                    Some(parsed) => kind = Some(parsed),
                    None => {
                        failures.push(failure(
                            case_name,
                            &format!("a known change kind for `{}`", expect.symbol),
                            &format!("unknown kind `{name}`"),
                        ));
                        continue;
                    }
                }
            }
            let present: Vec<&Change> = self
                .changes
                .iter()
                .filter(|change| {
                    change.symbol.path == expect.symbol
                        && kind.is_none_or(|value| change.kind == value)
                })
                .collect();
            if !present.is_empty() {
                let expected = format!(
                    "no `{}` change for `{}`",
                    kind.map_or("any", |value| value.as_str()),
                    expect.symbol
                );
                let found = present
                    .iter()
                    .map(|change| format_change(change))
                    .collect::<Vec<_>>();
                failures.push(failure(case_name, &expected, &join(&found)));
            }
        }
    }

    /// Checks `exact_changes`.
    fn check_exact_changes(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        if !case.exact_changes {
            return;
        }
        let mut expected: Vec<(ChangeKind, &str)> = Vec::new();
        for entry in &case.change {
            if let Some(kind) = parse_enum(&entry.kind, ChangeKind::ALL, |value| value.as_str()) {
                expected.push((kind, entry.symbol.as_str()));
            }
        }
        let unexpected = self
            .changes
            .iter()
            .filter(|change| belongs(&case.crates, &change.symbol.path))
            .filter(|change| {
                !expected.iter().any(|(kind, symbol)| {
                    *kind == change.kind && change.symbol.path.as_str() == *symbol
                })
            })
            .map(format_change)
            .collect::<Vec<_>>();
        if !unexpected.is_empty() {
            failures.push(failure(
                case_name,
                "only the listed changes in the case's crates",
                &join(&unexpected),
            ));
        }
    }

    /// The changes reported in the case's crates, formatted for a failure message.
    fn reported(&self, case: &Case) -> String {
        let listed = self
            .changes
            .iter()
            .filter(|change| belongs(&case.crates, &change.symbol.path))
            .map(format_change)
            .collect::<Vec<_>>();
        if listed.is_empty() {
            "none".to_string()
        } else {
            join(&listed)
        }
    }
}

/// Builds one failure line: `<case>: <expected> — <found>`.
fn failure(case_name: &str, expected: &str, found: &str) -> String {
    format!("{case_name}: {expected} — {found}")
}

/// Joins formatted items for a failure message.
fn join<T: AsRef<str>>(items: &[T]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items
            .iter()
            .map(|item| item.as_ref())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Finds a variant by its kebab-case name, reporting `None` for unknown names.
fn parse_enum<T: Copy, F: Fn(T) -> &'static str>(name: &str, all: &[T], as_str: F) -> Option<T> {
    all.iter().copied().find(|value| as_str(*value) == name)
}

/// Parses an `EdgeDetail` by its kebab-case name.
fn parse_edge_detail(name: &str) -> Option<EdgeDetail> {
    match name {
        "none" => Some(EdgeDetail::None),
        "match-exhaustive" => Some(EdgeDetail::MatchExhaustive),
        "match-wildcard" => Some(EdgeDetail::MatchWildcard),
        _ => None,
    }
}

/// The kebab-case name of an `EdgeDetail`.
fn edge_detail_name(detail: EdgeDetail) -> &'static str {
    match detail {
        EdgeDetail::None => "none",
        EdgeDetail::MatchExhaustive => "match-exhaustive",
        EdgeDetail::MatchWildcard => "match-wildcard",
    }
}

/// The kebab-case name of a `Delta`.
fn delta_name(delta: Delta) -> &'static str {
    match delta {
        Delta::Added => "added",
        Delta::Removed => "removed",
        Delta::Modified => "modified",
    }
}

/// The kebab-case name of a `Visibility`.
fn visibility_name(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Private => "private",
        Visibility::Restricted => "restricted",
        Visibility::Crate => "crate",
        Visibility::Public => "public",
    }
}

/// The canonical path text of a symbol.
fn node_path(graph: &RevisionGraph, id: SymbolId) -> &str {
    &graph.symbols[id.0 as usize].path.path
}

/// The kind name of a symbol.
fn node_kind(graph: &RevisionGraph, id: SymbolId) -> &'static str {
    graph.symbols[id.0 as usize].path.kind.as_str()
}

/// Every edge from one path text to another on one side.
fn edges_between<'g>(side: &GraphSide<'g>, from: &str, to: &str) -> Vec<&'g Edge> {
    side.graph
        .edges
        .iter()
        .filter(|edge| {
            node_path(side.graph, edge.from) == from && node_path(side.graph, edge.to) == to
        })
        .collect()
}

/// Describes why an expected edge was not found.
fn edge_diagnostic(side: &GraphSide<'_>, from: &str, between: &[&Edge]) -> String {
    if !between.is_empty() {
        let found = between
            .iter()
            .map(|edge| format_edge(side, edge))
            .collect::<Vec<_>>();
        return format!("edges between them: {}", join(&found));
    }
    let leaving = side
        .graph
        .edges
        .iter()
        .filter(|edge| node_path(side.graph, edge.from) == from)
        .map(|edge| format_edge(side, edge))
        .collect::<Vec<_>>();
    if leaving.is_empty() {
        format!("no edge leaves `{from}`")
    } else {
        format!(
            "no edge between them; edges leaving `{from}`: {}",
            join(&leaving)
        )
    }
}

/// Formats a change as `kind symbol [detail]`.
fn format_change(change: &Change) -> String {
    let detail = match &change.detail {
        ChangeDetail::None => String::new(),
        ChangeDetail::Member(delta) => format!(" member-{}", delta_name(*delta)),
        ChangeDetail::DefaultBody => " default-body".to_string(),
        ChangeDetail::Renamed { from } => format!(" renamed from {}", from.path),
        ChangeDetail::Visibility { from, to } => {
            format!(
                " visibility {} -> {}",
                visibility_name(*from),
                visibility_name(*to)
            )
        }
        ChangeDetail::Manifest(text) => format!(" manifest {text}"),
    };
    format!("{} {}{}", change.kind.as_str(), change.symbol.path, detail)
}

/// Formats an edge as `kind from -> to`, with certainty and detail when they are not default.
fn format_edge(side: &GraphSide<'_>, edge: &Edge) -> String {
    let from = node_path(side.graph, edge.from);
    let to = node_path(side.graph, edge.to);
    let mut text = format!("{} {from} -> {to}", edge.kind.as_str());
    if edge.certainty == Certainty::Candidate {
        text.push_str(" (candidate)");
    }
    if edge.detail != EdgeDetail::None {
        text.push_str(&format!(" ({})", edge_detail_name(edge.detail)));
    }
    text
}

/// Whether a path text's first segment names one of the case's crates.
fn belongs(crates: &[String], path: &str) -> bool {
    let first = path.split("::").next().unwrap_or(path);
    let underscored = first.replace('-', "_");
    crates
        .iter()
        .any(|krate| krate == first || krate == &underscored)
}

/// Whether a `detail` name in an expectation file is known.
fn is_known_detail(detail: &str) -> bool {
    matches!(
        detail,
        "member-added"
            | "member-removed"
            | "member-modified"
            | "default-body"
            | "renamed"
            | "visibility"
            | "manifest"
    )
}

/// Whether a change detail matches a `detail` name, and `from` when it was given.
fn detail_matches(expected: &str, from: Option<&str>, actual: &ChangeDetail) -> bool {
    match expected {
        "member-added" => matches!(actual, ChangeDetail::Member(Delta::Added)),
        "member-removed" => matches!(actual, ChangeDetail::Member(Delta::Removed)),
        "member-modified" => matches!(actual, ChangeDetail::Member(Delta::Modified)),
        "default-body" => matches!(actual, ChangeDetail::DefaultBody),
        "renamed" => matches!(actual, ChangeDetail::Renamed { from: previous }
            if from.is_none_or(|wanted| previous.path.as_str() == wanted)),
        "visibility" => matches!(actual, ChangeDetail::Visibility { .. }),
        "manifest" => matches!(actual, ChangeDetail::Manifest(_)),
        _ => false,
    }
}

/// Loads the two fixture workspaces, compares them and checks every expectation file.
#[test]
fn fixtures() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let before_dir = fixtures.join("before");
    let after_dir = fixtures.join("after");
    let expected_dir = fixtures.join("expected");

    let temp = tempfile::tempdir().expect("create a temporary directory");
    let paths = blastr_store::Paths {
        cache_dir: temp.path().to_path_buf(),
        config_dir: temp.path().join("config"),
    };
    let store = blastr_store::Store::open_in_memory().expect("open an in-memory store");
    let engine = Engine::with_store_and_paths(store, paths);

    let output = engine
        .diff_dirs(
            &before_dir,
            &after_dir,
            &AnalysisOptions::default(),
            &NoProgress,
            &CancelToken::new(),
        )
        .unwrap_or_else(|error| {
            let hint = error
                .hint()
                .map(|hint| format!("\nhint: {hint}"))
                .unwrap_or_default();
            panic!("diffing the fixture workspaces failed: {error}{hint}")
        });

    let before_index = GraphIndex::new(&output.base);
    let after_index = GraphIndex::new(&output.head);
    let context = Context {
        before: GraphSide {
            label: "before",
            graph: output.base.as_ref(),
            index: &before_index,
        },
        after: GraphSide {
            label: "after",
            graph: output.head.as_ref(),
            index: &after_index,
        },
        changes: &output.result.changes,
    };

    let mut files: Vec<PathBuf> = fs::read_dir(&expected_dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", expected_dir.display()))
        .map(|entry| entry.expect("read a directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == OsStr::new("toml"))
        })
        .collect();
    files.sort();
    if files.is_empty() {
        panic!("no expectation files in {}", expected_dir.display());
    }

    let mut failures = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        let case: Case = toml::from_str(&text)
            .unwrap_or_else(|error| panic!("parse {}: {error}", file.display()));
        let name = file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.display().to_string());
        failures.extend(context.check(&name, &case));
    }

    if !failures.is_empty() {
        panic!(
            "{} fixture expectation(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    println!(
        "fixtures: {} cases, {} symbols before / {} after, {} edges before / {} after, {} changes",
        files.len(),
        output.base.symbols.len(),
        output.head.symbols.len(),
        output.base.edges.len(),
        output.head.edges.len(),
        output.result.changes.len(),
    );
}
