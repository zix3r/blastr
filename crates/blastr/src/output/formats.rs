//! Markdown, `cargo test` and JSON renderings of an impact analysis.
//!
//! Everything here is a pure function of the data it is given, so the output
//! can be tested without a repository, pasted into a pull request or handed
//! to another tool.

use blastr_model::{
    Analysis, AnalysisOptions, Change, ChangeKind, EvidencePath, ImpactClass, ImpactItem,
    ImpactTags, Limit, Location,
};

use super::analysis::{path_summary, reason_text};
use super::{SCHEMA_VERSION, features_text, limit_text};

/// How many rows a table shows before the whole table is folded away.
const MAX_TABLE_ROWS: usize = 10;

/// What the tests section says when no test reaches the changes.
const NO_TEST: &str = "No test reaches the changed code.";

/// Renders the whole analysis as pretty JSON, with `schema_version` first.
pub fn analysis_json(analysis: &Analysis) -> Result<String, serde_json::Error> {
    with_schema_version(analysis)
}

/// Serialises a value and puts the schema version in front of its body.
///
/// `serde_json` keeps object keys sorted, so the version cannot be inserted
/// into the map; it is prepended to the rendered text instead.
fn with_schema_version<T>(value: &T) -> Result<String, serde_json::Error>
where
    T: serde::Serialize,
{
    let value = serde_json::to_value(value)?;
    let body = serde_json::to_string_pretty(&value)?;
    let inner = body
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(body.as_str());
    if inner.is_empty() {
        return Ok(format!("{{\n  \"schema_version\": {SCHEMA_VERSION}\n}}\n"));
    }
    let mut out = format!("{{\n  \"schema_version\": {SCHEMA_VERSION},");
    out.push_str(inner);
    out.push_str("}\n");
    Ok(out)
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
        .filter(|item| item.class == ImpactClass::Adaptation)
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
        format!("**{change_count} · {editing_text} · {behaving_text} · {tests_text}**"),
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
        blocks.push(section(
            "Needs editing",
            &table(&["Symbol", "Why", "Location", "Status"], rows),
        ));
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
    let row = super::row(change);
    vec![escape(&row.label), code(&row.symbol), escape(&row.location)]
}

/// The best path of the item's class, which is the first kept one.
fn best_path(item: &ImpactItem) -> Option<&EvidencePath> {
    item.paths
        .iter()
        .find(|path| path.class == item.class)
        .or_else(|| item.paths.first())
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
    let status = if item.tags.changed {
        "edited"
    } else {
        "not touched"
    };
    vec![
        code(&item.symbol.path),
        escape(&editing_why(item, analysis)),
        escape(&site_text(item)),
        status.to_owned(),
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

/// The fenced block of commands that run the selected tests, or the sentence
/// saying that none reach the changes.
fn tests_block(analysis: &Analysis) -> String {
    let lines: Vec<String> = analysis
        .tests
        .commands
        .iter()
        .map(|command| command.command_line())
        .collect();
    if lines.is_empty() {
        return NO_TEST.to_owned();
    }
    let mut out = String::from("```sh\n");
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
        AnalysisParams, Certainty, ChangeDetail, Comparison, Confidence, Delta, DiffResult,
        EdgeDetail, EdgeKind, EvidenceStep, ImpactLimits, Reason, RelPath, SelectedTest, Source,
        Span, SymbolKind, SymbolPath, TargetKind, TestCommand, TestReason, TestSelection,
    };

    use super::*;

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

    fn symbol_path(path: &str, kind: SymbolKind) -> SymbolPath {
        SymbolPath::new(path, kind)
    }

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

    fn change(
        kind: ChangeKind,
        path: &str,
        kind_of_symbol: SymbolKind,
        detail: ChangeDetail,
        old: Option<Location>,
        new: Option<Location>,
    ) -> Change {
        Change {
            kind,
            symbol: symbol_path(path, kind_of_symbol),
            old,
            new,
            detail,
        }
    }

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

    fn step(
        from: &str,
        to: &str,
        edge: EdgeKind,
        certainty: Certainty,
        site: Location,
    ) -> EvidenceStep {
        EvidenceStep {
            from: symbol_path(from, SymbolKind::Function),
            to: symbol_path(to, SymbolKind::Function),
            edge,
            certainty,
            detail: EdgeDetail::None,
            site,
        }
    }

    fn path(
        change: u32,
        reason: Reason,
        class: ImpactClass,
        score: f32,
        steps: Vec<EvidenceStep>,
    ) -> EvidencePath {
        EvidencePath {
            change,
            reason,
            class,
            steps,
            score,
        }
    }

    fn item(
        name: &str,
        class: ImpactClass,
        confidence: Confidence,
        paths: Vec<EvidencePath>,
    ) -> ImpactItem {
        ImpactItem {
            symbol: symbol_path(name, SymbolKind::Function),
            location: location("src/billing/lib.rs", 1),
            class,
            confidence,
            score: 0.5,
            distance: 1,
            paths,
            path_count: 1,
            tags: ImpactTags::default(),
        }
    }

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
        }
    }

    /// The analysis the mixed-markdown test renders: one adaptation, one
    /// behaviour item and one changed behaviour item that is not shown.
    fn mixed_analysis() -> Analysis {
        let mut analysis = empty_analysis();
        analysis.diff.changes = vec![
            change(
                ChangeKind::Signature,
                "billing::discount::apply_discount",
                SymbolKind::Function,
                ChangeDetail::None,
                Some(location("src/billing/discount.rs", 38)),
                Some(location("src/billing/discount.rs", 41)),
            ),
            change(
                ChangeKind::Variant,
                "billing::PaymentState::Refunded",
                SymbolKind::Variant,
                ChangeDetail::Member(Delta::Added),
                None,
                Some(location("src/billing/payment.rs", 8)),
            ),
            change(
                ChangeKind::NonSemantic,
                "billing::tax::vat_rate",
                SymbolKind::Function,
                ChangeDetail::None,
                Some(location("src/billing/tax.rs", 10)),
                Some(location("src/billing/tax.rs", 10)),
            ),
        ];
        analysis.diff.limits = vec![Limit::Unresolved { count: 1 }, Limit::Submodules];
        analysis.limits = ImpactLimits {
            candidate_steps: 2,
            below_floor: 1,
            ..ImpactLimits::default()
        };
        analysis.tests = TestSelection {
            selected: vec![
                SelectedTest {
                    symbol: symbol_path("billing::tax::tests::rounds", SymbolKind::Function),
                    location: location("src/billing/tax.rs", 40),
                    reason: TestReason::Reached,
                    filter: "tax::tests::rounds".to_owned(),
                },
                SelectedTest {
                    symbol: symbol_path("billing::tax::tests::brackets", SymbolKind::Function),
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

        let editing = item(
            "billing::invoice::total",
            ImpactClass::Adaptation,
            Confidence::Definite,
            vec![path(
                0,
                Reason::SignatureChanged,
                ImpactClass::Adaptation,
                1.0,
                vec![step(
                    "billing::invoice::total",
                    "billing::discount::apply_discount",
                    EdgeKind::Calls,
                    Certainty::Resolved,
                    location("src/billing/invoice.rs", 12),
                )],
            )],
        );

        let mut behaving = item(
            "billing::report::print",
            ImpactClass::Behaviour,
            Confidence::Likely,
            vec![path(
                2,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                0.5,
                vec![step(
                    "billing::report::print",
                    "billing::tax::vat_rate",
                    EdgeKind::Calls,
                    Certainty::Resolved,
                    location("src/billing/report.rs", 30),
                )],
            )],
        );
        behaving.tags.is_pub_api = true;

        let mut edited = item(
            "billing::discount::apply_discount",
            ImpactClass::Behaviour,
            Confidence::Likely,
            vec![path(
                0,
                Reason::CallsChanged,
                ImpactClass::Behaviour,
                0.4,
                vec![step(
                    "billing::discount::apply_discount",
                    "billing::discount::percent",
                    EdgeKind::Calls,
                    Certainty::Resolved,
                    location("src/billing/discount.rs", 20),
                )],
            )],
        );
        edited.tags.changed = true;

        analysis.items = vec![editing, behaving, edited];
        analysis
    }

    #[test]
    fn json_has_the_schema_version_first_and_round_trips() {
        let analysis = mixed_analysis();
        let text = analysis_json(&analysis).expect("the analysis serialises");
        assert!(
            text.starts_with("{\n  \"schema_version\": 1,\n"),
            "the schema version is not first: {text}"
        );
        assert!(text.ends_with("}\n"), "{text}");

        let mut value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        let version = value
            .as_object_mut()
            .expect("the analysis is an object")
            .remove("schema_version");
        assert_eq!(version, Some(serde_json::json!(1)));
        let back: Analysis = serde_json::from_value(value).expect("the analysis round-trips");
        assert_eq!(back, analysis);
    }

    #[test]
    fn the_mixed_analysis_renders_as_a_pull_request_comment() {
        let analysis = mixed_analysis();
        let adaptation_why = reason_text(Reason::SignatureChanged, &analysis.diff.changes[0]);
        let behaviour_why = path_summary(&analysis.items[1], &analysis);

        let expected = format!(
            "## Blastr impact report\n\
             \n\
             Comparing `4f2a91c` → `8be07d3` · features: default\n\
             \n\
             **2 changes · 1 needs editing · 1 may behave differently · 2 of 40 tests to run**\n\
             \n\
             ### Changes\n\
             \n\
             | Change | Symbol | Location |\n\
             | --- | --- | --- |\n\
             | signature | `billing::discount::apply_discount` | src/billing/discount.rs:41 |\n\
             | variant + | `billing::PaymentState::Refunded` | src/billing/payment.rs:8 |\n\
             \n\
             ### Needs editing\n\
             \n\
             | Symbol | Why | Location | Status |\n\
             | --- | --- | --- | --- |\n\
             | `billing::invoice::total` | {adaptation_why} | src/billing/invoice.rs:12 | not touched |\n\
             \n\
             ### May behave differently\n\
             \n\
             | Confidence | Symbol | Why | Location |\n\
             | --- | --- | --- | --- |\n\
             | likely | `billing::report::print` | {behaviour_why} (public API) | src/billing/report.rs:30 |\n\
             \n\
             ### Tests to run\n\
             \n\
             ```sh\n\
             cargo test -p billing --lib -- tax::tests::brackets tax::tests::rounds\n\
             ```\n\
             \n\
             ### Limits\n\
             \n\
             - submodules are not analysed\n\
             - 2 candidate steps in the listed paths\n\
             - 1 item below the score floor\n"
        );

        assert_eq!(
            analysis_markdown(&analysis, &AnalysisOptions::default()),
            expected
        );
    }

    #[test]
    fn long_tables_are_folded_into_a_details_block() {
        let mut analysis = empty_analysis();
        analysis.diff.changes = vec![change(
            ChangeKind::Body,
            "billing::tax::vat_rate",
            SymbolKind::Function,
            ChangeDetail::None,
            None,
            Some(location("src/billing/tax.rs", 10)),
        )];
        for index in 0..12 {
            let name = format!("billing::module::item_{index}");
            analysis.items.push(item(
                &name,
                ImpactClass::Adaptation,
                Confidence::Definite,
                vec![path(
                    0,
                    Reason::CallsChanged,
                    ImpactClass::Adaptation,
                    0.9,
                    vec![step(
                        &name,
                        "billing::tax::vat_rate",
                        EdgeKind::Calls,
                        Certainty::Resolved,
                        location("src/billing/module.rs", index + 3),
                    )],
                )],
            ));
        }

        let text = analysis_markdown(&analysis, &AnalysisOptions::default());
        assert!(
            text.contains("<details><summary>All 12 items</summary>\n\n| Symbol | Why |"),
            "{text}"
        );
        let (shown, folded) = text.split_once("<details>").expect("a details block");
        assert_eq!(shown.matches("| `billing::module::item_").count(), 10);
        assert_eq!(folded.matches("| `billing::module::item_").count(), 12);
        assert!(
            !shown.contains("billing::module::item_11"),
            "only the first ten rows stay visible: {shown}"
        );
        assert!(text.ends_with("</details>\n"), "{text}");
    }

    #[test]
    fn empty_sections_are_omitted() {
        let analysis = empty_analysis();
        let text = analysis_markdown(&analysis, &AnalysisOptions::default());
        assert_eq!(
            text,
            "## Blastr impact report\n\
             \n\
             Comparing `4f2a91c` → `8be07d3` · features: default\n\
             \n\
             **0 changes · 0 need editing · 0 may behave differently · 0 of 0 tests to run**\n"
        );
        for heading in ["Changes", "Needs editing", "May behave", "Tests", "Limits"] {
            assert!(!text.contains(heading), "{heading} must be omitted: {text}");
        }
    }

    #[test]
    fn pipes_in_cells_are_escaped() {
        let mut analysis = empty_analysis();
        let affected = item(
            "billing::Options::inline|boxed",
            ImpactClass::Adaptation,
            Confidence::Definite,
            vec![path(
                0,
                Reason::SignatureChanged,
                ImpactClass::Adaptation,
                1.0,
                vec![step(
                    "billing::Options::inline|boxed",
                    "billing::Options",
                    EdgeKind::UsesType,
                    Certainty::Resolved,
                    location("src/a|b.rs", 5),
                )],
            )],
        );
        analysis.diff.changes = vec![change(
            ChangeKind::Signature,
            "billing::Options",
            SymbolKind::Struct,
            ChangeDetail::None,
            None,
            Some(location("src/options.rs", 2)),
        )];
        analysis.items = vec![affected];

        let text = analysis_markdown(&analysis, &AnalysisOptions::default());
        assert!(
            text.contains("| `billing::Options::inline\\|boxed` |"),
            "{text}"
        );
        assert!(text.contains("| src/a\\|b.rs:5 |"), "{text}");
        // Every row must still have exactly the four columns of its header.
        for line in text.lines().filter(|line| line.starts_with("| `billing::")) {
            assert_eq!(
                line.matches(" | ").count(),
                3,
                "the cell was not escaped: {line}"
            );
        }
    }

    #[test]
    fn tests_markdown_shows_the_commands_or_says_there_are_none() {
        let analysis = mixed_analysis();
        assert_eq!(
            tests_markdown(&analysis),
            "## Tests to run (2 of 40)\n\
             \n\
             ```sh\n\
             cargo test -p billing --lib -- tax::tests::brackets tax::tests::rounds\n\
             ```\n"
        );

        let empty = empty_analysis();
        assert_eq!(
            tests_markdown(&empty),
            "## Tests to run (0 of 0)\n\nNo test reaches the changed code.\n"
        );
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
