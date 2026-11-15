//! Test selection: which tests a set of changes touches or reaches, and the
//! `cargo test` commands that run exactly those tests.
//!
//! A test is a head-side symbol flagged as a test. It is selected when a
//! change touches it, when an added or renamed item contains it, or when
//! impact analysis reaches it. Selected tests are grouped by their Cargo
//! target and narrowed to module filters, because running a whole target is
//! wasteful once only a few of its tests are affected.

use std::collections::{BTreeMap, HashSet};

use blastr_model::{
    Change, ChangeKind, ImpactItem, RevisionGraph, SelectedTest, Symbol, SymbolId, SymbolPath,
    TargetKind, TestCommand, TestReason, TestSelection,
};

/// Selects the tests the changes touch or reach, grouped into commands.
pub(crate) fn select_tests(
    head: &RevisionGraph,
    changes: &[Change],
    items: &[ImpactItem],
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
    let reached: HashSet<&SymbolPath> = items.iter().map(|item| &item.symbol).collect();

    let mut total = 0_u32;
    let mut selected: Vec<SelectedTest> = Vec::new();
    let mut targets: BTreeMap<(String, u8, String), (TargetKind, Vec<TargetTest>)> =
        BTreeMap::new();

    for symbol in &head.symbols {
        if !symbol.flags.is_test {
            continue;
        }
        total = total.saturating_add(1);

        let reason = if touched.contains(&symbol.path)
            || has_introduced_ancestor(&head.symbols, symbol, &introduced)
        {
            Some(TestReason::Changed)
        } else if reached.contains(&symbol.path) {
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
        let filters = if count as usize == tests.len() {
            Vec::new()
        } else {
            command_filters(&tests)
        };
        commands.push(TestCommand {
            package,
            target_kind,
            target_name,
            filters,
            tests: count,
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

/// The filters of a command whose target holds both selected and unselected tests.
fn command_filters(tests: &[TargetTest]) -> Vec<String> {
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
        ChangeDetail, Confidence, CrateId, CrateInfo, ImpactClass, ImpactTags, Location, RelPath,
        Span, SymbolFlags, SymbolKind, Visibility,
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

    /// An affected item for the function at `path`.
    fn item(path: &str) -> ImpactItem {
        ImpactItem {
            symbol: SymbolPath::new(path, SymbolKind::Function),
            location: location(),
            class: ImpactClass::Behaviour,
            confidence: Confidence::Definite,
            score: 1.0,
            distance: 1,
            paths: Vec::new(),
            path_count: 1,
            tags: ImpactTags::default(),
        }
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
        let selection = select_tests(&head, &changes, &items);

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
        let selection = select_tests(&head, &changes, &[]);

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
        let selection = select_tests(&head, &[], &items);

        assert_eq!(selection.selected.len(), 2);
        assert_eq!(filters(&selection), vec![Vec::<String>::new()]);
        assert_eq!(selection.commands[0].tests, 2);
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
        let selection = select_tests(&head, &[], &items);

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
        let selection = select_tests(&head, &[], &[item("billing::tax::tests::rounds")]);

        assert_eq!(filters(&selection), vec![vec!["tax::".to_owned()]]);
    }

    #[test]
    fn a_prefix_that_also_matches_an_unselected_test_is_not_used() {
        let head = billing(&[
            "billing",
            "billing::tax::tests::rounds!",
            "billing::ratings::tax::tests::slow!",
        ]);
        let selection = select_tests(&head, &[], &[item("billing::tax::tests::rounds")]);

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
        let selection = select_tests(&head, &[], &items);

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
        let lines: Vec<(String, Vec<String>, u32)> = selection
            .commands
            .iter()
            .map(|command| {
                (
                    command.command_line(),
                    command.filters.clone(),
                    command.tests,
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
                ),
                (
                    "cargo test -p alpha --test alpha_tests".to_owned(),
                    Vec::new(),
                    1,
                ),
                ("cargo test -p zeta --lib".to_owned(), Vec::new(), 1),
            ]
        );
    }
}
