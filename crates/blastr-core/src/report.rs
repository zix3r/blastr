//! Markdown and `cargo test` renderings of an impact analysis.
//!
//! Everything here is a pure function of the data it is given, so the output
//! can be tested without a repository, pasted into a pull request or handed
//! to another tool. The window copies these texts, and a later agent
//! interface hands over the same ones.

use blastr_model::{
    Analysis, AnalysisOptions, Change, ChangeKind, ImpactClass, ImpactItem, ImpactTags, Limit,
    Location,
};

use crate::wording::{
    best_path, change_row as row, features_text, limit_text, path_summary, reason_text,
    target_label,
};

/// What a report says where the command lines are a format away.
pub const COMMANDS_HINT: &str = "Copy the test commands from the Tests list.";

/// How many rows a table shows before the whole table is folded away.
const MAX_TABLE_ROWS: usize = 10;

/// Widest command line a Markdown block repeats before leaving them to the
/// Tests list.
const MAX_COMMAND_WIDTH: usize = 120;

/// What the tests section says when no test reaches the changes.
const NO_TEST: &str = "No test reaches the changed code.";

/// One sentence saying how many adaptations the change already made itself.
pub fn edited_summary(count: usize) -> String {
    if count == 1 {
        "1 dependant was edited together with the change.".to_owned()
    } else {
        format!("{count} dependants were edited together with the change.")
    }
}

/// Renders the analysis as a pull-request comment.
///
/// `options` only supplies the feature list of the opening line; the numbers
/// come from the analysis itself.
pub fn analysis_markdown(analysis: &Analysis, options: &AnalysisOptions) -> String {
    let changes: Vec<&Change> = analysis
        .diff
        .changes
        .iter()
        .filter(|change| change.kind != ChangeKind::NonSemantic)
        .collect();
    let editing: Vec<&ImpactItem> = analysis
        .items
        .iter()
        .filter(|item| item.class == ImpactClass::Adaptation && !item.tags.changed)
        .collect();
    let edited: Vec<&ImpactItem> = analysis
        .items
        .iter()
        .filter(|item| item.class == ImpactClass::Adaptation && item.tags.changed)
        .collect();
    let behaving: Vec<&ImpactItem> = analysis
        .items
        .iter()
        .filter(|item| item.class == ImpactClass::Behaviour && !item.tags.changed)
        .collect();

    let editing_count = editing.len();
    let editing_text = if editing_count == 1 {
        "1 needs editing".to_owned()
    } else {
        format!("{editing_count} need editing")
    };
    let edited_text = if edited.is_empty() {
        String::new()
    } else {
        format!(" · {} edited with the change", edited.len())
    };
    let behaving_count = behaving.len();
    let behaving_text = format!("{behaving_count} may behave differently");
    let selected = analysis.tests.selected.len();
    let total = analysis.tests.total;
    let tests_text = format!("{selected} of {total} tests to run");

    let base = &analysis.diff.comparison.base;
    let head = &analysis.diff.comparison.head;
    let features = features_text(options);
    let change_count = count_text(changes.len(), "change", "changes");

    let mut blocks = vec![
        "## Blastr impact report".to_owned(),
        format!("Comparing `{base}` → `{head}` · features: {features}"),
        format!(
            "**{change_count} · {editing_text}{edited_text} · {behaving_text} · {tests_text}**"
        ),
    ];

    if !changes.is_empty() {
        let rows = changes
            .iter()
            .copied()
            .map(change_row)
            .collect::<Vec<Vec<String>>>();
        blocks.push(section(
            "Changes",
            &table(&["Change", "Symbol", "Location"], rows),
        ));
    }
    if !editing.is_empty() {
        let rows = editing
            .iter()
            .copied()
            .map(|item| editing_row(item, analysis))
            .collect::<Vec<Vec<String>>>();
        let mut body = table(&["Symbol", "Why", "Location"], rows);
        if !edited.is_empty() {
            body.push_str("\n\n");
            body.push_str(&edited_summary(edited.len()));
        }
        blocks.push(section("Needs editing", &body));
    } else if !edited.is_empty() {
        blocks.push(edited_summary(edited.len()));
    }
    if !behaving.is_empty() {
        let rows = behaving
            .iter()
            .copied()
            .map(|item| behaving_row(item, analysis))
            .collect::<Vec<Vec<String>>>();
        blocks.push(section(
            "May behave differently",
            &table(&["Confidence", "Symbol", "Why", "Location"], rows),
        ));
    }
    if analysis.tests.total > 0 {
        blocks.push(section("Tests to run", &tests_block(analysis)));
    }
    if let Some(limits) = limits_block(analysis) {
        blocks.push(limits);
    }

    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}

/// Renders the selected tests as a short report.
pub fn tests_markdown(analysis: &Analysis) -> String {
    let selected = analysis.tests.selected.len();
    let total = analysis.tests.total;
    let block = tests_block(analysis);
    format!("## Tests to run ({selected} of {total})\n\n{block}\n")
}

/// Renders the selected tests as the command lines that run them.
///
/// Returns an empty string when there is nothing to run.
pub fn tests_cargo(analysis: &Analysis) -> String {
    let lines: Vec<String> = analysis
        .tests
        .commands
        .iter()
        .map(|command| command.command_line())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// One titled section: its heading, a blank line and its body.
fn section(title: &str, body: &str) -> String {
    format!("### {title}\n\n{body}")
}

/// Counts of one thing, with the right singular or plural noun.
fn count_text(count: usize, one: &str, many: &str) -> String {
    let noun = if count == 1 { one } else { many };
    format!("{count} {noun}")
}

/// Renders one table, folding all of it into a `<details>` block when it is
/// longer than [`MAX_TABLE_ROWS`] rows.
fn table(header: &[&str], rows: Vec<Vec<String>>) -> String {
    let total = rows.len();
    if total <= MAX_TABLE_ROWS {
        return render_table(header, &rows);
    }
    let head = render_table(header, &rows[..MAX_TABLE_ROWS]);
    let all = render_table(header, &rows);
    format!("{head}\n\n<details><summary>All {total} items</summary>\n\n{all}\n\n</details>")
}

/// Renders a Markdown table with the given rows.
fn render_table(header: &[&str], rows: &[Vec<String>]) -> String {
    let joined = header.join(" | ");
    let mut out = format!("| {joined} |\n|");
    for _ in header {
        out.push_str(" --- |");
    }
    for row in rows {
        out.push_str("\n| ");
        out.push_str(&row.join(" | "));
        out.push_str(" |");
    }
    out
}

/// Makes a value safe inside a table cell.
fn escape(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Wraps a value in backticks, escaping what would break the cell.
fn code(text: &str) -> String {
    let escaped = escape(text);
    format!("`{escaped}`")
}

/// One row of the `Changes` table.
fn change_row(change: &Change) -> Vec<String> {
    let row = row(change);
    vec![escape(&row.label), code(&row.symbol), escape(&row.location)]
}

/// Where the last step of the best path is written.
fn site_text(item: &ImpactItem) -> String {
    let location: &Location = best_path(item)
        .and_then(|path| path.steps.last())
        .map(|step| &step.site)
        .unwrap_or(&item.location);
    let file = &location.file;
    let line = location.span.start_line;
    format!("{file}:{line}")
}

/// Why an adaptation item must be edited.
fn editing_why(item: &ImpactItem, analysis: &Analysis) -> String {
    let Some(path) = best_path(item) else {
        return path_summary(item, analysis);
    };
    match analysis.diff.changes.get(path.change as usize) {
        Some(change) => reason_text(path.reason, change),
        None => path_summary(item, analysis),
    }
}

/// The tags of an item, as the suffixes of its `Why` cell.
fn tags_suffix(tags: &ImpactTags) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if tags.is_test {
        parts.push("(test)");
    }
    if tags.is_main {
        parts.push("(main)");
    }
    if tags.is_pub_api {
        parts.push("(public API)");
    }
    if parts.is_empty() {
        return String::new();
    }
    let joined = parts.join(" ");
    format!(" {joined}")
}

/// One row of the `Needs editing` table.
fn editing_row(item: &ImpactItem, analysis: &Analysis) -> Vec<String> {
    vec![
        code(&item.symbol.path),
        escape(&editing_why(item, analysis)),
        escape(&site_text(item)),
    ]
}

/// One row of the `May behave differently` table.
fn behaving_row(item: &ImpactItem, analysis: &Analysis) -> Vec<String> {
    let summary = path_summary(item, analysis);
    let tags = tags_suffix(&item.tags);
    let why = format!("{summary}{tags}");
    vec![
        escape(item.confidence.as_str()),
        code(&item.symbol.path),
        escape(&why),
        escape(&site_text(item)),
    ]
}

/// The table of targets behind the selected tests, followed by the commands
/// that run them, or by the sentence saying that none reach the changes.
pub fn tests_block(analysis: &Analysis) -> String {
    let commands = &analysis.tests.commands;
    if commands.is_empty() {
        return NO_TEST.to_owned();
    }

    let rows: Vec<Vec<String>> = commands
        .iter()
        .map(|command| {
            vec![
                escape(&command.package),
                escape(&target_label(command)),
                format!("{} of {}", command.tests, command.total),
            ]
        })
        .collect();
    let mut out = table(&["Package", "Target", "Selected"], rows);

    let lines: Vec<String> = commands
        .iter()
        .map(|command| command.command_line())
        .collect();
    out.push_str("\n\n");
    if lines
        .iter()
        .any(|line| line.chars().count() > MAX_COMMAND_WIDTH)
    {
        out.push_str(COMMANDS_HINT);
        return out;
    }
    out.push_str("```sh\n");
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("```");
    out
}

/// The `Limits` section, or `None` when nothing limits the result.
fn limits_block(analysis: &Analysis) -> Option<String> {
    let mut bullets: Vec<String> = analysis
        .diff
        .limits
        .iter()
        .filter(|limit| !matches!(limit, Limit::Unresolved { .. }))
        .map(limit_text)
        .collect();

    let limits = &analysis.limits;
    push_count(
        &mut bullets,
        limits.unresolved_total,
        "unresolved reference",
        "unresolved references",
        "in the head graph",
    );
    push_count(
        &mut bullets,
        limits.unresolved_in_reach,
        "unresolved reference",
        "unresolved references",
        "inside changed or listed code",
    );
    push_count(
        &mut bullets,
        limits.candidate_steps,
        "candidate step",
        "candidate steps",
        "in the listed paths",
    );
    push_count(
        &mut bullets,
        limits.coarse_rules,
        "coarse rule match",
        "coarse rule matches",
        "that cannot tell exactly who is affected",
    );
    let depth = analysis.params.depth;
    let beyond = format!("beyond depth {depth}");
    push_count(
        &mut bullets,
        limits.beyond_depth,
        "item only reachable",
        "items only reachable",
        &beyond,
    );
    push_count(
        &mut bullets,
        limits.below_floor,
        "item",
        "items",
        "below the score floor",
    );
    push_count(
        &mut bullets,
        limits.below_confidence,
        "item",
        "items",
        "below the minimum confidence",
    );
    push_count(
        &mut bullets,
        limits.ignored,
        "item",
        "items",
        "in ignored files",
    );

    if bullets.is_empty() {
        return None;
    }
    let body = bullets
        .iter()
        .map(|bullet| format!("- {bullet}"))
        .collect::<Vec<String>>()
        .join("\n");
    Some(section("Limits", &body))
}

/// Appends one bullet for a counter that is not zero.
fn push_count(bullets: &mut Vec<String>, count: u32, one: &str, many: &str, suffix: &str) {
    if count > 0 {
        let counted = count_text(count as usize, one, many);
        bullets.push(format!("{counted} {suffix}"));
    }
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        AnalysisParams, Certainty, ChangeDetail, Comparison, Confidence, DiffResult, EdgeDetail,
        EdgeKind, EvidencePath, EvidenceStep, ImpactLimits, Reason, RelPath, SelectedTest, Source,
        Span, SymbolKind, SymbolPath, TargetKind, TestCommand, TestReason, TestSelection,
    };

    use super::*;

    /// A source range at `line` of `file`.
    fn location(file: &str, line: u32) -> Location {
        Location {
            file: RelPath::new(file),
            span: Span {
                start: 0,
                end: 10,
                start_line: line,
                end_line: line,
            },
        }
    }

    /// The two sides of the comparison every test renders.
    fn comparison() -> Comparison {
        Comparison {
            base: Source::Tree {
                id: "0f2a91c000000000000000000000000000000000".to_owned(),
                label: "4f2a91c".to_owned(),
            },
            head: Source::Tree {
                id: "0be07d3000000000000000000000000000000000".to_owned(),
                label: "8be07d3".to_owned(),
            },
        }
    }

    /// An analysis with nothing in it.
    fn empty_analysis() -> Analysis {
        Analysis {
            diff: DiffResult {
                comparison: comparison(),
                base_key: None,
                head_key: None,
                changed_files: Vec::new(),
                changes: Vec::new(),
                limits: Vec::new(),
            },
            items: Vec::new(),
            tests: TestSelection::default(),
            limits: ImpactLimits::default(),
            params: AnalysisParams::default(),
        }
    }

    /// One classified change of `path`.
    fn change(kind: ChangeKind, path: &str, at: Location) -> Change {
        Change {
            kind,
            symbol: SymbolPath::new(path, SymbolKind::Function),
            old: Some(at.clone()),
            new: Some(at),
            detail: ChangeDetail::None,
        }
    }

    /// One evidence step from `from` to `to`.
    fn step(from: &str, to: &str, site: Location) -> EvidenceStep {
        EvidenceStep {
            from: SymbolPath::new(from, SymbolKind::Function),
            to: SymbolPath::new(to, SymbolKind::Function),
            edge: EdgeKind::Calls,
            certainty: Certainty::Resolved,
            detail: EdgeDetail::None,
            site,
        }
    }

    /// An adaptation reached by a call to the first change of the analysis.
    fn adaptation(name: &str, edited: bool) -> ImpactItem {
        ImpactItem {
            symbol: SymbolPath::new(name, SymbolKind::Function),
            location: location("src/billing/lib.rs", 1),
            class: ImpactClass::Adaptation,
            confidence: Confidence::Definite,
            score: 0.9,
            distance: 1,
            paths: vec![EvidencePath {
                change: 0,
                reason: Reason::CallsChanged,
                class: ImpactClass::Adaptation,
                steps: vec![step(
                    name,
                    "billing::tax::vat_rate",
                    location("src/billing/invoice.rs", 12),
                )],
                score: 0.9,
            }],
            path_count: 1,
            tags: ImpactTags {
                changed: edited,
                ..ImpactTags::default()
            },
        }
    }

    /// A `cargo test` command for one target.
    fn command(
        package: &str,
        kind: TargetKind,
        name: &str,
        filters: &[&str],
        tests: u32,
    ) -> TestCommand {
        TestCommand {
            package: package.to_owned(),
            target_kind: kind,
            target_name: name.to_owned(),
            filters: filters.iter().map(|filter| (*filter).to_owned()).collect(),
            tests,
            total: tests,
        }
    }

    #[test]
    fn the_edited_summary_counts_in_the_plural() {
        assert_eq!(
            edited_summary(1),
            "1 dependant was edited together with the change."
        );
        assert_eq!(
            edited_summary(3),
            "3 dependants were edited together with the change."
        );
    }

    #[test]
    fn tests_markdown_shows_the_targets_and_the_commands_or_says_there_are_none() {
        let mut analysis = empty_analysis();
        analysis.tests = TestSelection {
            selected: vec![
                SelectedTest {
                    symbol: SymbolPath::new("billing::tax::tests::rounds", SymbolKind::Function),
                    location: location("src/billing/tax.rs", 40),
                    reason: TestReason::Reached,
                    filter: "tax::tests::rounds".to_owned(),
                },
                SelectedTest {
                    symbol: SymbolPath::new("billing::tax::tests::brackets", SymbolKind::Function),
                    location: location("src/billing/tax.rs", 50),
                    reason: TestReason::Reached,
                    filter: "tax::tests::brackets".to_owned(),
                },
            ],
            total: 40,
            commands: vec![command(
                "billing",
                TargetKind::Lib,
                "billing",
                &["tax::tests::brackets", "tax::tests::rounds"],
                2,
            )],
        };
        assert_eq!(
            tests_markdown(&analysis),
            "## Tests to run (2 of 40)\n\
             \n\
             | Package | Target | Selected |\n\
             | --- | --- | --- |\n\
             | billing | unit tests | 2 of 2 |\n\
             \n\
             ```sh\n\
             cargo test -p billing --lib -- tax::tests::brackets tax::tests::rounds\n\
             ```\n"
        );

        assert_eq!(
            tests_markdown(&empty_analysis()),
            "## Tests to run (0 of 0)\n\nNo test reaches the changed code.\n"
        );
    }

    #[test]
    fn a_command_line_that_is_too_long_is_left_to_the_tests_list() {
        let mut analysis = empty_analysis();
        analysis.tests = TestSelection {
            selected: vec![SelectedTest {
                symbol: SymbolPath::new("billing::tax::tests::rounds", SymbolKind::Function),
                location: location("src/billing/tax.rs", 40),
                reason: TestReason::Reached,
                filter: "tax::tests::rounds".to_owned(),
            }],
            total: 40,
            commands: vec![command(
                "billing",
                TargetKind::Lib,
                "billing",
                &[
                    "tax::tests::brackets",
                    "tax::tests::brackets_and_rates",
                    "tax::tests::brackets_for_regions",
                    "tax::tests::rounds",
                    "tax::tests::rounds_for_regions",
                ],
                1,
            )],
        };

        let expected = format!(
            "| Package | Target | Selected |\n| --- | --- | --- |\n\
             | billing | unit tests | 1 of 1 |\n\n{COMMANDS_HINT}"
        );
        assert_eq!(tests_block(&analysis), expected);
        assert!(
            tests_markdown(&analysis).contains(COMMANDS_HINT),
            "{}",
            tests_markdown(&analysis)
        );
        assert!(
            !tests_markdown(&analysis).contains("```sh"),
            "the block is too wide"
        );
        assert!(tests_cargo(&analysis).starts_with("cargo test -p billing --lib -- tax::"));
    }

    #[test]
    fn markdown_summarises_the_adaptations_the_change_already_made() {
        let mut analysis = empty_analysis();
        analysis.diff.changes = vec![change(
            ChangeKind::Body,
            "billing::tax::vat_rate",
            location("src/billing/tax.rs", 10),
        )];
        analysis.items = vec![
            adaptation("billing::invoice::total", false),
            adaptation("billing::invoice::refund", true),
            adaptation("billing::invoice::void", true),
        ];

        let text = analysis_markdown(&analysis, &AnalysisOptions::default());
        assert!(
            text.contains(
                "**1 change · 1 needs editing · 2 edited with the change · \
                 0 may behave differently · 0 of 0 tests to run**"
            ),
            "{text}"
        );
        assert!(text.contains("| Symbol | Why | Location |\n"), "{text}");
        assert!(text.contains("`billing::invoice::total`"), "{text}");
        assert!(!text.contains("`billing::invoice::refund`"), "{text}");
        assert!(
            text.contains("\n\n2 dependants were edited together with the change.\n"),
            "{text}"
        );
    }

    #[test]
    fn markdown_keeps_the_edited_summary_without_a_table() {
        let mut analysis = empty_analysis();
        analysis.items = vec![adaptation("billing::invoice::refund", true)];

        let text = analysis_markdown(&analysis, &AnalysisOptions::default());
        assert!(
            text.contains(
                "**0 changes · 0 need editing · 1 edited with the change · \
                 0 may behave differently · 0 of 0 tests to run**"
            ),
            "{text}"
        );
        assert!(
            text.contains("\n\n1 dependant was edited together with the change.\n"),
            "{text}"
        );
        assert!(!text.contains("### Needs editing"), "{text}");
    }

    #[test]
    fn a_cell_with_a_bar_is_escaped() {
        let mut analysis = empty_analysis();
        let mut affected = adaptation("billing::invoice::total", false);
        affected.symbol = SymbolPath::new("billing::Options::inline|boxed", SymbolKind::Function);
        affected.location = location("src/a|b.rs", 5);
        affected.paths[0].steps[0].site = location("src/a|b.rs", 5);
        analysis.diff.changes = vec![change(
            ChangeKind::Signature,
            "billing::Options",
            location("src/options.rs", 2),
        )];
        analysis.items = vec![affected];

        let text = analysis_markdown(&analysis, &AnalysisOptions::default());
        assert!(
            text.contains(r"| `billing::Options::inline\|boxed` |"),
            "{text}"
        );
        assert!(text.contains(r"| src/a\|b.rs:5 |"), "{text}");
        // Every row must still have exactly the three columns of its header.
        for line in text.lines().filter(|line| line.starts_with("| `billing::")) {
            assert_eq!(
                line.matches(" | ").count(),
                2,
                "the cell was not escaped: {line}"
            );
        }
    }

    #[test]
    fn tests_cargo_lists_one_command_per_line() {
        let mut analysis = empty_analysis();
        analysis.tests.commands = vec![
            command("billing", TargetKind::Lib, "billing", &["tax"], 3),
            command("billing", TargetKind::Test, "checkout", &[], 2),
        ];
        assert_eq!(
            tests_cargo(&analysis),
            "cargo test -p billing --lib -- tax\ncargo test -p billing --test checkout\n"
        );
        assert_eq!(tests_cargo(&empty_analysis()), "");
    }
}
