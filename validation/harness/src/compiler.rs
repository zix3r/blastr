//! The compiler check: does Blastr list the definitions the compiler rejects?
//!
//! For one seeded edit, the answer has three parts. The compiler compiles the
//! edited project and reports the spans it rejects; Blastr analyses the same
//! edited project and lists the items it thinks need editing. The comparison is
//! per symbol: an item the compiler rejects but Blastr does not list is a
//! **miss**, an item Blastr lists that the compiler accepts is a **false
//! alarm**. The changed definition itself is never counted either way: it is
//! allowed to break inside its own body, which is not what the list is for.
//!
//! A case is scored only when the compiler could have said something about
//! every relevant item: when it stopped at name resolution, when a crate was
//! not built because a dependency failed, when the run timed out, or when Blastr
//! itself failed, the case is excluded and counted under its reason.

use std::collections::BTreeSet;

use blastr_core::{FileEdit, build_id};
use blastr_model::{AnalysisParams, ImpactClass, RevisionGraph, SymbolId};

use crate::cargo::{self, CheckOutput, Diagnostic};
use crate::edits::EditKind;
use crate::map::{self, Placement, SpanIndex};
use crate::plan::{self, PlannedCase};
use crate::project::{CHECK_TIMEOUT, Project};
use crate::results::{CaseResult, CheckReport};
use crate::{Error, Result, reach};

/// The options one check runs with.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// How many cases to sample.
    pub limit: Option<usize>,
    /// Seed of the sample; `None` takes the plan order.
    pub seed: Option<u64>,
    /// Which edit kinds to plan.
    pub kinds: Vec<EditKind>,
    /// Rerun one case by id and print its evidence.
    pub case: Option<String>,
}

/// Runs the compiler check over the project.
pub fn run(project: &mut Project, options: &Options) -> Result<CheckReport> {
    let params = AnalysisParams::default();
    let kinds = if options.kinds.is_empty() {
        crate::edits::EDIT_KINDS.to_vec()
    } else {
        options.kinds.clone()
    };
    let (planned, unplanned) = plan::compiler_cases(project.graph(), project.dir(), &kinds)?;
    let available = planned.len() as u32;
    let planned = plan::sample(planned, options.limit, options.seed);
    let planned = crate::plan::only(planned, options.case.as_deref())?;

    println!(
        "{}: {} compiler cases planned{}",
        project_name(project),
        planned.len(),
        match options.limit {
            Some(limit) => format!(" (limit {limit}, seed {:?})", options.seed),
            None => String::new(),
        }
    );

    // One cold and one warm check of the unchanged project: the first pays for
    // the dependency build, the second is what a case costs on its own.
    let warm = warm_up(project)?;

    let mut cases = Vec::new();
    for case in &planned {
        let (result, evidence) = run_case(project, case, &params, options.case.is_some())?;
        if options.case.is_some() {
            print_evidence(&result, &evidence);
        } else {
            println!(
                "{} {:>3} {} {:?} ({} expected, {} listed, blastr {} ms, cargo {} ms)",
                result.id,
                result.excluded.as_deref().unwrap_or(""),
                if result.misses.is_empty() && result.false_alarms.is_empty() {
                    "ok "
                } else {
                    "!! "
                },
                result.symbol,
                result.expected.len(),
                result.listed.len(),
                result.blastr_ms,
                result.cargo_ms
            );
        }
        cases.push(result);
    }

    let mut report = CheckReport::new(&project_name(project), "compiler", cases);
    report.revision = project.revision().to_owned();
    report.blastr_version = blastr_core::VERSION.to_owned();
    report.blastr_build = build_id().to_owned();
    report.rust_analyzer = blastr_core::RUST_ANALYZER_VERSION.to_owned();
    report.params = params;
    report.crates = project.graph().crates.len() as u32;
    report.symbols = project.graph().symbols.len() as u32;
    report.edges = project.graph().edges.len() as u32;
    report.unplanned = unplanned;
    report.planned = available;
    report.warm_check_ms = warm;
    report.base_load_ms = project.load_ms();
    report.sampled = options.limit.map(|limit| limit as u32);
    report.seed = options.seed;
    Ok(report)
}

/// The directory name of the project, for reports.
pub fn project_name(project: &Project) -> String {
    project
        .dir()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| project.dir().display().to_string())
}

/// One check of the unchanged copy, after one that pays for the dependencies.
fn warm_up(project: &Project) -> Result<u64> {
    let (_, cold) = cargo::check(project.copy(), project.target_dir(), CHECK_TIMEOUT)?;
    if cold.timed_out {
        return Err(Error::Harness(
            "the first `cargo check` of the project timed out".to_owned(),
        ));
    }
    if !cold.success {
        return Err(Error::Harness(format!(
            "the unmodified project does not compile: {}",
            first_line(&cold.stderr)
        )));
    }
    let (_, warm) = cargo::check(project.copy(), project.target_dir(), CHECK_TIMEOUT)?;
    println!(
        "cargo check: {} ms cold, {} ms warm",
        cold.millis, warm.millis
    );
    Ok(warm.millis)
}

/// The first non-empty line of a text.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// Runs one case: Blastr's analysis, the compiler's verdict, and the comparison.
fn run_case(
    project: &mut Project,
    case: &PlannedCase,
    params: &AnalysisParams,
    wants_evidence: bool,
) -> Result<(CaseResult, Vec<String>)> {
    let mut evidence: Vec<String> = Vec::new();
    let mut result = CaseResult {
        id: case.id.clone(),
        kind: case.kind.as_str().to_owned(),
        symbol: case.symbol.clone(),
        file: case.file.as_str().to_owned(),
        before: case.edit.before.clone(),
        after: case.edit.after.clone(),
        excluded: None,
        expected: Vec::new(),
        listed: Vec::new(),
        misses: Vec::new(),
        false_alarms: Vec::new(),
        reachable: 0,
        blastr_items: 0,
        unconfirmed: 0,
        suite_total: 0,
        suite_selected_percent: 0.0,
        blastr_ms: 0,
        cargo_ms: 0,
        notes: Vec::new(),
    };

    // Blastr first: it reads the edit from memory and never touches the copy.
    let edit = FileEdit {
        path: case.file.clone(),
        content: Some(case.text.clone()),
    };
    let variant = match project.analyze(&[edit], params) {
        Ok((variant, millis)) => {
            result.blastr_ms = millis;
            if wants_evidence {
                evidence = evidence_lines(&variant.analysis);
            }
            variant
        }
        Err(error) => {
            result.excluded = Some("blastr-failed".to_owned());
            result.notes.push(format!("{error}"));
            return Ok((result, evidence));
        }
    };

    // The same text, this time on disk in the copy, for the compiler.
    let original = project.write_copy(&case.file, &case.text)?;
    let checked = cargo::check(project.copy(), project.target_dir(), CHECK_TIMEOUT);
    project.restore_copy(&case.file, &original)?;
    let (check, run) = checked?;
    result.cargo_ms = run.millis;

    if run.timed_out {
        result.excluded = Some("timeout".to_owned());
        return Ok((result, evidence));
    }
    if !run.success && check.errors.is_empty() && check.spanless.is_empty() {
        result.excluded = Some("cargo-failed".to_owned());
        result.notes.push(first_line(&run.stderr));
        return Ok((result, evidence));
    }

    // The graph of the edited revision is what the errors are mapped onto: it
    // describes exactly the text the compiler read.
    let span_index = SpanIndex::new(&variant.head);
    let mut expected: BTreeSet<String> = BTreeSet::new();
    let mut outside = 0_u32;
    let mut unmapped = 0_u32;
    let mut relevant_crates: BTreeSet<String> = BTreeSet::new();
    for error in &check.errors {
        match place(&span_index, project.copy(), error) {
            Placement::Symbol(path) => {
                expected.insert(path.path.clone());
                note_crate(&variant.head, &path.path, &mut relevant_crates);
            }
            Placement::FileOnly(file) => {
                unmapped = unmapped.saturating_add(1);
                result.notes.push(format!(
                    "{}:{} has no enclosing symbol",
                    file.as_str(),
                    error.line
                ));
            }
            Placement::Outside => outside = outside.saturating_add(1),
        }
    }

    let codes: Vec<String> = cargo::error_codes(&check.errors).into_keys().collect();
    if cargo::stopped_at_resolution(&codes) {
        result.excluded = Some("stopped-at-name-resolution".to_owned());
        return Ok((result, evidence));
    }

    // The changed definition is excused on both sides.
    expected.remove(&case.symbol);
    let listed: BTreeSet<String> = variant
        .analysis
        .items
        .iter()
        .filter(|item| item.class == ImpactClass::Adaptation)
        .map(|item| item.symbol.path.clone())
        .filter(|path| path != &case.symbol)
        .collect();

    // A crate the compiler never built cannot confirm or deny its items: a
    // crate whose dependency failed to compile is not built at all, so the
    // items it holds are neither confirmed nor refuted. Those items are left
    // out of both sides and counted; a case whose items are all in such crates
    // is not scored at all.
    note_crate(&variant.head, &case.symbol, &mut relevant_crates);
    let built = built_packages(&check, &run.stderr);
    let checked = |path: &String| {
        let mut packages = BTreeSet::new();
        note_crate(&variant.head, path, &mut packages);
        packages.is_empty() || packages.iter().any(|package| built.contains(package))
    };
    let expected_checked: BTreeSet<String> = expected
        .iter()
        .filter(|path| checked(path))
        .cloned()
        .collect();
    let listed_checked: BTreeSet<String> = listed
        .iter()
        .filter(|path| checked(path))
        .cloned()
        .collect();
    result.unconfirmed = u32::try_from(
        (expected.len() - expected_checked.len()) + (listed.len() - listed_checked.len()),
    )
    .unwrap_or(u32::MAX);

    let confirmed_crates: Vec<&String> = relevant_crates
        .iter()
        .filter(|package| built.contains(*package))
        .collect();
    if confirmed_crates.is_empty() && !relevant_crates.is_empty() {
        result.excluded = Some("crate-not-checked".to_owned());
        result.notes.push(format!(
            "the compiler did not build {}",
            relevant_crates
                .iter()
                .map(|package| package.as_str())
                .collect::<Vec<&str>>()
                .join(", ")
        ));
        return Ok((result, evidence));
    }
    if result.unconfirmed > 0 {
        let missing: Vec<&String> = relevant_crates
            .iter()
            .filter(|package| !built.contains(*package))
            .collect();
        result.notes.push(format!(
            "{} item(s) in crates the compiler did not build ({})",
            result.unconfirmed,
            missing
                .iter()
                .map(|package| package.as_str())
                .collect::<Vec<&str>>()
                .join(", ")
        ));
    }

    result.misses = expected_checked
        .difference(&listed_checked)
        .cloned()
        .collect();
    result.false_alarms = listed_checked
        .difference(&expected_checked)
        .cloned()
        .collect();
    result.expected = expected_checked.into_iter().collect();
    result.listed = listed_checked.into_iter().collect();
    result.blastr_items = variant.analysis.items.len() as u32;
    result.reachable = reachable(project.graph(), &variant.head, case);

    if outside > 0 {
        result
            .notes
            .push(format!("{outside} error(s) outside the project"));
    }
    if unmapped > 0 {
        result
            .notes
            .push(format!("{unmapped} error(s) in no symbol"));
    }
    if !check.errors.is_empty() {
        let histogram: Vec<String> = cargo::error_codes(&check.errors)
            .iter()
            .map(|(code, count)| format!("{code} x{count}"))
            .collect();
        result.notes.push(histogram.join(", "));
    }
    Ok((result, evidence))
}

/// Places one diagnostic by the span index of the edited graph.
fn place(index: &SpanIndex, copy: &std::path::Path, error: &Diagnostic) -> Placement {
    let Some(file) = &error.file else {
        return Placement::Outside;
    };
    map::place(index, copy, file, error.start, error.end)
}

/// Adds the package a symbol belongs to, when the graph knows it.
fn note_crate(graph: &RevisionGraph, path: &str, into: &mut BTreeSet<String>) {
    for symbol in &graph.symbols {
        if symbol.path.path != path {
            continue;
        }
        if let Some(info) = graph.crates.get(symbol.krate.0 as usize) {
            into.insert(info.package.clone());
        }
    }
}

/// The packages the compiler built something for, over every target kind but
/// build scripts.
///
/// A unit that failed to compile produces no artifact message at all, so the
/// progress lines on standard error are read as well: they name every unit
/// Cargo checked, built or found fresh.
fn built_packages(check: &CheckOutput, stderr: &str) -> BTreeSet<String> {
    let mut packages = cargo::progress_packages(stderr);
    packages.extend(
        check
            .artifacts
            .iter()
            .filter(|artifact| artifact.kind != "custom-build")
            .map(|artifact| artifact.package.clone()),
    );
    packages
}

/// The number of symbols that depend on the changed one, by plain reverse
/// reachability. The head graph answers for everything that still exists; a
/// removed symbol is looked up on the base graph, as the spec's rule does.
fn reachable(base: &RevisionGraph, head: &RevisionGraph, case: &PlannedCase) -> u32 {
    if let Some(seed) = seed_in(head, &case.symbol, &case.file, case.span_start) {
        return u32::try_from(reach::dependents(head, &[seed]).len()).unwrap_or(u32::MAX);
    }
    let Some(seed) = seed_in(base, &case.symbol, &case.file, case.span_start) else {
        return 0;
    };
    u32::try_from(reach::dependents(base, &[seed]).len()).unwrap_or(u32::MAX)
}

/// The symbol of `graph` with that path in that file, preferring the one whose
/// span starts at `start`.
pub fn seed_in(
    graph: &RevisionGraph,
    path: &str,
    file: &blastr_model::RelPath,
    start: u32,
) -> Option<SymbolId> {
    let mut fallback = None;
    for (index, symbol) in graph.symbols.iter().enumerate() {
        if symbol.path.path != path || &symbol.location.file != file {
            continue;
        }
        if symbol.location.span.start == start {
            return Some(SymbolId(index as u32));
        }
        fallback.get_or_insert(SymbolId(index as u32));
    }
    fallback
}

/// Prints what Blastr knew about one case, for `--case`.
/// Prints what Blastr knew about one case, for `--case`.
pub(crate) fn print_evidence(result: &CaseResult, evidence: &[String]) {
    println!("\ncase {}", result.id);
    println!("  kind     {}", result.kind);
    println!("  symbol   {}", result.symbol);
    println!("  file     {}", result.file);
    if let Some(reason) = &result.excluded {
        println!("  excluded {reason}");
    }
    for note in &result.notes {
        println!("  note     {note}");
    }
    println!("  before\n{}", indent(&result.before));
    println!("  after\n{}", indent(&result.after));
    println!("  what Blastr said");
    for line in evidence {
        println!("    {line}");
    }
    if evidence.is_empty() {
        println!("    (nothing)");
    }
    println!(
        "  expected {} and listed {}: {} miss(es), {} false alarm(s), {} unconfirmed",
        result.expected.len(),
        result.listed.len(),
        result.misses.len(),
        result.false_alarms.len(),
        result.unconfirmed
    );
    for entry in &result.expected {
        let mark = if result.misses.contains(entry) {
            "MISS"
        } else {
            "ok"
        };
        println!("    {mark} {entry}");
    }
    for entry in &result.listed {
        if result.false_alarms.contains(entry) {
            println!("    FALSE ALARM {entry}");
        }
    }
    println!(
        "  reachable (plain reverse reachability) {}, listed by Blastr {}",
        result.reachable, result.blastr_items
    );
}

/// One line per change and per item of an analysis: what Blastr reported and
/// the evidence path that reached the item.
pub(crate) fn evidence_lines(analysis: &blastr_model::Analysis) -> Vec<String> {
    let mut lines = Vec::new();
    for change in &analysis.diff.changes {
        lines.push(format!(
            "change {} {}{}",
            change.kind,
            change.symbol.path,
            match &change.detail {
                blastr_model::ChangeDetail::None => String::new(),
                other => format!(" ({other:?})"),
            }
        ));
    }
    for item in &analysis.items {
        lines.push(format!(
            "{} {} {} d={} score={:.2}{}",
            item.class,
            item.confidence,
            item.symbol.path,
            item.distance,
            item.score,
            if item.tags.changed {
                " (adapted in this diff)"
            } else {
                ""
            }
        ));
        if let Some(path) = item.paths.first() {
            let steps: Vec<String> = path
                .steps
                .iter()
                .map(|step| {
                    format!(
                        "{} -[{}]-> {} ({}:{})",
                        step.from.path,
                        step.edge,
                        step.to.path,
                        step.site.file,
                        step.site.span.start_line
                    )
                })
                .collect();
            lines.push(format!(
                "    {:?} via {} (certainty {}): {}",
                path.reason,
                if path.steps.len() == 1 {
                    "one step"
                } else {
                    "a chain"
                },
                path.steps
                    .first()
                    .map(|step| step.certainty.to_string())
                    .unwrap_or_default(),
                steps.join(", then ")
            ));
        }
    }
    lines
}

/// Indents a snippet for printing.
pub(crate) fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    | {line}"))
        .collect::<Vec<String>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_crate_is_reported_not_scored() {
        let check = CheckOutput {
            artifacts: vec![cargo::Artifact {
                package: "tinycore".to_owned(),
                kind: "lib".to_owned(),
                target: "tinycore".to_owned(),
            }],
            ..CheckOutput::default()
        };
        let stderr = "   Checking tinyapp v0.1.0 (C:\\tmp)\\n    Fresh tinycore v0.1.0";
        let built = built_packages(&check, stderr);
        assert!(built.contains("tinycore"));
        assert!(built.contains("tinyapp"));
        assert!(!built.contains("tinyother"));
    }

    #[test]
    fn progress_lines_name_the_units_cargo_checked() {
        let stderr = concat!(
            "   Compiling tinycore v0.1.0 (/tmp/copy/geometry)\n",
            "    Building [===>     ] 1/2: tinycore\n",
            "error: could not compile `tinycore` (lib)\n",
            "warning: build failed, waiting for other jobs to finish...\n",
        );
        let packages = cargo::progress_packages(stderr);
        assert_eq!(packages.len(), 1);
        assert!(packages.contains("tinycore"));
    }

    #[test]
    fn the_first_line_skips_the_noise() {
        assert_eq!(first_line("\n\n  error: boom\nmore"), "error: boom");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn snippets_are_indented_for_printing() {
        assert_eq!(indent("a\nb"), "    | a\n    | b");
    }
}
