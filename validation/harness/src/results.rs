//! The result types of the checks, their arithmetic, and the Markdown report.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use blastr_model::AnalysisParams;
use serde::{Deserialize, Serialize};

use crate::Result;

/// One case of a check: one edit or mutation, and how Blastr did on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    /// Identifier, unique within the project.
    pub id: String,
    /// The edit or mutation kind.
    pub kind: String,
    /// The symbol the case changes.
    pub symbol: String,
    /// The file it changes.
    pub file: String,
    /// The item before the change, clipped.
    pub before: String,
    /// The item after the change, clipped.
    pub after: String,
    /// Why the case was not scored, when it was not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded: Option<String>,
    /// What had to be found: the symbols the compiler reported errors in, or
    /// the tests that failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected: Vec<String>,
    /// What Blastr listed: its *needs editing* items, or the tests it selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listed: Vec<String>,
    /// Expected entries Blastr did not list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub misses: Vec<String>,
    /// Listed entries that had no error and did not fail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub false_alarms: Vec<String>,
    /// Everything that transitively depends on the change, by plain reverse
    /// reachability.
    #[serde(default)]
    pub reachable: u32,
    /// How many items Blastr listed, whatever their class.
    #[serde(default)]
    pub blastr_items: u32,
    /// Items that could not be confirmed either way, because the compiler did
    /// not build the crate they are in.
    #[serde(default)]
    pub unconfirmed: u32,
    /// How many tests the suite has, for the test check.
    #[serde(default)]
    pub suite_total: u32,
    /// How much of the suite Blastr selected, as a percentage.
    #[serde(default)]
    pub suite_selected_percent: f32,
    /// Time Blastr took for the case, in milliseconds.
    #[serde(default)]
    pub blastr_ms: u64,
    /// Time Cargo took for the case, in milliseconds.
    #[serde(default)]
    pub cargo_ms: u64,
    /// Anything the case needs to be read correctly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl CaseResult {
    /// Whether the case counts towards the numbers.
    pub fn scored(&self) -> bool {
        self.excluded.is_none()
    }
}

/// The counts of one kind, or of a whole check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// The kind, or `all`.
    pub kind: String,
    /// Cases planned, scored or not.
    pub cases: u32,
    /// Cases that could not be judged.
    pub excluded: u32,
    /// Entries that had to be listed and were not.
    pub misses: u32,
    /// Entries listed that did not have to be.
    pub false_alarms: u32,
    /// Entries that had to be found.
    pub expected: u32,
    /// Entries Blastr listed.
    pub listed: u32,
    /// Symbols that transitively depend on the changes, over the scored cases.
    pub reachable: u32,
    /// Items Blastr listed over the scored cases, whatever their class.
    pub blastr_items: u32,
    /// Items left out of both sides because the compiler did not build their
    /// crate, so nothing could confirm or refute them.
    pub unconfirmed: u32,
    /// Tests selected, over the scored cases of the test check.
    pub suite_selected: u32,
    /// Tests in the suite, over the scored cases of the test check.
    pub suite_total: u32,
}

impl Summary {
    /// Sums up `cases` of one kind.
    pub fn of(kind: &str, cases: &[&CaseResult]) -> Summary {
        let mut summary = Summary {
            kind: kind.to_owned(),
            ..Summary::default()
        };
        for case in cases {
            summary.cases = summary.cases.saturating_add(1);
            if !case.scored() {
                summary.excluded = summary.excluded.saturating_add(1);
                continue;
            }
            summary.misses = summary.misses.saturating_add(case.misses.len() as u32);
            summary.false_alarms = summary
                .false_alarms
                .saturating_add(case.false_alarms.len() as u32);
            summary.expected = summary.expected.saturating_add(case.expected.len() as u32);
            summary.listed = summary.listed.saturating_add(case.listed.len() as u32);
            summary.reachable = summary.reachable.saturating_add(case.reachable);
            summary.blastr_items = summary.blastr_items.saturating_add(case.blastr_items);
            summary.unconfirmed = summary.unconfirmed.saturating_add(case.unconfirmed);
            summary.suite_selected = summary
                .suite_selected
                .saturating_add(case.listed.len() as u32);
            summary.suite_total = summary.suite_total.saturating_add(case.suite_total);
        }
        summary
    }

    /// The share of the whole suite the selection covers, in per cent.
    pub fn selection_percent(&self) -> f32 {
        if self.suite_total == 0 {
            return 0.0;
        }
        let selected = f64::from(self.suite_selected);
        let total = f64::from(self.suite_total);
        (selected * 100.0 / total) as f32
    }

    /// The size of Blastr's list as a share of everything that transitively
    /// depends on the changes, in per cent. Above 100 means Blastr lists more
    /// than plain reverse reachability would.
    pub fn listed_ratio_percent(&self) -> f32 {
        if self.reachable == 0 {
            return 0.0;
        }
        let listed = f64::from(self.blastr_items);
        let reachable = f64::from(self.reachable);
        (listed * 100.0 / reachable) as f32
    }
}

/// What one check found in one project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckReport {
    /// The project, by directory name.
    pub project: String,
    /// `compiler` or `tests`.
    pub check: String,
    /// The revision of the sample.
    pub revision: String,
    /// The Blastr version the result came from.
    pub blastr_version: String,
    /// The Blastr build fingerprint.
    pub blastr_build: String,
    /// The rust-analyzer libraries the build uses.
    pub rust_analyzer: String,
    /// The parameters the analysis ran with.
    pub params: AnalysisParams,
    /// Crates in the project.
    pub crates: u32,
    /// Symbols in the graph.
    pub symbols: u32,
    /// Edges in the graph.
    pub edges: u32,
    /// Edits that could not be planned, because the item has no syntax of its
    /// own or takes no such edit.
    pub unplanned: u32,
    /// The per-kind counts.
    pub kinds: Vec<Summary>,
    /// The whole check.
    pub totals: Summary,
    /// The cases, in plan order.
    pub cases: Vec<CaseResult>,
    /// How many cases were planned before sampling.
    #[serde(default)]
    pub planned: u32,
    /// How many cases the check sampled, when it was limited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled: Option<u32>,
    /// The seed of the sample, when one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// How long the single project load took, in milliseconds.
    pub base_load_ms: u64,
    /// The mean time Blastr needed per case, in milliseconds.
    pub mean_blastr_ms: u64,
    /// The mean time Cargo needed per case, in milliseconds.
    pub mean_cargo_ms: u64,
    /// How long one warm `cargo check` of the unchanged project took.
    pub warm_check_ms: u64,
    /// How long the baseline test run took, for the test check.
    pub baseline_test_ms: u64,
}

impl CheckReport {
    /// Builds a report from its cases, filling in the summaries.
    pub fn new(project: &str, check: &str, cases: Vec<CaseResult>) -> CheckReport {
        let kinds = summarize_kinds(&cases);
        let totals = Summary::of("all", &cases.iter().collect::<Vec<&CaseResult>>());
        let scored = cases.iter().filter(|case| case.scored()).count() as u64;
        let blastr: u64 = cases.iter().map(|case| case.blastr_ms).sum();
        let cargo: u64 = cases.iter().map(|case| case.cargo_ms).sum();
        CheckReport {
            project: project.to_owned(),
            check: check.to_owned(),
            revision: String::new(),
            blastr_version: String::new(),
            blastr_build: String::new(),
            rust_analyzer: String::new(),
            params: AnalysisParams::default(),
            crates: 0,
            symbols: 0,
            edges: 0,
            unplanned: 0,
            kinds,
            totals,
            cases,
            planned: 0,
            sampled: None,
            seed: None,
            base_load_ms: 0,
            mean_blastr_ms: blastr.checked_div(scored).unwrap_or(0),
            mean_cargo_ms: cargo.checked_div(scored).unwrap_or(0),
            warm_check_ms: 0,
            baseline_test_ms: 0,
        }
    }
}

/// The per-kind summaries of `cases`, in alphabetical order of the kind.
pub fn summarize_kinds(cases: &[CaseResult]) -> Vec<Summary> {
    let mut grouped: BTreeMap<&str, Vec<&CaseResult>> = BTreeMap::new();
    for case in cases {
        grouped.entry(case.kind.as_str()).or_default().push(case);
    }
    grouped
        .into_iter()
        .map(|(kind, cases)| Summary::of(kind, &cases))
        .collect()
}

/// Reads a report from a JSON file.
pub fn read(path: &std::path::Path) -> Result<CheckReport> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("`{}` cannot be read: {error}", path.display()))?;
    let report = serde_json::from_str(&text)
        .map_err(|error| format!("`{}` is not a report: {error}", path.display()))?;
    Ok(report)
}

/// Writes a report as JSON.
pub fn write(path: &std::path::Path, report: &CheckReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(report)
        .map_err(|error| format!("the report could not be written: {error}"))?;
    std::fs::write(path, format!("{text}\n"))?;
    Ok(())
}

/// The Markdown summary of one run: the numbers, then every miss.
pub fn markdown(reports: &[CheckReport]) -> String {
    let mut out = String::new();
    let first = reports.first();
    let version = first
        .map(|report| report.blastr_version.clone())
        .unwrap_or_default();
    let build = first
        .map(|report| report.blastr_build.clone())
        .unwrap_or_default();
    let analyzer = first
        .map(|report| report.rust_analyzer.clone())
        .unwrap_or_default();
    let _ = writeln!(out, "# Blastr validation report\n");
    let _ = writeln!(
        out,
        "Blastr {version} (build `{build}`), rust-analyzer {analyzer}."
    );
    if let Some(report) = first {
        let params = &report.params;
        let _ = writeln!(
            out,
            "Parameters: depth {}, decay {}, candidate weight {}, adaptation weight {}, behaviour weight {}, score floor {}, minimum confidence {}, ignore {:?}.\n",
            params.depth,
            params.ranking.decay,
            params.ranking.candidate_weight,
            params.ranking.adaptation_weight,
            params.ranking.behaviour_weight,
            params.ranking.score_floor,
            params.min_confidence,
            params.ignore
        );
    }
    for report in reports {
        if let Some(sampled) = report.sampled {
            let _ = writeln!(
                out,
                "`{}` {}: the first {sampled} cases of {} planned, in plan order (seed {:?}).\n",
                report.project, report.check, report.planned, report.seed
            );
        }
        if report.unplanned > 0 {
            let _ = writeln!(
                out,
                "`{}` {}: {} edit(s) could not be planned (no syntax of their own, or the edit does not apply).\n",
                report.project, report.check, report.unplanned
            );
        }
    }

    let compiler: Vec<&CheckReport> = reports
        .iter()
        .filter(|report| report.check == "compiler")
        .collect();
    let tests: Vec<&CheckReport> = reports
        .iter()
        .filter(|report| report.check == "tests")
        .collect();

    if !compiler.is_empty() {
        let _ = writeln!(out, "## Compiler check: *needs editing* items\n");
        let _ = writeln!(
            out,
            "Every case applies one seeded edit to one definition, runs Blastr on the original and the edited project, and runs `cargo check --all-targets` on the edited copy. The symbols with compiler errors (and the changed symbol itself) are what Blastr should have listed as *needs editing*.\n"
        );
        let _ = writeln!(
            out,
            "| project | revision | crates | symbols | cases | excluded | misses | false alarms | expected | listed | unconfirmed | blastr items | reachable | listed / reachable |"
        );
        let _ = writeln!(
            out,
            "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
        );
        for report in &compiler {
            let totals = &report.totals;
            let _ = writeln!(
                out,
                "| {} | `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.0}% |",
                report.project,
                report.revision,
                report.crates,
                report.symbols,
                totals.cases,
                totals.excluded,
                totals.misses,
                totals.false_alarms,
                totals.expected,
                totals.listed,
                totals.unconfirmed,
                totals.blastr_items,
                totals.reachable,
                totals.listed_ratio_percent()
            );
        }
        let _ = writeln!(out, "\n### Per change kind\n");
        let _ = writeln!(
            out,
            "| project | kind | cases | excluded | misses | false alarms | expected | listed | unconfirmed | blastr items | reachable | listed / reachable |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|---|");
        for report in &compiler {
            for summary in &report.kinds {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.0}% |",
                    report.project,
                    summary.kind,
                    summary.cases,
                    summary.excluded,
                    summary.misses,
                    summary.false_alarms,
                    summary.expected,
                    summary.listed,
                    summary.unconfirmed,
                    summary.blastr_items,
                    summary.reachable,
                    summary.listed_ratio_percent()
                );
            }
        }
    }

    if !tests.is_empty() {
        let _ = writeln!(
            out,
            "\n## Test check: *may behave differently* items and test selection\n"
        );
        let _ = writeln!(
            out,
            "Every case seeds one mutation into one function body, keeps it only if it compiles, and runs `cargo test --workspace --no-fail-fast`. Every test that then fails must be among the tests Blastr selects for the same change.\n"
        );
        let _ = writeln!(
            out,
            "| project | revision | mutants | excluded | misses | selected | suite | selection |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
        for report in &tests {
            let totals = &report.totals;
            let _ = writeln!(
                out,
                "| {} | `{}` | {} | {} | {} | {} | {} | {:.0}% |",
                report.project,
                report.revision,
                totals.cases,
                totals.excluded,
                totals.misses,
                totals.suite_selected,
                totals.suite_total,
                totals.selection_percent()
            );
        }
        let _ = writeln!(out, "\n### Per mutation kind\n");
        let _ = writeln!(
            out,
            "| project | kind | cases | excluded | misses | selected | suite | selection |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
        for report in &tests {
            for summary in &report.kinds {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} | {} | {} | {:.0}% |",
                    report.project,
                    summary.kind,
                    summary.cases,
                    summary.excluded,
                    summary.misses,
                    summary.suite_selected,
                    summary.suite_total,
                    summary.selection_percent()
                );
            }
        }
    }

    let _ = writeln!(out, "\n## Performance\n");
    let _ = writeln!(
        out,
        "| project | check | load | blastr per case | cargo per case | warm check | baseline tests |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|");
    for report in reports {
        let _ = writeln!(
            out,
            "| {} | {} | {} ms | {} ms | {} ms | {} ms | {} ms |",
            report.project,
            report.check,
            report.base_load_ms,
            report.mean_blastr_ms,
            report.mean_cargo_ms,
            report.warm_check_ms,
            report.baseline_test_ms
        );
    }

    let misses: Vec<(&CheckReport, &CaseResult)> = reports
        .iter()
        .flat_map(|report| {
            report
                .cases
                .iter()
                .filter(|case| !case.misses.is_empty())
                .map(move |case| (report, case))
        })
        .collect();
    let _ = writeln!(out, "\n## Misses\n");
    if misses.is_empty() {
        let _ = writeln!(out, "None in these runs.");
    }
    for (report, case) in &misses {
        let _ = writeln!(
            out,
            "- `{}` / `{}` / {} — missed {}",
            report.project,
            case.id,
            case.symbol,
            case.misses.join(", ")
        );
    }

    let exclusions: BTreeMap<(String, &str), u32> = {
        let mut counts = BTreeMap::new();
        for report in reports {
            for case in &report.cases {
                if let Some(reason) = &case.excluded {
                    *counts
                        .entry((report.check.clone(), reason.as_str()))
                        .or_insert(0) += 1;
                }
            }
        }
        counts
    };
    let _ = writeln!(out, "\n## Exclusions\n");
    if exclusions.is_empty() {
        let _ = writeln!(out, "None.");
    }
    for ((check, reason), count) in exclusions {
        let _ = writeln!(out, "- {reason} ({check} check): {count}");
    }

    let _ = writeln!(out, "\n## How to read this\n");
    let _ = writeln!(
        out,
        "A **case** is one edited definition. It is *scored* unless the compiler or Cargo could not judge it; the reason is counted under Exclusions. **expected** counts the symbols the compiler reported errors in (or the tests that failed), **listed** counts what Blastr listed as needing editing (or the tests it selected) for the same edit. **unconfirmed** counts items the compiler could not judge: those in a crate it did not build, and those in a target where it stopped after the first privacy error. **blastr items** counts every item of the result, of both classes, and **reachable** what plain reverse reachability from the change returns; the last column is the first as a share of the second, so 100% means Blastr lists as much as a reference search, less means it is more selective."
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scored case with the given expected and listed entries.
    fn case(
        kind: &str,
        expected: &[&str],
        listed: &[&str],
        misses: &[&str],
        alarms: &[&str],
    ) -> CaseResult {
        CaseResult {
            id: format!("{kind}-1"),
            kind: kind.to_owned(),
            symbol: "krate::f".to_owned(),
            file: "src/lib.rs".to_owned(),
            before: String::new(),
            after: String::new(),
            excluded: None,
            expected: expected.iter().map(|entry| (*entry).to_owned()).collect(),
            listed: listed.iter().map(|entry| (*entry).to_owned()).collect(),
            misses: misses.iter().map(|entry| (*entry).to_owned()).collect(),
            false_alarms: alarms.iter().map(|entry| (*entry).to_owned()).collect(),
            reachable: 10,
            blastr_items: 4,
            unconfirmed: 0,
            suite_total: 20,
            suite_selected_percent: 0.0,
            blastr_ms: 100,
            cargo_ms: 200,
            notes: Vec::new(),
        }
    }

    #[test]
    fn summaries_count_misses_and_alarms_of_scored_cases_only() {
        let mut excluded = case("add-field", &["a"], &[], &[], &[]);
        excluded.excluded = Some("crate-not-checked".to_owned());
        let cases = [
            case("add-field", &["a", "b"], &["a"], &["b"], &[]),
            case("remove-field", &[], &["c"], &[], &["c"]),
            excluded,
        ];
        let summary = Summary::of("all", &cases.iter().collect::<Vec<&CaseResult>>());
        assert_eq!(summary.cases, 3);
        assert_eq!(summary.excluded, 1);
        assert_eq!(summary.misses, 1);
        assert_eq!(summary.false_alarms, 1);
        // The excluded case contributes nothing but its count.
        assert_eq!(summary.reachable, 20);
        assert_eq!(summary.blastr_items, 8);
        assert_eq!(summary.suite_total, 40);
    }

    #[test]
    fn kinds_are_summarized_in_alphabetical_order() {
        let cases = vec![
            case("remove-field", &[], &[], &[], &[]),
            case("add-field", &[], &[], &[], &[]),
            case("add-field", &[], &[], &[], &[]),
        ];
        let kinds = summarize_kinds(&cases);
        assert_eq!(kinds[0].kind, "add-field");
        assert_eq!(kinds[0].cases, 2);
        assert_eq!(kinds[1].kind, "remove-field");
        assert_eq!(kinds[1].cases, 1);
    }

    #[test]
    fn the_list_is_measured_against_plain_reachability() {
        let summary = Summary {
            reachable: 10,
            blastr_items: 4,
            unconfirmed: 0,
            ..Summary::default()
        };
        assert!((summary.listed_ratio_percent() - 40.0).abs() < 0.01);
        assert_eq!(Summary::default().listed_ratio_percent(), 0.0);
    }

    #[test]
    fn the_selection_share_is_measured_against_the_suite() {
        let summary = Summary {
            suite_selected: 5,
            suite_total: 20,
            ..Summary::default()
        };
        assert!((summary.selection_percent() - 25.0).abs() < 0.01);
        assert_eq!(Summary::default().selection_percent(), 0.0);
    }

    #[test]
    fn a_report_sums_up_its_cases_and_means() {
        let cases = vec![
            case("add-field", &["a"], &["a"], &[], &[]),
            case("add-field", &["a"], &[], &["a"], &[]),
        ];
        let report = CheckReport::new("tiny", "compiler", cases);
        assert_eq!(report.totals.cases, 2);
        assert_eq!(report.totals.misses, 1);
        assert_eq!(report.kinds.len(), 1);
        assert_eq!(report.mean_blastr_ms, 100);
        assert_eq!(report.mean_cargo_ms, 200);
    }

    #[test]
    fn the_markdown_lists_the_misses() {
        let cases = vec![case("add-field", &["a", "b"], &["a"], &["krate::b"], &[])];
        let mut report = CheckReport::new("tiny", "compiler", cases);
        report.blastr_version = "0.1.1".to_owned();
        let text = markdown(&[report]);
        assert!(text.contains("Blastr 0.1.1"));
        assert!(text.contains("## Compiler check"));
        assert!(text.contains("missed krate::b"));
        assert!(text.contains("None."));
    }

    #[test]
    fn a_report_round_trips_through_json() {
        let cases = vec![case("add-field", &["a"], &["a"], &[], &[])];
        let report = CheckReport::new("tiny", "compiler", cases);
        let temp = tempfile::tempdir().expect("a temporary directory");
        let path = temp.path().join("report.json");
        write(&path, &report).expect("the report is written");
        let read_back = read(&path).expect("the report is read");
        assert_eq!(read_back.totals, report.totals);
        assert_eq!(read_back.cases.len(), 1);
    }
}
