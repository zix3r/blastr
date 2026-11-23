//! Test selection: which tests a set of changes touches or reaches, and the
//! `cargo test` commands that run them.
//!
//! A test is a head-side symbol flagged as a test. It is selected when a
//! change touches it, when an added or renamed item contains it, or when
//! impact analysis reaches it at any depth. Selected tests are grouped by their Cargo
//! target and narrowed to module filters, because running a whole target is
//! wasteful once only a few of its tests are affected. A selection that would
//! need many filters, or that covers most of a target, runs the whole target
//! instead: it may run tests that are not selected, but it never skips one.

use std::collections::{BTreeMap, HashSet};

use blastr_model::{
    Change, ChangeKind, RevisionGraph, SelectedTest, Symbol, SymbolId, SymbolPath, TargetKind,
    TestCommand, TestReason, TestSelection,
};

/// The most filters a command carries before it runs its whole target instead.
const MAX_FILTERS: usize = 8;

/// The share of a target's tests, in per cent, from which a command runs the
/// whole target rather than naming the tests it selected.
const WHOLE_TARGET_SHARE: u64 = 60;

/// Selects the tests the changes touch or reach, grouped into commands.
pub(crate) fn select_tests(
    head: &RevisionGraph,
    changes: &[Change],
    reached: &HashSet<SymbolId>,
) -> TestSelection {
    // Changes that touch a test directly. A removed item is gone from the head
    // side, so it cannot name a head test; formatting and manifest changes say
    // nothing about behaviour.
    let touched: HashSet<&SymbolPath> = changes
        .iter()
        .filter(|change| {
            !matches!(
                change.kind,
                ChangeKind::NonSemantic | ChangeKind::Manifest | ChangeKind::Removed
            )
        })
        .map(|change| &change.symbol)
        .collect();
    // Everything inside an added or renamed item is new code as well.
    let introduced: HashSet<&SymbolPath> = changes
        .iter()
        .filter(|change| matches!(change.kind, ChangeKind::Added | ChangeKind::Renamed))
        .map(|change| &change.symbol)
        .collect();

    let mut total = 0_u32;
    let mut selected: Vec<SelectedTest> = Vec::new();
    let mut targets: BTreeMap<(String, u8, String), (TargetKind, Vec<TargetTest>)> =
        BTreeMap::new();

    for (index, symbol) in head.symbols.iter().enumerate() {
        if !symbol.flags.is_test {
            continue;
        }
        total = total.saturating_add(1);

        let reason = if touched.contains(&symbol.path)
            || has_introduced_ancestor(&head.symbols, symbol, &introduced)
        {
            Some(TestReason::Changed)
        } else if reached.contains(&SymbolId(index as u32)) {
            Some(TestReason::Reached)
        } else {
            None
        };

        let Some(info) = head.crates.get(symbol.krate.0 as usize) else {
            continue;
        };
        let entry = targets
            .entry((
                info.package.clone(),
                target_rank(info.target_kind),
                info.target_name.clone(),
            ))
            .or_insert_with(|| (info.target_kind, Vec::new()));
        let filter = filter_text(&symbol.path.path);
        entry.1.push(TargetTest {
            filter: filter.clone(),
            selected: reason.is_some(),
        });
        if let Some(reason) = reason {
            selected.push(SelectedTest {
                symbol: symbol.path.clone(),
                location: symbol.location.clone(),
                reason,
                filter,
            });
        }
    }

    let mut commands: Vec<TestCommand> = Vec::new();
    for ((package, _, target_name), (target_kind, tests)) in targets {
        let count = tests.iter().filter(|test| test.selected).count() as u32;
        if count == 0 {
            continue;
        }
        let filters = command_filters(&tests, count);
        commands.push(TestCommand {
            package,
            target_kind,
            target_name,
            filters,
            tests: count,
            total: tests.len() as u32,
        });
    }

    selected.sort_by(|left, right| left.symbol.cmp(&right.symbol));
    TestSelection {
        selected,
        total,
        commands,
    }
}

/// Whether one of the symbol's ancestors was added or renamed.
fn has_introduced_ancestor(
    symbols: &[Symbol],
    symbol: &Symbol,
    introduced: &HashSet<&SymbolPath>,
) -> bool {
    let mut current: Option<SymbolId> = symbol.parent;
    for _ in 0..=symbols.len() {
        let Some(ancestor) = current.and_then(|id| symbols.get(id.0 as usize)) else {
            return false;
        };
        if introduced.contains(&ancestor.path) {
            return true;
        }
        current = ancestor.parent;
    }
    false
}

/// The name `cargo test` matches a test by: its path without the crate segment.
fn filter_text(path: &str) -> String {
    match path.split_once("::") {
        Some((_, rest)) if !rest.is_empty() => rest.to_owned(),
        _ => path.to_owned(),
    }
}

/// One test of a target, by the name it is matched by and whether it is selected.
struct TargetTest {
    /// The test name without the crate segment.
    filter: String,
    /// Whether the test belongs to the selection.
    selected: bool,
}

/// The filters of a command for a target holding `selected` of its tests.
///
/// The list is empty when the command runs the whole target: because every test
/// of it is selected, because the selected tests are a large enough share of it
/// that naming them saves little, or because naming them needs so many filters
/// that the command line stops being readable and pasteable.
fn command_filters(tests: &[TargetTest], selected: u32) -> Vec<String> {
    let total = tests.len() as u32;
    if selected >= total {
        return Vec::new();
    }
    if covers_most_of_target(selected, total) {
        return Vec::new();
    }
    let filters = shortened_filters(tests);
    if filters.len() > MAX_FILTERS {
        return Vec::new();
    }
    filters
}

/// Whether the selected tests are a large enough share of the target that
/// naming them is not worth the length of the command line.
fn covers_most_of_target(selected: u32, total: u32) -> bool {
    u64::from(selected) * 100 >= u64::from(total) * WHOLE_TARGET_SHARE
}

/// The filters of a command whose target holds both selected and unselected
/// tests, each module prefix shortened as far as it stays accurate.
fn shortened_filters(tests: &[TargetTest]) -> Vec<String> {
    let unselected: Vec<&str> = tests
        .iter()
        .filter(|test| !test.selected)
        .map(|test| test.filter.as_str())
        .collect();
    let mut remaining: Vec<&str> = tests
        .iter()
        .filter(|test| test.selected)
        .map(|test| test.filter.as_str())
        .collect();

    // Every module prefix of a selected name is a candidate, shortest first so
    // that a broad prefix is preferred over the narrower ones it contains.
    let mut candidates: Vec<(usize, String)> = Vec::new();
    for name in &remaining {
        candidates.extend(module_prefixes(name));
    }
    candidates.sort();
    candidates.dedup();

    let mut chosen: Vec<String> = Vec::new();
    for (_, prefix) in candidates {
        if !remaining.iter().any(|name| name.starts_with(&prefix)) {
            continue;
        }
        if clashes(&prefix, &unselected) {
            continue;
        }
        chosen.push(prefix.clone());
        remaining.retain(|name| !name.starts_with(&prefix));
        if remaining.is_empty() {
            break;
        }
    }

    // Whatever no prefix covers keeps its whole name, even when a substring of
    // it also matches a test that is not selected.
    chosen.extend(remaining.into_iter().map(str::to_owned));
    chosen.sort();
    chosen.dedup();
    chosen
}

/// The module prefixes of a test name, each with the number of segments it spans.
fn module_prefixes(filter: &str) -> Vec<(usize, String)> {
    let segments: Vec<&str> = filter.split("::").collect();
    (1..segments.len())
        .map(|count| (count, format!("{}::", segments[..count].join("::"))))
        .collect()
}

/// Whether `cargo test` would run an unselected test with this filter.
///
/// Cargo matches substrings, so a test name that merely contains the filter
/// runs too; a name that starts with it is one such case.
fn clashes(filter: &str, unselected: &[&str]) -> bool {
    unselected.iter().any(|name| name.contains(filter))
}

/// The position of a target kind in the order commands are listed, which is
/// the order its variants are declared in.
fn target_rank(kind: TargetKind) -> u8 {
    kind as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::{
        ChangeDetail, CrateId, CrateInfo, Location, RelPath, Span, SymbolFlags, SymbolKind,
        Visibility,
    };

    /// The location every symbol of a test graph shares.
    fn location() -> Location {
        let span = Span {
            start: 0,
            end: 10,
            start_line: 1,
            end_line: 1,
        };
        Location {
            file: RelPath::new("src/lib.rs"),
            span,
        }
    }

    /// Builds a graph from crate descriptions and symbol paths.
    ///
    /// A path ending in `!` names a test. A symbol that prefixes another one is
    /// a module, the rest are functions. A symbol belongs to the crate its
    /// first segment names, and its parent is the symbol one segment shorter.
    fn graph(crates: &[(&str, &str, TargetKind, &str)], symbols: &[&str]) -> RevisionGraph {
        let mut graph = RevisionGraph::default();
        for (name, package, target_kind, target_name) in crates {
            graph.crates.push(CrateInfo {
                name: (*name).to_owned(),
                package: (*package).to_owned(),
                target_kind: *target_kind,
                target_name: (*target_name).to_owned(),
                root_file: RelPath::new("src/lib.rs"),
            });
        }
        for spec in symbols {
            let is_test = spec.ends_with('!');
            let path = spec.strip_suffix('!').unwrap_or(spec);
            let is_module = symbols.iter().any(|other| {
                other
                    .strip_prefix(path)
                    .is_some_and(|rest| rest.starts_with("::"))
            });
            let kind = if is_module {
                SymbolKind::Module
            } else {
                SymbolKind::Function
            };
            let symbol_path = SymbolPath::new(path, kind);
            let krate = graph
                .crates
                .iter()
                .position(|info| info.name == symbol_path.crate_name())
                .map_or(CrateId(0), |index| CrateId(index as u32));
            let parent = path
                .rsplit_once("::")
                .and_then(|(prefix, _)| {
                    graph
                        .symbols
                        .iter()
                        .position(|symbol| symbol.path.path == prefix)
                })
                .map(|index| SymbolId(index as u32));
            graph.symbols.push(Symbol {
                path: symbol_path,
                krate,
                location: location(),
                visibility: Visibility::Private,
                signature_hash: 0,
                body_hash: 0,
                attr_hash: 0,
                body_sketch: Vec::new(),
                parent,
                flags: SymbolFlags {
                    is_test,
                    ..SymbolFlags::default()
                },
                impl_info: None,
            });
        }
        graph
    }

    /// A head graph of one library crate called `billing`.
    fn billing(symbols: &[&str]) -> RevisionGraph {
        graph(
            &[("billing", "billing", TargetKind::Lib, "billing")],
            symbols,
        )
    }

    /// A reached item: the function at `path`.
    fn item(path: &str) -> String {
        path.to_owned()
    }

    /// Selects the tests of `head` with the items at `reached` reached.
    fn select(head: &RevisionGraph, changes: &[Change], reached: &[String]) -> TestSelection {
        let reached = head
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, symbol)| reached.contains(&symbol.path.path))
            .map(|(index, _)| SymbolId(index as u32))
            .collect();
        select_tests(head, changes, &reached)
    }

    /// A change of `kind` to the symbol at `path`.
    fn change(kind: ChangeKind, path: &str, symbol_kind: SymbolKind) -> Change {
        Change {
            kind,
            symbol: SymbolPath::new(path, symbol_kind),
            old: Some(location()),
            new: Some(location()),
            detail: ChangeDetail::None,
        }
    }

    /// A head graph of `billing` with `count` tests at the crate root, named
    /// `t01`, `t02`, …, which no module prefix shortens, and an affected item
    /// for each of the first `selected` of them.
    fn root_tests(count: usize, selected: usize) -> (RevisionGraph, Vec<String>) {
        let mut names: Vec<String> = vec!["billing".to_owned()];
        names.extend((1..=count).map(|index| format!("billing::t{index:02}!")));
        let specs: Vec<&str> = names.iter().map(String::as_str).collect();
        let head = billing(&specs);
        let items = (1..=selected)
            .map(|index| item(&format!("billing::t{index:02}")))
            .collect();
        (head, items)
    }

    /// The filter lists of the commands, in command order.
    fn filters(selection: &TestSelection) -> Vec<Vec<String>> {
        selection
            .commands
            .iter()
            .map(|command| command.filters.clone())
            .collect()
    }

    #[test]
    fn a_reached_test_is_selected_and_a_changed_test_outranks_it() {
        let head = billing(&[
            "billing",
            "billing::tax::rounds!",
            "billing::tax::totals!",
            "billing::tax::gone!",
            "billing::tax::helper",
        ]);
        let changes = [
            change(
                ChangeKind::Body,
                "billing::tax::totals",
                SymbolKind::Function,
            ),
            change(
                ChangeKind::Removed,
                "billing::tax::gone",
                SymbolKind::Function,
            ),
        ];
        let items = [item("billing::tax::rounds"), item("billing::tax::totals")];
        let selection = select(&head, &changes, &items);

        assert_eq!(selection.total, 3, "the helper is not a test");
        let reasons: Vec<(&str, TestReason)> = selection
            .selected
            .iter()
            .map(|test| (test.symbol.path.as_str(), test.reason))
            .collect();
        assert_eq!(
            reasons,
            vec![
                ("billing::tax::rounds", TestReason::Reached),
                ("billing::tax::totals", TestReason::Changed),
            ],
            "the removed test is not selected"
        );
        assert_eq!(selection.selected[0].filter, "tax::rounds");
        assert_eq!(selection.commands.len(), 1);
        assert_eq!(selection.commands[0].tests, 2);
        assert_eq!(
            selection.commands[0].total, 3,
            "the target holds three tests"
        );
    }

    #[test]
    fn tests_inside_an_added_or_renamed_module_are_changed() {
        let head = billing(&[
            "billing",
            "billing::fresh",
            "billing::fresh::works!",
            "billing::moved",
            "billing::moved::works!",
        ]);
        let mut renamed = change(ChangeKind::Renamed, "billing::moved", SymbolKind::Module);
        renamed.detail = ChangeDetail::Renamed {
            from: SymbolPath::new("billing::old", SymbolKind::Module),
        };
        let changes = [
            change(ChangeKind::Added, "billing::fresh", SymbolKind::Module),
            renamed,
        ];
        let selection = select(&head, &changes, &[]);

        assert_eq!(selection.selected.len(), 2);
        assert!(
            selection
                .selected
                .iter()
                .all(|test| test.reason == TestReason::Changed)
        );
    }

    #[test]
    fn a_target_with_every_test_selected_gets_no_filters() {
        let head = billing(&[
            "billing",
            "billing::tax::rounds!",
            "billing::invoice::totals!",
        ]);
        let items = [
            item("billing::tax::rounds"),
            item("billing::invoice::totals"),
        ];
        let selection = select(&head, &[], &items);

        assert_eq!(selection.selected.len(), 2);
        assert_eq!(filters(&selection), vec![Vec::<String>::new()]);
        assert_eq!(selection.commands[0].tests, 2);
        assert_eq!(selection.commands[0].total, 2);
    }

    #[test]
    fn more_filters_than_the_limit_run_the_whole_target() {
        let (head, items) = root_tests(29, 9);
        let selection = select(&head, &[], &items);

        assert_eq!(selection.commands.len(), 1);
        let command = &selection.commands[0];
        assert_eq!(command.tests, 9);
        assert_eq!(command.total, 29);
        assert!(command.filters.is_empty());
        assert!(
            command.filters.is_empty() && command.tests < command.total,
            "a reader can tell the command runs more than was selected"
        );
    }

    #[test]
    fn the_limit_itself_still_names_every_selected_test() {
        let (head, items) = root_tests(28, 8);
        let selection = select(&head, &[], &items);

        let expected: Vec<String> = (1..=8).map(|index| format!("t{index:02}")).collect();
        assert_eq!(filters(&selection), vec![expected]);
        assert_eq!(selection.commands[0].tests, 8);
        assert_eq!(selection.commands[0].total, 28);
    }

    #[test]
    fn a_selection_covering_most_of_a_target_runs_the_whole_target() {
        let (head, items) = root_tests(10, 6);
        let selection = select(&head, &[], &items);

        assert_eq!(filters(&selection), vec![Vec::<String>::new()]);
        assert_eq!(selection.commands[0].tests, 6);
        assert_eq!(selection.commands[0].total, 10);
    }

    #[test]
    fn a_selection_just_under_the_share_keeps_its_filters() {
        let (head, items) = root_tests(10, 5);
        let selection = select(&head, &[], &items);

        let expected: Vec<String> = (1..=5).map(|index| format!("t{index:02}")).collect();
        assert_eq!(filters(&selection), vec![expected]);
        assert_eq!(selection.commands[0].tests, 5);
        assert_eq!(selection.commands[0].total, 10);
    }

    #[test]
    fn a_versioned_package_name_reaches_the_command_line() {
        let head = graph(
            &[("billing", "billing@1.2.0", TargetKind::Lib, "billing")],
            &["billing", "billing::tax::rounds!"],
        );
        let selection = select(&head, &[], &[item("billing::tax::rounds")]);

        assert_eq!(selection.commands.len(), 1);
        assert_eq!(selection.commands[0].package, "billing@1.2.0");
        assert_eq!(
            selection.commands[0].command_line(),
            "cargo test -p billing@1.2.0 --lib"
        );
    }

    #[test]
    fn tests_under_one_module_collapse_into_that_module() {
        let head = billing(&[
            "billing",
            "billing::tax::tests::rounds!",
            "billing::tax::tests::totals!",
            "billing::tax::rates::checks!",
            "billing::invoice::tests::totals!",
        ]);
        let items = [
            item("billing::tax::tests::rounds"),
            item("billing::tax::tests::totals"),
        ];
        let selection = select(&head, &[], &items);

        assert_eq!(filters(&selection), vec![vec!["tax::tests::".to_owned()]]);
        assert_eq!(selection.commands[0].tests, 2);
    }

    #[test]
    fn the_shortest_qualifying_prefix_wins() {
        let head = billing(&[
            "billing",
            "billing::tax::tests::rounds!",
            "billing::invoice::tests::totals!",
        ]);
        let selection = select(&head, &[], &[item("billing::tax::tests::rounds")]);

        assert_eq!(filters(&selection), vec![vec!["tax::".to_owned()]]);
    }

    #[test]
    fn a_prefix_that_also_matches_an_unselected_test_is_not_used() {
        let head = billing(&[
            "billing",
            "billing::tax::tests::rounds!",
            "billing::ratings::tax::tests::slow!",
        ]);
        let selection = select(&head, &[], &[item("billing::tax::tests::rounds")]);

        assert_eq!(
            filters(&selection),
            vec![vec!["tax::tests::rounds".to_owned()]],
            "no prefix is safe, so the whole name is used"
        );
    }

    #[test]
    fn commands_and_selected_tests_come_out_sorted() {
        let head = graph(
            &[
                ("alpha", "alpha", TargetKind::Lib, "alpha"),
                ("alpha_tests", "alpha", TargetKind::Test, "alpha_tests"),
                ("zeta", "zeta", TargetKind::Lib, "zeta"),
            ],
            &[
                "alpha",
                "alpha::a_check!",
                "alpha::b_check!",
                "alpha::c_check!",
                "alpha::zeta_other!",
                "alpha_tests",
                "alpha_tests::two!",
                "zeta",
                "zeta::api::one!",
            ],
        );
        let items = [
            item("zeta::api::one"),
            item("alpha::b_check"),
            item("alpha::a_check"),
            item("alpha_tests::two"),
        ];
        let selection = select(&head, &[], &items);

        let paths: Vec<&str> = selection
            .selected
            .iter()
            .map(|test| test.symbol.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "alpha::a_check",
                "alpha::b_check",
                "alpha_tests::two",
                "zeta::api::one",
            ]
        );
        let lines: Vec<(String, Vec<String>, u32, u32)> = selection
            .commands
            .iter()
            .map(|command| {
                (
                    command.command_line(),
                    command.filters.clone(),
                    command.tests,
                    command.total,
                )
            })
            .collect();
        assert_eq!(
            lines,
            vec![
                (
                    "cargo test -p alpha --lib -- a_check b_check".to_owned(),
                    vec!["a_check".to_owned(), "b_check".to_owned()],
                    2,
                    4,
                ),
                (
                    "cargo test -p alpha --test alpha_tests".to_owned(),
                    Vec::new(),
                    1,
                    1,
                ),
                ("cargo test -p zeta --lib".to_owned(), Vec::new(), 1, 1),
            ]
        );
    }
}
