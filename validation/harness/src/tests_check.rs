//! The test check: does Blastr select the tests a mutation makes fail?
//!
//! One mutation per function, the first the mutators can find. A mutant that
//! does not compile is discarded: it proves nothing about which tests a change
//! reaches. A mutant that compiles is run with `cargo test --workspace
//! --no-fail-fast`, and every test that then fails must be among the tests
//! Blastr selects for the same edit. A failing test Blastr did not select is a
//! **miss**; the share of the whole suite the selection covers is recorded next
//! to it.

use std::collections::BTreeSet;

use blastr_core::{FileEdit, build_id};
use blastr_model::AnalysisParams;

use crate::cargo;
use crate::compiler;
use crate::edits::parses;
use crate::plan::{self, PlannedMutant};
use crate::project::{CHECK_TIMEOUT, Project, TEST_TIMEOUT};
use crate::results::{CaseResult, CheckReport};
use crate::{Error, Result, compiler::Options, reach};

/// Runs the test check over the project.
pub fn run(project: &mut Project, options: &Options) -> Result<CheckReport> {
    let params = AnalysisParams::default();
    let (planned, unplanned) = plan::mutant_cases(project.graph(), project.dir())?;
    let available = planned.len() as u32;
    let planned = plan::sample_mutants(planned, options.limit, options.seed);
    let planned = plan::only(planned, options.case.as_deref())?;

    println!(
        "{}: {} mutants planned{}",
        compiler::project_name(project),
        planned.len(),
        match options.limit {
            Some(limit) => format!(" (limit {limit}, seed {:?})", options.seed),
            None => String::new(),
        }
    );

    let baseline = baseline(project)?;
    println!(
        "baseline: {} tests pass twice in a row ({} ms)",
        baseline.total, baseline.millis
    );

    let mut cases = Vec::new();
    for mutant in &planned {
        let (result, evidence) =
            run_case(project, mutant, &params, &baseline, options.case.is_some())?;
        if options.case.is_some() {
            compiler::print_evidence(&result, &evidence);
        } else {
            println!(
                "{} {:>20} {:?} ({} failing, {} selected, blastr {} ms, cargo {} ms)",
                result.id,
                result.excluded.as_deref().unwrap_or(""),
                result.symbol,
                result.expected.len(),
                result.listed.len(),
                result.blastr_ms,
                result.cargo_ms
            );
        }
        cases.push(result);
    }

    let mut report = CheckReport::new(&compiler::project_name(project), "tests", cases);
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
    report.baseline_test_ms = baseline.millis;
    report.base_load_ms = project.load_ms();
    report.sampled = options.limit.map(|limit| limit as u32);
    report.seed = options.seed;
    Ok(report)
}

/// What the unmodified project's test suite looks like.
struct Baseline {
    /// How many tests ran.
    total: u32,
    /// How long the second run took.
    millis: u64,
}

/// Runs the whole suite twice on the unchanged copy: a suite that fails, or
/// that fails only sometimes, cannot be the ground truth of a mutation.
fn baseline(project: &Project) -> Result<Baseline> {
    let mut millis = 0;
    let mut total = 0;
    for attempt in 1..=2 {
        let (output, run) = cargo::test(project.copy(), project.target_dir(), TEST_TIMEOUT)?;
        if run.timed_out {
            return Err(Error::Harness(format!(
                "the baseline test run {attempt} timed out"
            )));
        }
        if !run.success || !output.failed.is_empty() {
            return Err(Error::Harness(format!(
                "the unmodified project's tests do not pass (run {attempt}): {}",
                output.failed.join(", ")
            )));
        }
        millis = run.millis;
        total = output.total;
    }
    Ok(Baseline { total, millis })
}

/// Runs one mutant: does it compile, which tests does it fail, and did Blastr
/// select them?
fn run_case(
    project: &mut Project,
    mutant: &PlannedMutant,
    params: &AnalysisParams,
    baseline: &Baseline,
    wants_evidence: bool,
) -> Result<(CaseResult, Vec<String>)> {
    let mut evidence: Vec<String> = Vec::new();
    let mut result = CaseResult {
        id: mutant.id.clone(),
        kind: mutant.kind.as_str().to_owned(),
        symbol: mutant.symbol.clone(),
        file: mutant.file.as_str().to_owned(),
        before: mutant.edit.before.clone(),
        after: mutant.edit.after.clone(),
        excluded: None,
        expected: Vec::new(),
        listed: Vec::new(),
        misses: Vec::new(),
        false_alarms: Vec::new(),
        reachable: 0,
        blastr_items: 0,
        unconfirmed: 0,
        suite_total: baseline.total,
        suite_selected_percent: 0.0,
        blastr_ms: 0,
        cargo_ms: 0,
        notes: Vec::new(),
    };
    if !parses(&mutant.text) {
        result.excluded = Some("does-not-parse".to_owned());
        return Ok((result, evidence));
    }

    // Blastr first: the mutant is applied in memory, the copy is written after.
    let edit = FileEdit {
        path: mutant.file.clone(),
        content: Some(mutant.text.clone()),
    };
    let variant = match project.analyze(&[edit], params) {
        Ok((variant, millis)) => {
            result.blastr_ms = millis;
            if wants_evidence {
                evidence = compiler::evidence_lines(&variant.analysis);
                for test in &variant.analysis.tests.selected {
                    evidence.push(format!(
                        "selects {} ({:?}) {}",
                        test.filter, test.reason, test.symbol.path
                    ));
                }
            }
            variant
        }
        Err(error) => {
            result.excluded = Some("blastr-failed".to_owned());
            result.notes.push(format!("{error}"));
            return Ok((result, evidence));
        }
    };

    let original = project.write_copy(&mutant.file, &mutant.text)?;
    let checked = cargo::check(project.copy(), project.target_dir(), CHECK_TIMEOUT);
    let compiles = match &checked {
        Ok((check, _)) => check.errors.is_empty(),
        Err(_) => false,
    };
    if !compiles {
        project.restore_copy(&mutant.file, &original)?;
        // A mutant that does not compile cannot say anything about behaviour.
        result.excluded = Some("does-not-compile".to_owned());
        return Ok((result, evidence));
    }

    let tested = cargo::test(project.copy(), project.target_dir(), TEST_TIMEOUT);
    project.restore_copy(&mutant.file, &original)?;
    let (output, run) = tested?;
    result.cargo_ms = run.millis;

    if run.timed_out {
        result.excluded = Some("timeout".to_owned());
        return Ok((result, evidence));
    }
    if !output.failed_doctests.is_empty() && output.failed.is_empty() {
        // Version 1 does not analyse doc-tests, and the spec records that as a
        // limit rather than as an accuracy question.
        result.excluded = Some("failed-only-in-doc-tests".to_owned());
        result.notes.push(output.failed_doctests.join(", "));
        return Ok((result, evidence));
    }
    if !output.failed_doctests.is_empty() {
        result.notes.push(format!(
            "{} doc-test(s) also failed: {}",
            output.failed_doctests.len(),
            output.failed_doctests.join(", ")
        ));
    }

    let selected: Vec<String> = variant
        .analysis
        .tests
        .selected
        .iter()
        .map(|test| test.filter.clone())
        .collect();
    let selected_set: BTreeSet<String> = selected.iter().cloned().collect();
    let failing: BTreeSet<String> = output.failed.iter().cloned().collect();

    result.misses = failing.difference(&selected_set).cloned().collect();
    result.expected = failing.into_iter().collect();
    result.listed = selected;
    result.blastr_items = variant.analysis.items.len() as u32;
    result.reachable = reachable(&variant, mutant);
    result.suite_total = if output.total == 0 {
        baseline.total
    } else {
        output.total
    };
    if result.suite_total > 0 {
        let share = result.listed.len() as f64 * 100.0 / f64::from(result.suite_total);
        result.suite_selected_percent = share as f32;
    }
    if result.expected.is_empty() {
        result
            .notes
            .push("the suite does not notice this mutation".to_owned());
    }
    Ok((result, evidence))
}

/// The symbols that depend on the mutated function, by plain reverse
/// reachability.
fn reachable(variant: &blastr_core::Variant, mutant: &PlannedMutant) -> u32 {
    let Some(seed) = compiler::seed_in(
        &variant.head,
        &mutant.symbol,
        &mutant.file,
        mutant.edit.range.start as u32,
    ) else {
        return 0;
    };
    u32::try_from(reach::dependents(&variant.head, &[seed]).len()).unwrap_or(u32::MAX)
}
