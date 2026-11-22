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
use std::path::Path;

use blastr_core::{FileEdit, build_id};
use blastr_model::{AnalysisParams, ImpactClass, RelPath, RevisionGraph, SymbolId};

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
    /// The analysis parameters to run with; `None` takes the defaults.
    pub params: Option<AnalysisParams>,
}

/// Runs the compiler check over the project.
pub fn run(project: &mut Project, options: &Options) -> Result<CheckReport> {
    let params = options.params.clone().unwrap_or_default();
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
    let mut relevant_roots: BTreeSet<RelPath> = BTreeSet::new();
    for error in &check.errors {
        match place(&span_index, project.copy(), error) {
            Placement::Symbol(path) => {
                expected.insert(path.path.clone());
                note_roots(&variant.head, &path.path, &mut relevant_roots);
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

    // A target the compiler never started cannot confirm or deny its items: a
    // failing library stops every target that links against it, so a test, a
    // binary or a dependent crate is not built at all. Cargo names the target
    // of every diagnostic and the crate root of every artifact, so an item is
    // judged exactly when the root file of its target was compiled. Items that
    // were not are left out of both sides and counted; a case whose items are
    // all in such targets is not scored at all.
    note_roots(&variant.head, &case.symbol, &mut relevant_roots);
    for path in expected.iter().chain(listed.iter()) {
        note_roots(&variant.head, path, &mut relevant_roots);
    }
    let judged = judged_targets(&check, project.copy());
    let checked = |path: &String| {
        let Some(judged) = &judged else {
            return true;
        };
        let mut roots = BTreeSet::new();
        note_roots(&variant.head, path, &mut roots);
        roots.is_empty() || roots.iter().any(|root| judged.contains(root))
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

    let confirmed: Vec<&RelPath> = match &judged {
        Some(judged) => relevant_roots
            .iter()
            .filter(|root| judged.contains(*root))
            .collect(),
        None => relevant_roots.iter().collect(),
    };
    if confirmed.is_empty() && !relevant_roots.is_empty() {
        result.excluded = Some("crate-not-checked".to_owned());
        result.notes.push(format!(
            "the compiler did not build {}",
            target_names(&variant.head, &relevant_roots)
        ));
        return Ok((result, evidence));
    }
    if result.unconfirmed > 0
        && let Some(judged) = &judged
    {
        let missing: BTreeSet<RelPath> = relevant_roots
            .iter()
            .filter(|root| !judged.contains(*root))
            .cloned()
            .collect();
        result.notes.push(format!(
            "{} item(s) in targets the compiler did not build ({})",
            result.unconfirmed,
            target_names(&variant.head, &missing)
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
    // rustc reports one privacy error per target and then stops looking at the
    // other literals and accesses of the same item, so a target that reported
    // one says nothing about the items of that target it did not report. An
    // item Blastr lists there is not a false alarm and cannot be confirmed
    // either; it is counted as unconfirmed, as an item of a target the
    // compiler never built is.
    let privacy = privacy_targets(&check, project.copy());
    if !privacy.is_empty() {
        let mut suppressed = 0_u32;
        result.false_alarms.retain(|path| {
            let mut roots = BTreeSet::new();
            note_roots(&variant.head, path, &mut roots);
            let stopped = roots.iter().any(|root| privacy.contains(root));
            if stopped {
                suppressed = suppressed.saturating_add(1);
            }
            !stopped
        });
        if suppressed > 0 {
            result.unconfirmed = result.unconfirmed.saturating_add(suppressed);
            result.notes.push(format!(
                "{suppressed} item(s) unconfirmed: rustc stopped after the first privacy error \
                 of their target"
            ));
        }
    }
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

/// Adds the crate roots of the crates that hold the symbol at `path`.
fn note_roots(graph: &RevisionGraph, path: &str, into: &mut BTreeSet<RelPath>) {
    for symbol in &graph.symbols {
        if symbol.path.path != path {
            continue;
        }
        if let Some(info) = graph.crates.get(symbol.krate.0 as usize) {
            into.insert(info.root_file.clone());
        }
    }
}

/// The crate root files of the targets the compiler started, whether they
/// succeeded or not; `None` when Cargo's messages named no root file at all.
///
/// A target that compiled reports an artifact; a target that failed reports
/// diagnostics, which Cargo labels with the crate root of the target they come
/// from. A target that never started does neither.
fn judged_targets(check: &CheckOutput, root: &Path) -> Option<BTreeSet<RelPath>> {
    let mut roots = BTreeSet::new();
    let mut named = false;
    for artifact in &check.artifacts {
        if artifact.kind == "custom-build" {
            continue;
        }
        if let Some(path) = &artifact.root_file {
            named = true;
            if let Some(relative) = map::rel_path(root, path) {
                roots.insert(relative);
            }
        }
    }
    for error in &check.errors {
        if let Some(path) = &error.root_file {
            named = true;
            if let Some(relative) = map::rel_path(root, path) {
                roots.insert(relative);
            }
        }
    }
    named.then_some(roots)
}

/// The error codes that report a use of something the change made private.
///
/// rustc stops after the first of them in a target: the code that checks
/// privacy reports one site and returns, so the other literals and accesses of
/// the same field produce no error at all.
pub const PRIVACY_CODES: &[&str] = &[
    "E0451", // field is private, in a struct expression
    "E0603", // item is private, and cannot be resolved
    "E0616", // field is private, in an access
    "E0624", // method is private
];

/// The crate roots of the targets that reported a privacy error.
fn privacy_targets(check: &CheckOutput, root: &Path) -> BTreeSet<RelPath> {
    check
        .errors
        .iter()
        .filter(|error| PRIVACY_CODES.contains(&error.code.as_str()))
        .filter_map(|error| error.root_file.as_deref())
        .filter_map(|file| map::rel_path(root, file))
        .collect()
}

/// How the given crate roots are shown in a note: `package (target)`, or the
/// path of a root no crate of the graph owns.
fn target_names(graph: &RevisionGraph, roots: &BTreeSet<RelPath>) -> String {
    let mut names: Vec<String> = Vec::new();
    for root in roots {
        let mut owned = false;
        for info in &graph.crates {
            if &info.root_file != root {
                continue;
            }
            owned = true;
            names.push(format!("{} ({})", info.package, info.target_name));
        }
        if !owned {
            names.push(root.as_str().to_owned());
        }
    }
    names.sort();
    names.dedup();
    names.join(", ")
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
    fn a_target_that_never_started_is_not_judged() {
        let root = Path::new("C:/tmp/copy");
        let check = CheckOutput {
            artifacts: vec![cargo::Artifact {
                package: "tinycore".to_owned(),
                kind: "lib".to_owned(),
                target: "tinycore".to_owned(),
                root_file: Some(root.join("tinycore/src/lib.rs")),
            }],
            errors: vec![Diagnostic {
                code: "E0308".to_owned(),
                message: String::new(),
                file: None,
                start: 0,
                end: 0,
                line: 1,
                root_file: Some(root.join("tinycore/src/lib.rs")),
            }],
            ..CheckOutput::default()
        };

        let judged = judged_targets(&check, root).expect("the messages name their targets");

        assert!(judged.contains(&RelPath::new("tinycore/src/lib.rs")));
        assert!(
            !judged.contains(&RelPath::new("tinyapp/tests/smoke.rs")),
            "a test of a crate that failed to compile never ran"
        );
    }

    #[test]
    fn without_target_information_every_item_is_judged() {
        let check = CheckOutput::default();
        assert!(judged_targets(&check, Path::new("C:/tmp/copy")).is_none());
    }

    #[test]
    fn a_target_that_reported_a_privacy_error_stopped_early() {
        let root = Path::new("C:/tmp/copy");
        let error = |code: &str, file: &str| Diagnostic {
            code: code.to_owned(),
            message: String::new(),
            file: None,
            start: 0,
            end: 0,
            line: 1,
            root_file: Some(root.join(file)),
        };
        let check = CheckOutput {
            errors: vec![
                error("E0616", "storage/src/lib.rs"),
                error("E0308", "domain/src/lib.rs"),
            ],
            ..CheckOutput::default()
        };

        let stopped = privacy_targets(&check, root);

        assert!(stopped.contains(&RelPath::new("storage/src/lib.rs")));
        assert!(
            !stopped.contains(&RelPath::new("domain/src/lib.rs")),
            "a type error says nothing about privacy"
        );
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
