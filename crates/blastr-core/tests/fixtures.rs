//! Runs the engine over the fixture workspaces and checks the result against
//! the expectations in `fixtures/expected/*.toml`.
//!
//! Every file names the crates its case owns; only symbols, edges, changes and
//! impact items inside those crates are checked. A case may also state which
//! items the impact analysis must list (`impact`, `no_impact`), which tests it
//! must select (`selected_test`, `no_selected_test`) and which `cargo test`
//! command lines it must produce (`test_command`).
//!
//! The two workspaces are committed one after the other into a temporary
//! repository, and the engine analyses the last commit.
//!
//! Deliberately one test: loading the two fixture workspaces is slow and the
//! CI runner gives every test its own process, so a static would not share it.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use blastr_core::{AnalysisRequest, Engine};
use blastr_model::{
    Analysis, AnalysisOptions, AnalysisParams, CancelToken, Certainty, Change, ChangeDetail,
    ChangeKind, Confidence, Delta, Edge, EdgeDetail, EdgeKind, GraphIndex, ImpactClass, ImpactItem,
    NoProgress, Reason, RevSpec, RevisionGraph, SelectedTest, SymbolId, SymbolKind, TestReason,
    Visibility,
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
    /// Whether the `impact` entries must be all the items in the case's crates.
    #[serde(default)]
    exact_impact: bool,
    /// Impact items that must be listed.
    #[serde(default)]
    impact: Vec<ExpectedImpact>,
    /// Impact items that must not be listed.
    #[serde(default)]
    no_impact: Vec<AbsentImpact>,
    /// Tests that must be selected.
    #[serde(default)]
    selected_test: Vec<ExpectedSelectedTest>,
    /// Tests that must not be selected.
    #[serde(default)]
    no_selected_test: Vec<AbsentSelectedTest>,
    /// Command lines that must be among the selected tests' commands.
    #[serde(default)]
    test_command: Vec<ExpectedTestCommand>,
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

/// An impact item that must be listed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedImpact {
    /// Canonical path text of the affected symbol.
    symbol: String,
    /// Required class, when given: `adaptation` or `behaviour`.
    class: Option<String>,
    /// Required confidence, when given: `definite`, `likely` or `possible`.
    confidence: Option<String>,
    /// A reason that any kept path of the item must give, in kebab case.
    reason: Option<String>,
    /// Required distance from the change, when given.
    distance: Option<u32>,
    /// Tags that must be set: `test`, `main`, `public-api` or `changed`.
    #[serde(default)]
    tags: Vec<String>,
}

impl ExpectedImpact {
    /// Whether an item satisfies every constraint of this expectation.
    fn matches(&self, item: &ImpactItem) -> bool {
        let class = self.class.as_deref();
        let confidence = self.confidence.as_deref();
        let reason = self.reason.as_deref();
        let class_ok = class.is_none_or(|name| item.class.as_str() == name);
        let confidence_ok = confidence.is_none_or(|name| item.confidence.as_str() == name);
        let distance_ok = self.distance.is_none_or(|value| item.distance == value);
        let reason_ok = match reason {
            Some(name) => item
                .paths
                .iter()
                .any(|path| reason_name(path.reason) == name),
            None => true,
        };
        let tags = item_tags(item);
        let tags_ok = self.tags.iter().all(|tag| tags.contains(&tag.as_str()));
        class_ok && confidence_ok && distance_ok && reason_ok && tags_ok
    }

    /// The constraints of this expectation, for a failure message.
    fn filters(&self) -> String {
        let mut parts = Vec::new();
        if let Some(class) = &self.class {
            parts.push(format!("class {class}"));
        }
        if let Some(confidence) = &self.confidence {
            parts.push(format!("confidence {confidence}"));
        }
        if let Some(reason) = &self.reason {
            parts.push(format!("reason {reason}"));
        }
        if let Some(distance) = self.distance {
            parts.push(format!("distance {distance}"));
        }
        if !self.tags.is_empty() {
            parts.push(format!("tags {}", self.tags.join(",")));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(", "))
        }
    }
}

/// An impact item that must not be listed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbsentImpact {
    /// Canonical path text of the affected symbol.
    symbol: String,
    /// Class the item must not have; any class when omitted.
    class: Option<String>,
}

/// A test that must be selected.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedSelectedTest {
    /// Canonical path text of the test function.
    symbol: String,
    /// Required reason, when given: `reached` or `changed`.
    reason: Option<String>,
}

/// A test that must not be selected.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbsentSelectedTest {
    /// Canonical path text of the test function.
    symbol: String,
}

/// A command line that must be among the selected tests' commands.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedTestCommand {
    /// The line, as `TestCommand::command_line` renders it.
    line: String,
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

/// The two graphs, the changes found between them and their impact.
struct Context<'a> {
    /// The base side.
    before: GraphSide<'a>,
    /// The head side.
    after: GraphSide<'a>,
    /// Reported changes.
    changes: &'a [Change],
    /// What the changes can affect.
    impact: &'a Analysis,
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
        self.check_impacts(case_name, case, &mut failures);
        self.check_absent_impacts(case_name, case, &mut failures);
        self.check_exact_impact(case_name, case, &mut failures);
        self.check_selected_tests(case_name, case, &mut failures);
        self.check_unselected_tests(case_name, case, &mut failures);
        self.check_test_commands(case_name, case, &mut failures);
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

    /// Checks the `impact` entries.
    fn check_impacts(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.impact {
            let known = expect.class.as_deref().is_none_or(is_known_class)
                && expect.confidence.as_deref().is_none_or(is_known_confidence)
                && expect.reason.as_deref().is_none_or(is_known_reason)
                && expect.tags.iter().all(|tag| is_known_tag(tag));
            if !known {
                failures.push(failure(
                    case_name,
                    &format!("known names for `{}`", expect.symbol),
                    "an unknown class, confidence, reason or tag",
                ));
                continue;
            }
            let matched = self
                .impact
                .items
                .iter()
                .filter(|item| item.symbol.path == expect.symbol)
                .any(|item| expect.matches(item));
            if !matched {
                failures.push(failure(
                    case_name,
                    &format!("an impact item `{}`{}", expect.symbol, expect.filters()),
                    &self.items_of(case),
                ));
            }
        }
    }

    /// Checks the `no_impact` entries.
    fn check_absent_impacts(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.no_impact {
            if let Some(name) = expect.class.as_deref()
                && !is_known_class(name)
            {
                failures.push(failure(
                    case_name,
                    &format!("a known impact class for `{}`", expect.symbol),
                    &format!("unknown class `{name}`"),
                ));
                continue;
            }
            let class = expect.class.as_deref();
            let present: Vec<String> = self
                .impact
                .items
                .iter()
                .filter(|item| item.symbol.path == expect.symbol)
                .filter(|item| class.is_none_or(|name| item.class.as_str() == name))
                .map(format_item)
                .collect();
            if !present.is_empty() {
                let expected = match class {
                    Some(class) => format!("no `{class}` impact item `{}`", expect.symbol),
                    None => format!("no impact item `{}`", expect.symbol),
                };
                failures.push(failure(case_name, &expected, &join(&present)));
            }
        }
    }

    /// Checks `exact_impact`.
    fn check_exact_impact(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        if !case.exact_impact {
            return;
        }
        let unexpected: Vec<String> = self
            .impact
            .items
            .iter()
            .filter(|item| belongs(&case.crates, item.symbol.path.as_str()))
            .filter(|item| !is_expected(case, &item.symbol.path))
            .map(format_item)
            .collect();
        if !unexpected.is_empty() {
            failures.push(failure(
                case_name,
                "only the listed impact items in the case's crates",
                &join(&unexpected),
            ));
        }
    }

    /// Checks the `selected_test` entries.
    fn check_selected_tests(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.selected_test {
            if let Some(name) = expect.reason.as_deref()
                && !is_known_test_reason(name)
            {
                failures.push(failure(
                    case_name,
                    &format!("a known test reason for `{}`", expect.symbol),
                    &format!("unknown reason `{name}`"),
                ));
                continue;
            }
            let wanted = expect.reason.as_deref();
            let matched = self
                .impact
                .tests
                .selected
                .iter()
                .filter(|test| test.symbol.path == expect.symbol)
                .any(|test| wanted.is_none_or(|name| test_reason_name(test.reason) == name));
            if !matched {
                let expected = match wanted {
                    Some(reason) => {
                        format!("a selected test `{}` with reason {reason}", expect.symbol)
                    }
                    None => format!("a selected test `{}`", expect.symbol),
                };
                failures.push(failure(case_name, &expected, &self.selected_tests_of(case)));
            }
        }
    }

    /// Checks the `no_selected_test` entries.
    fn check_unselected_tests(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.no_selected_test {
            let present: Vec<String> = self
                .impact
                .tests
                .selected
                .iter()
                .filter(|test| test.symbol.path == expect.symbol)
                .map(format_selected_test)
                .collect();
            if !present.is_empty() {
                failures.push(failure(
                    case_name,
                    &format!("no selected test `{}`", expect.symbol),
                    &join(&present),
                ));
            }
        }
    }

    /// Checks the `test_command` entries.
    fn check_test_commands(&self, case_name: &str, case: &Case, failures: &mut Vec<String>) {
        for expect in &case.test_command {
            let found = self
                .impact
                .tests
                .commands
                .iter()
                .any(|command| command.command_line() == expect.line);
            if !found {
                failures.push(failure(
                    case_name,
                    &format!("the test command `{}`", expect.line),
                    &self.commands_of(case),
                ));
            }
        }
    }

    /// The impact items in the case's crates, formatted for a failure message.
    fn items_of(&self, case: &Case) -> String {
        let listed: Vec<String> = self
            .impact
            .items
            .iter()
            .filter(|item| belongs(&case.crates, item.symbol.path.as_str()))
            .map(format_item)
            .collect();
        if listed.is_empty() {
            "none".to_string()
        } else {
            join(&listed)
        }
    }

    /// The selected tests in the case's crates, formatted for a failure message.
    fn selected_tests_of(&self, case: &Case) -> String {
        let listed: Vec<String> = self
            .impact
            .tests
            .selected
            .iter()
            .filter(|test| belongs(&case.crates, test.symbol.path.as_str()))
            .map(format_selected_test)
            .collect();
        if listed.is_empty() {
            "none".to_string()
        } else {
            join(&listed)
        }
    }

    /// The commands of the case's packages, formatted for a failure message.
    fn commands_of(&self, case: &Case) -> String {
        let listed: Vec<String> = self
            .impact
            .tests
            .commands
            .iter()
            .filter(|command| belongs_package(&case.crates, &command.package))
            .map(|command| command.command_line())
            .collect();
        if listed.is_empty() {
            "none".to_string()
        } else {
            join(&listed)
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
        "with-rest" => Some(EdgeDetail::WithRest),
        _ => None,
    }
}

/// The kebab-case name of an `EdgeDetail`.
fn edge_detail_name(detail: EdgeDetail) -> &'static str {
    match detail {
        EdgeDetail::None => "none",
        EdgeDetail::MatchExhaustive => "match-exhaustive",
        EdgeDetail::MatchWildcard => "match-wildcard",
        EdgeDetail::WithRest => "with-rest",
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
        ChangeDetail::Retyped => " retyped".to_string(),
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

/// Formats an impact item as `symbol (class, confidence, distance N, reasons: ...)`.
fn format_item(item: &ImpactItem) -> String {
    let mut reasons: Vec<String> = item
        .paths
        .iter()
        .map(|path| reason_name(path.reason))
        .collect();
    reasons.sort();
    reasons.dedup();
    let mut text = format!(
        "{} ({}, {}, distance {}",
        item.symbol.path,
        item.class.as_str(),
        item.confidence.as_str(),
        item.distance
    );
    if !reasons.is_empty() {
        text.push_str(&format!(", reasons: {}", reasons.join(", ")));
    }
    let tags = item_tags(item);
    if !tags.is_empty() {
        text.push_str(&format!(", tags: {}", tags.join(",")));
    }
    text.push(')');
    text
}

/// The tag names set on an impact item.
fn item_tags(item: &ImpactItem) -> Vec<&'static str> {
    let mut tags = Vec::new();
    if item.tags.is_test {
        tags.push("test");
    }
    if item.tags.is_main {
        tags.push("main");
    }
    if item.tags.is_pub_api {
        tags.push("public-api");
    }
    if item.tags.changed {
        tags.push("changed");
    }
    tags
}

/// Whether a tag name in an expectation file is known.
fn is_known_tag(tag: &str) -> bool {
    matches!(tag, "test" | "main" | "public-api" | "changed")
}

/// Whether a name in an expectation file is a known impact class.
fn is_known_class(name: &str) -> bool {
    parse_enum(name, ImpactClass::ALL, |value| value.as_str()).is_some()
}

/// Whether a name in an expectation file is a known confidence.
fn is_known_confidence(name: &str) -> bool {
    parse_enum(name, Confidence::ALL, |value| value.as_str()).is_some()
}

/// Whether a name in an expectation file is a known reason.
fn is_known_reason(name: &str) -> bool {
    parse_kebab::<Reason>(name).is_some()
}

/// Whether a name in an expectation file is a known test reason.
fn is_known_test_reason(name: &str) -> bool {
    parse_kebab::<TestReason>(name).is_some()
}

/// Whether a case lists an impact item with this path text.
fn is_expected(case: &Case, symbol: &str) -> bool {
    case.impact.iter().any(|expect| expect.symbol == symbol)
}

/// Formats a selected test as `symbol (reason, filter `name`)`.
fn format_selected_test(test: &SelectedTest) -> String {
    format!(
        "{} ({}, filter `{}`)",
        test.symbol.path,
        test_reason_name(test.reason),
        test.filter
    )
}

/// The kebab-case name serde gives a value.
fn kebab_name<T: serde::Serialize>(value: T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

/// Parses a value from its kebab-case name, reporting `None` for unknown names.
fn parse_kebab<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

/// The kebab-case name of a `Reason`.
fn reason_name(reason: Reason) -> String {
    kebab_name(reason)
}

/// The kebab-case name of a `TestReason`.
fn test_reason_name(reason: TestReason) -> String {
    kebab_name(reason)
}

/// Whether a Cargo package belongs to the case's crates.
fn belongs_package(crates: &[String], package: &str) -> bool {
    let underscored = package.replace('-', "_");
    crates
        .iter()
        .any(|krate| krate == package || *krate == underscored)
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
            | "retyped"
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
        "retyped" => matches!(actual, ChangeDetail::Retyped),
        _ => false,
    }
}

/// Copies the fixture workspace in `from` into `to`, leaving out what a load
/// writes into it.
fn copy_workspace(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap_or_else(|error| panic!("create {}: {error}", to.display()));
    for entry in
        fs::read_dir(from).unwrap_or_else(|error| panic!("read {}: {error}", from.display()))
    {
        let entry = entry.expect("read a directory entry");
        let name = entry.file_name();
        if name == "target" || name == "Cargo.lock" {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().expect("read a file type").is_dir() {
            copy_workspace(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target)
                .unwrap_or_else(|error| panic!("copy {}: {error}", entry.path().display()));
        }
    }
}

/// Runs one Git command in `dir`, committing files byte for byte.
fn git(dir: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
        ])
        .args(arguments)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {arguments:?} failed");
}

/// A repository whose last two commits hold the two fixture workspaces.
fn fixture_repository(fixtures: &Path, dir: &Path) {
    copy_workspace(&fixtures.join("before"), dir);
    fs::write(dir.join(".gitignore"), "target\nCargo.lock\n").expect("write the ignore file");
    git(dir, &["init", "--quiet", "."]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "before"]);
    for entry in fs::read_dir(dir).expect("read the repository") {
        let entry = entry.expect("read a directory entry");
        if entry.file_name() == ".git" || entry.file_name() == ".gitignore" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).expect("remove a directory");
        } else {
            fs::remove_file(&path).expect("remove a file");
        }
    }
    copy_workspace(&fixtures.join("after"), dir);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "after"]);
}

/// Loads the two fixture workspaces, compares them and checks every expectation file.
#[test]
fn fixtures() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let expected_dir = fixtures.join("expected");

    let temp = tempfile::tempdir().expect("create a temporary directory");
    let repo = temp.path().join("repo");
    fixture_repository(&fixtures, &repo);
    let paths = blastr_model::Paths {
        cache_dir: temp.path().join("cache"),
        config_dir: temp.path().join("config"),
    };
    let engine = Engine::with_paths(paths);

    let spec = RevSpec {
        rev: Some("HEAD~1..HEAD".to_owned()),
        ..RevSpec::default()
    };
    let options = AnalysisOptions::default();
    let request = AnalysisRequest {
        repo_root: &repo,
        spec: &spec,
        options: &options,
        params: &AnalysisParams::default(),
    };
    let fail = |error: blastr_core::Error| -> ! {
        let hint = error
            .hint()
            .map(|hint| format!("\nhint: {hint}"))
            .unwrap_or_default();
        panic!("analysing the fixture workspaces failed: {error}{hint}")
    };
    let analysis = engine
        .analyze(&request, &NoProgress, &CancelToken::new(), &mut |_| {})
        .unwrap_or_else(|error| fail(error));
    // Both graphs come from the cache the analysis filled.
    let [base, head] = [
        &analysis.diff.comparison.base,
        &analysis.diff.comparison.head,
    ]
    .map(|source| {
        engine
            .graph(&repo, source, &options, &NoProgress, &CancelToken::new())
            .unwrap_or_else(|error| fail(error))
    });

    let before_index = GraphIndex::new(&base);
    let after_index = GraphIndex::new(&head);
    let context = Context {
        before: GraphSide {
            label: "before",
            graph: base.as_ref(),
            index: &before_index,
        },
        after: GraphSide {
            label: "after",
            graph: head.as_ref(),
            index: &after_index,
        },
        changes: &analysis.diff.changes,
        impact: &analysis,
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
        "fixtures: {} cases, {} symbols before / {} after, {} edges before / {} after, {} changes, {} impact items, {} selected tests",
        files.len(),
        base.symbols.len(),
        head.symbols.len(),
        base.edges.len(),
        head.edges.len(),
        analysis.diff.changes.len(),
        analysis.items.len(),
        analysis.tests.selected.len(),
    );
}
