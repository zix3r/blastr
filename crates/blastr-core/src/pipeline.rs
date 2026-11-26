//! The one analysis pipeline, and the one place file edits reach a loaded
//! project.
//!
//! Whatever produced the two graphs — the cache, two project loads, one load
//! with the head side applied on top, or a [`Session`](crate::Session) that
//! keeps the working tree loaded — [`pipeline`] classifies the changes between
//! them, hands them over as soon as they are known, and traces their impact.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::sync::Arc;

use blastr_git::Repo;
use blastr_model::{
    Analysis, AnalysisKey, AnalysisOptions, AnalysisParams, CancelToken, Change, ChangedFile,
    Comparison, DiffResult, ExecMode, FileStatus, IgnoreSet, Limit, Progress, RelPath,
    RevisionGraph, Source,
};

use crate::error::Result;
use crate::hunks::{changed_lines, overlaps};
use crate::impact::ImpactInput;

/// The file name whose changes are reported as manifest changes.
pub(crate) const MANIFEST_FILE: &str = "Cargo.toml";

/// Reports a step's progress under the phase that runs it.
///
/// rust-analyzer names its own steps (`metadata`, `database`, `extract`)
/// without saying which side they belong to; under `load head` its
/// `database` is reported as `load head: database`, and a step named as
/// the phase it runs under (`extract` under `extract head`) as the phase.
pub(crate) struct Within<'a> {
    /// Where the reports go.
    pub(crate) inner: &'a dyn Progress,
    /// The phase the steps run under.
    pub(crate) phase: String,
}

impl Progress for Within<'_> {
    fn report(&self, step: &str, done: u64, total: Option<u64>) {
        if self.phase.starts_with(step) {
            self.inner.report(&self.phase, done, total);
        } else {
            self.inner
                .report(&format!("{}: {step}", self.phase), done, total);
        }
    }
}

/// What every step of one run against an open repository reads.
#[derive(Clone, Copy)]
pub(crate) struct Run<'a> {
    /// The repository the sides belong to.
    pub(crate) repo: &'a Repo,
    /// Configuration to analyse the project under.
    pub(crate) options: &'a AnalysisOptions,
    /// The toolchain version recorded in analysis keys.
    pub(crate) toolchain: &'a str,
    /// Where progress is reported.
    pub(crate) progress: &'a dyn Progress,
    /// Cancellation flag.
    pub(crate) cancel: &'a CancelToken,
}

/// Both graphs of one comparison and the cache keys they belong to.
#[derive(Clone)]
pub(crate) struct Sides {
    /// The graph of the base side.
    pub(crate) base: Arc<RevisionGraph>,
    /// Its key; `None` when the source has no content id.
    pub(crate) base_key: Option<AnalysisKey>,
    /// The graph of the head side.
    pub(crate) head: Arc<RevisionGraph>,
    /// Its key; `None` when the source has no content id.
    pub(crate) head_key: Option<AnalysisKey>,
    /// The procedural-macro crates either graph may be missing, sorted and
    /// without duplicates.
    pub(crate) missing_macros: Vec<String>,
}

/// A project loaded into rust-analyzer.
pub(crate) struct Loaded {
    /// The loaded project.
    workspace: blastr_ra::Workspace,
    /// The procedural-macro crates it could not use, sorted and without
    /// duplicates. They do not change while the project stays loaded.
    pub(crate) missing_macros: Vec<String>,
}

impl Loaded {
    /// Wraps a freshly loaded project.
    pub(crate) fn new(workspace: blastr_ra::Workspace) -> Loaded {
        let mut missing_macros = workspace.missing_proc_macros().to_vec();
        missing_macros.sort();
        missing_macros.dedup();
        Loaded {
            workspace,
            missing_macros,
        }
    }

    /// Extracts the graph of the project as it is loaded now.
    pub(crate) fn extract(&self, run: &Run<'_>, phase: &str) -> Result<RevisionGraph> {
        let within = Within {
            inner: run.progress,
            phase: format!("extract {phase}"),
        };
        within.report(&within.phase, 0, None);
        let graph = self.workspace.extract(&within, run.cancel)?;
        run.cancel.check()?;
        Ok(graph)
    }

    /// Gives the loaded project the text `head` has for `changed_files` and
    /// extracts it.
    ///
    /// A file that no longer differs keeps whatever text it was last given, so
    /// the project must have been loaded from the base side or from `head`.
    pub(crate) fn overlay(
        &mut self,
        run: &Run<'_>,
        head: &Source,
        changed_files: &[ChangedFile],
    ) -> Result<RevisionGraph> {
        let edits = file_edits(run.repo, head, changed_files)?;
        run.cancel.check()?;
        run.progress.report("apply", edits.len() as u64, None);
        self.workspace.apply(&edits)?;
        run.cancel.check()?;
        self.extract(run, "head")
    }
}

/// Classifies the changes between the two graphs, hands them to `on_changes`
/// and traces their impact.
pub(crate) fn pipeline(
    run: &Run<'_>,
    comparison: &Comparison,
    changed_files: Vec<ChangedFile>,
    sides: &Sides,
    params: &AnalysisParams,
    on_changes: &mut dyn FnMut(&DiffResult),
) -> Result<Analysis> {
    run.progress.report("classify", 0, None);
    let mut changes = crate::diff::classify(
        &sides.base,
        &sides.head,
        &changed_files,
        &crate::diff::MatchParams::default(),
    );
    changes.extend(manifest_changes(run.repo, comparison, &changed_files)?);
    let lines = changed_lines_of(run.repo, comparison, &changed_files)?;
    let limits = collect_limits(
        run,
        &sides.base,
        &sides.head,
        sides.base.failed_queries + sides.head.failed_queries,
        &lines,
        &sides.missing_macros,
    )?;
    let diff = DiffResult {
        comparison: comparison.clone(),
        base_key: sides.base_key.clone(),
        head_key: sides.head_key.clone(),
        changed_files,
        changes,
        limits,
    };
    on_changes(&diff);
    trace(diff, &sides.base, &sides.head, params, run)
}

/// Traces the impact of `diff.changes` and completes the analysis.
pub(crate) fn trace(
    diff: DiffResult,
    base: &RevisionGraph,
    head: &RevisionGraph,
    params: &AnalysisParams,
    run: &Run<'_>,
) -> Result<Analysis> {
    run.progress.report("impact", 0, None);
    let ignore = IgnoreSet::new(&params.ignore)?;
    let output = crate::impact::analyze(
        &ImpactInput {
            base,
            head,
            changes: &diff.changes,
            params,
            ignore: &ignore,
        },
        run.cancel,
    )?;
    run.cancel.check()?;
    Ok(Analysis {
        diff,
        items: output.items,
        tests: output.tests,
        limits: output.limits,
        params: params.clone(),
    })
}

/// The edits that give a project the text `head` has for `changed_files`.
fn file_edits(
    repo: &Repo,
    head: &Source,
    changed_files: &[ChangedFile],
) -> Result<Vec<blastr_ra::FileEdit>> {
    let mut edits = Vec::new();
    for file in changed_files {
        // A rename is the removal of the old path and the addition of the new one.
        if let Some(old_path) = &file.old_path {
            edits.push(blastr_ra::FileEdit {
                path: old_path.clone(),
                content: None,
            });
        }
        if file.status == FileStatus::Deleted {
            edits.push(blastr_ra::FileEdit {
                path: file.path.clone(),
                content: None,
            });
            continue;
        }
        // Text that is not UTF-8 cannot be a Rust source file and
        // rust-analyzer would drop it as well, so the old text is left alone.
        if let Some(text) = read_utf8(repo.read_file(head, &file.path)?) {
            edits.push(blastr_ra::FileEdit {
                path: file.path.clone(),
                content: Some(text),
            });
        }
    }
    Ok(edits)
}

/// Invalid UTF-8 counts as an absent file.
fn read_utf8(bytes: Option<Vec<u8>>) -> Option<String> {
    bytes.and_then(|bytes| String::from_utf8(bytes).ok())
}

/// Feature and dependency changes of the `Cargo.toml` files that changed.
fn manifest_changes(
    repo: &Repo,
    comparison: &Comparison,
    changed_files: &[ChangedFile],
) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    for file in changed_files {
        if file.path.as_str().rsplit('/').next() != Some(MANIFEST_FILE) {
            continue;
        }
        let old_path = file.old_path.as_ref().unwrap_or(&file.path);
        let old = read_utf8(repo.read_file(&comparison.base, old_path)?);
        let new = read_utf8(repo.read_file(&comparison.head, &file.path)?);
        changes.extend(crate::diff::manifest_changes(
            &file.path,
            old.as_deref(),
            new.as_deref(),
        ));
    }
    Ok(changes)
}

/// The head-side lines that differ from the base side, per changed file.
///
/// Files that did not change in content are absent, and so are files that are
/// not UTF-8 text, which the pipeline cannot compare.
fn changed_lines_of(
    repo: &Repo,
    comparison: &Comparison,
    changed_files: &[ChangedFile],
) -> Result<BTreeMap<RelPath, Vec<RangeInclusive<u32>>>> {
    let mut lines = BTreeMap::new();
    for file in changed_files {
        if !matches!(file.status, FileStatus::Added | FileStatus::Modified) {
            continue;
        }
        let Some(head) = read_utf8(repo.read_file(&comparison.head, &file.path)?) else {
            continue;
        };
        let base = if file.status == FileStatus::Added {
            None
        } else {
            let old = file.old_path.as_ref().unwrap_or(&file.path);
            read_utf8(repo.read_file(&comparison.base, old)?)
        };
        let ranges = changed_lines(base.as_deref(), &head);
        if !ranges.is_empty() {
            lines.insert(file.path.clone(), ranges);
        }
    }
    Ok(lines)
}

/// What limits the completeness of a comparison; `failed_queries` counts the
/// rust-analyzer queries that failed while the graphs were extracted.
pub(crate) fn collect_limits(
    run: &Run<'_>,
    base: &RevisionGraph,
    head: &RevisionGraph,
    failed_queries: u32,
    changed_lines: &BTreeMap<RelPath, Vec<RangeInclusive<u32>>>,
    missing_macros: &[String],
) -> Result<Vec<Limit>> {
    let mut limits = Vec::new();
    if run.options.exec == ExecMode::Restricted {
        limits.push(Limit::Restricted);
    }
    if !missing_macros.is_empty() {
        limits.push(Limit::MacrosNotExpanded {
            crates: missing_macros.to_vec(),
        });
    }
    if failed_queries > 0 {
        limits.push(Limit::FailedQueries {
            count: failed_queries,
        });
    }
    let empty = union_of(&crates_without_symbols(base), &crates_without_symbols(head));
    if !empty.is_empty() {
        limits.push(Limit::CratesWithoutSymbols { crates: empty });
    }
    if run.repo.has_submodules()? {
        limits.push(Limit::Submodules);
    }
    if run.repo.uses_lfs()? {
        limits.push(Limit::Lfs);
    }
    if !head.unresolved.is_empty() {
        limits.push(Limit::Unresolved {
            count: u32::try_from(head.unresolved.len()).unwrap_or(u32::MAX),
        });
    }
    // Only inactive code that the comparison actually touched is a limit; an
    // inactive block elsewhere in a changed file is not.
    limits.extend(
        head.inactive
            .iter()
            .filter(|location| {
                changed_lines.get(&location.file).is_some_and(|ranges| {
                    overlaps(ranges, location.span.start_line, location.span.end_line)
                })
            })
            .cloned()
            .map(Limit::ChangedButInactive),
    );
    Ok(limits)
}

/// The crates of `graph` that have no symbol, as `name (root file)`.
///
/// Every crate that is extracted has at least its root module, so a crate
/// without any symbol was loaded but not found again in rust-analyzer's
/// database, or its files could not be placed in the repository. Either way
/// its code is missing, and saying so beats a result that looks complete.
fn crates_without_symbols(graph: &RevisionGraph) -> Vec<String> {
    let mut used = vec![false; graph.crates.len()];
    for symbol in &graph.symbols {
        if let Some(slot) = used.get_mut(symbol.krate.0 as usize) {
            *slot = true;
        }
    }
    graph
        .crates
        .iter()
        .zip(used)
        .filter(|(_, used)| !used)
        .map(|(krate, _)| format!("{} ({})", krate.name, krate.root_file.as_str()))
        .collect()
}

/// The union of two lists of crate names, sorted and without duplicates.
pub(crate) fn union_of(left: &[String], right: &[String]) -> Vec<String> {
    let mut union: Vec<String> = left.iter().chain(right).cloned().collect();
    union.sort();
    union.dedup();
    union
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use blastr_model::{
        CrateId, CrateInfo, Location, Span, Symbol, SymbolFlags, SymbolKind, SymbolPath,
        TargetKind, Visibility,
    };

    use super::*;

    /// A crate of the workspace named `name`, rooted at `root`.
    fn krate(name: &str, kind: TargetKind, root: &str) -> CrateInfo {
        CrateInfo {
            name: name.to_owned(),
            package: name.to_owned(),
            target_kind: kind,
            target_name: name.to_owned(),
            root_file: RelPath::new(root),
        }
    }

    /// The root module of crate `index`, named `name`.
    fn root_module(name: &str, index: u32, root: &str) -> Symbol {
        Symbol {
            path: SymbolPath::new(name, SymbolKind::Module),
            krate: CrateId(index),
            location: Location {
                file: RelPath::new(root),
                span: Span {
                    start: 0,
                    end: 1,
                    start_line: 1,
                    end_line: 1,
                },
            },
            visibility: Visibility::Public,
            signature_hash: 0,
            body_hash: 0,
            attr_hash: 0,
            body_sketch: Vec::new(),
            parent: None,
            flags: SymbolFlags::default(),
            impl_info: None,
        }
    }

    #[test]
    fn a_crate_without_symbols_is_named() {
        // Two crates share a name, as a library and a binary may; only the
        // root file tells them apart.
        let mut graph = RevisionGraph::default();
        graph
            .crates
            .push(krate("tool", TargetKind::Lib, "crates/tool/src/lib.rs"));
        graph
            .crates
            .push(krate("tool", TargetKind::Bin, "src/bin/tool.rs"));
        graph
            .symbols
            .push(root_module("tool", 0, "crates/tool/src/lib.rs"));
        assert_eq!(crates_without_symbols(&graph), ["tool (src/bin/tool.rs)"]);

        graph
            .symbols
            .push(root_module("tool", 1, "src/bin/tool.rs"));
        assert!(crates_without_symbols(&graph).is_empty());
    }

    #[test]
    fn a_step_is_reported_under_the_phase_that_runs_it() {
        let seen = Mutex::new(Vec::new());
        let sink = |phase: &str, done: u64, total: Option<u64>| {
            seen.lock().unwrap().push((phase.to_owned(), done, total));
        };
        let within = Within {
            inner: &sink,
            phase: "extract head".to_owned(),
        };
        within.report("extract", 3, Some(9));
        within.report("database", 0, None);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                ("extract head".to_owned(), 3, Some(9)),
                ("extract head: database".to_owned(), 0, None),
            ]
        );
    }
}
