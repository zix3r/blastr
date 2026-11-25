//! The open repository, what is being compared and what came back.
//!
//! Its methods are the state transitions of the window: opening a repository,
//! choosing a comparison, a result arriving, a job being cancelled, a failure.
//! None of them does any work or touches the screen, so they are tested as
//! plain functions. Every job carries a generation, so a result of a job that
//! was replaced is dropped, and a failed refresh keeps the last good result.

use std::sync::Arc;
use std::time::{Duration, Instant};

use blastr_model::protocol::{Commit, Failure, JobId, JobKind, Repository};
use blastr_model::{
    Analysis, AnalysisOptions, AnalysisParams, Assume, DiffResult, ImpactClass, RevSpec,
    RevisionGraph, SymbolPath,
};

use super::loadable::{Loadable, Progress};
use super::selection::{History, Selection};

/// What the window compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compare {
    /// The uncommitted changes against `HEAD`.
    WorkingTree,
    /// One commit against its parent.
    Commit(Commit),
    /// A range of commits: from the parent of `oldest` (or `oldest` itself
    /// when it has no parent) to `newest`.
    Range {
        oldest: Commit,
        newest: Commit,
        from_parent: bool,
    },
}

impl Compare {
    /// The revision spec the engine reads.
    pub fn spec(&self) -> RevSpec {
        match self {
            Compare::WorkingTree => RevSpec {
                worktree: true,
                ..RevSpec::default()
            },
            Compare::Commit(commit) => RevSpec {
                rev: Some(commit.id.clone()),
                ..RevSpec::default()
            },
            Compare::Range {
                oldest,
                newest,
                from_parent,
            } => {
                let parent = if *from_parent { "^" } else { "" };
                RevSpec {
                    rev: Some(format!("{}{parent}..{}", oldest.id, newest.id)),
                    ..RevSpec::default()
                }
            }
        }
    }

    /// The labels of the two sides, older first.
    pub fn labels(&self) -> (String, String) {
        match self {
            Compare::WorkingTree => ("HEAD".to_owned(), "Working tree".to_owned()),
            Compare::Commit(commit) => (format!("{}^", commit.short), commit.short.clone()),
            Compare::Range {
                oldest,
                newest,
                from_parent,
            } => {
                let parent = if *from_parent { "^" } else { "" };
                (format!("{}{parent}", oldest.short), newest.short.clone())
            }
        }
    }

    /// Whether the comparison follows the working tree, and so refreshes when
    /// a file is saved.
    pub fn watches(&self) -> bool {
        matches!(self, Compare::WorkingTree)
    }
}

/// A result that arrived.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The analysis.
    pub analysis: Arc<Analysis>,
    /// The graph of the newer side, for symbol search.
    pub graph: Option<Arc<RevisionGraph>>,
    /// How long the engine took.
    pub elapsed: Duration,
    /// Counts the results of one comparison, so that views can tell a
    /// refreshed result from the one they drew.
    pub revision: u64,
}

impl PartialEq for Outcome {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.analysis, &other.analysis) && self.revision == other.revision
    }
}

/// A hypothetical change of one symbol, shown instead of the comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct WhatIf {
    pub symbol: SymbolPath,
    pub assume: Assume,
    pub result: Loadable<Outcome>,
}

/// One kind of job: the current generation and whether it runs.
#[derive(Debug, Default)]
pub struct TaskSlot {
    generation: u64,
    running: bool,
    /// Generations stopped since the engine was last told.
    cancelled: Vec<u64>,
}

impl TaskSlot {
    /// Starts a new job, cancelling the one that runs.
    pub fn begin(&mut self) -> u64 {
        self.cancel();
        self.generation += 1;
        self.running = true;
        self.generation
    }

    /// Stops the running job; whether there was one.
    pub fn cancel(&mut self) -> bool {
        if !self.running {
            return false;
        }
        self.running = false;
        self.cancelled.push(self.generation);
        true
    }

    /// Whether `generation` is the job that runs.
    pub fn is_current(&self, generation: u64) -> bool {
        self.running && self.generation == generation
    }

    /// Whether a job runs.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The generation of the newest job.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Ends the job `generation`; whether it was the current one.
    pub fn finish(&mut self, generation: u64) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.running = false;
        true
    }
}

/// The state of the window's work.
#[derive(Debug, Default)]
pub struct Session {
    pub repo: Option<Repository>,
    pub options: AnalysisOptions,
    pub params: AnalysisParams,
    pub compare: Option<Compare>,
    /// The classified changes, which arrive before the result.
    pub changes: Option<Arc<DiffResult>>,
    pub result: Loadable<Outcome>,
    /// Whether the last comparison was cancelled.
    pub cancelled: bool,
    /// The progress of a refresh while the last result stays on screen.
    pub refreshing: Option<Progress>,
    /// When the comparison, refresh or hypothetical change being computed
    /// started.
    pub started: Option<Instant>,
    pub what_if: Option<WhatIf>,
    pub history: History,
    pub run: TaskSlot,
    pub what_if_run: TaskSlot,
}

impl Session {
    /// Opens a repository, forgetting everything about the last one.
    pub fn open_repository(&mut self, repo: Repository) {
        self.close_repository();
        self.repo = Some(repo);
    }

    /// Closes the repository and stops its jobs.
    pub fn close_repository(&mut self) {
        self.run.cancel();
        self.what_if_run.cancel();
        // The generations go on counting, so that a late answer of a job of
        // the last repository is never taken for one of this.
        *self = Session {
            run: std::mem::take(&mut self.run),
            what_if_run: std::mem::take(&mut self.what_if_run),
            ..Session::default()
        };
    }

    fn take_cancelled_slots(&mut self) -> (Vec<u64>, Vec<u64>) {
        (
            std::mem::take(&mut self.run.cancelled),
            std::mem::take(&mut self.what_if_run.cancelled),
        )
    }

    /// The jobs stopped since the last call, which the engine must be told of.
    pub fn take_cancelled(&mut self) -> Vec<JobId> {
        let (runs, what_ifs) = self.take_cancelled_slots();
        let job = |kind| move |generation| JobId { kind, generation };
        runs.into_iter()
            .map(job(JobKind::Compare))
            .chain(what_ifs.into_iter().map(job(JobKind::WhatIf)))
            .collect()
    }

    /// What a freshly opened repository compares: its uncommitted changes
    /// when there are any, else its newest commit.
    pub fn default_compare(&self) -> Option<Compare> {
        let repo = self.repo.as_ref()?;
        match &repo.head {
            Some(head) if !repo.dirty => Some(Compare::Commit(head.clone())),
            _ => Some(Compare::WorkingTree),
        }
    }

    /// Starts a comparison; returns its generation.
    pub fn choose(&mut self, compare: Compare) -> u64 {
        self.compare = Some(compare);
        self.changes = None;
        self.result = Loadable::Loading(Progress::default());
        self.cancelled = false;
        self.refreshing = None;
        self.what_if_run.cancel();
        self.what_if = None;
        self.history.clear();
        self.started = Some(Instant::now());
        self.run.begin()
    }

    /// Runs the current comparison again, keeping the selection.
    pub fn retry(&mut self) -> Option<u64> {
        let compare = self.compare.clone()?;
        let history = std::mem::take(&mut self.history);
        let generation = self.choose(compare);
        self.history = history;
        Some(generation)
    }

    /// A progress report of the comparison `generation`.
    pub fn progress(&mut self, generation: u64, progress: Progress) {
        if !self.run.is_current(generation) {
            return;
        }
        match &mut self.result {
            Loadable::Loading(current) => *current = progress,
            Loadable::Ready(_) if self.refreshing.is_some() => self.refreshing = Some(progress),
            _ => {}
        }
    }

    /// The classified changes of `generation` arrived.
    pub fn changes_arrived(&mut self, generation: u64, diff: Arc<DiffResult>) -> bool {
        if !self.run.is_current(generation) {
            return false;
        }
        self.changes = Some(diff);
        true
    }

    /// The result of `generation` arrived. A comparison that `watching` stays
    /// alive and sends a result after every save.
    pub fn result_arrived(
        &mut self,
        generation: u64,
        analysis: Arc<Analysis>,
        graph: Option<Arc<RevisionGraph>>,
        elapsed: Duration,
        watching: bool,
    ) -> bool {
        if !self.run.is_current(generation) {
            return false;
        }
        let previous = self.result.ready();
        let revision = previous.map_or(1, |outcome| outcome.revision + 1);
        let graph = graph.or_else(|| previous.and_then(|outcome| outcome.graph.clone()));
        // The analysis carries its own changes from now on.
        self.changes = None;
        self.result = Loadable::Ready(Outcome {
            analysis,
            graph,
            elapsed,
            revision,
        });
        self.refreshing = None;
        self.cancelled = false;
        if !watching {
            self.run.finish(generation);
        }
        self.drop_stale_selections();
        self.select_something();
        true
    }

    /// A watching comparison noticed a save and started again.
    pub fn refresh_started(&mut self, generation: u64) {
        if self.run.is_current(generation) && self.result.ready().is_some() {
            self.refreshing = Some(Progress::default());
            self.started = Some(Instant::now());
        }
    }

    /// The comparison `generation` failed. A failure while watching keeps the
    /// last good result on screen.
    pub fn failed(&mut self, generation: u64, failure: Failure, watching: bool) -> bool {
        if !self.run.is_current(generation) {
            return false;
        }
        self.refreshing = None;
        if watching && self.result.ready().is_some() {
            return true;
        }
        self.result = Loadable::Failed(failure);
        self.run.finish(generation);
        true
    }

    /// Cancels what runs: a hypothetical change first, else a comparison that
    /// has no result yet. Returns whether something stopped.
    pub fn cancel(&mut self) -> bool {
        if self.what_if_run.is_running() {
            self.leave_what_if();
            return true;
        }
        if !self.result.is_loading() {
            return false;
        }
        self.run.cancel();
        self.result = Loadable::Empty;
        self.cancelled = true;
        true
    }

    /// Whether something is being computed.
    pub fn is_busy(&self) -> bool {
        self.result.is_loading()
            || self.refreshing.is_some()
            || self
                .what_if
                .as_ref()
                .is_some_and(|what_if| what_if.result.is_loading())
    }

    /// How long the work on screen has run, while it runs.
    pub fn running_for(&self) -> Option<Duration> {
        self.is_busy()
            .then(|| self.started.map(|started| started.elapsed()))
            .flatten()
    }

    /// Whether a finished comparison follows the working tree.
    pub fn is_watching(&self) -> bool {
        self.run.is_running() && self.result.ready().is_some()
    }

    /// Starts a hypothetical change; returns its generation.
    pub fn begin_what_if(&mut self, symbol: SymbolPath, assume: Assume) -> u64 {
        self.started = Some(Instant::now());
        self.what_if = Some(WhatIf {
            symbol,
            assume,
            result: Loadable::Loading(Progress::default()),
        });
        self.what_if_run.begin()
    }

    /// A progress report of the hypothetical change `generation`.
    pub fn what_if_progress(&mut self, generation: u64, progress: Progress) {
        if !self.what_if_run.is_current(generation) {
            return;
        }
        if let Some(WhatIf {
            result: Loadable::Loading(current),
            ..
        }) = &mut self.what_if
        {
            *current = progress;
        }
    }

    /// The result of the hypothetical change `generation` arrived.
    pub fn what_if_arrived(
        &mut self,
        generation: u64,
        analysis: Arc<Analysis>,
        elapsed: Duration,
    ) -> bool {
        if !self.what_if_run.finish(generation) {
            return false;
        }
        let graph = self
            .result
            .ready()
            .and_then(|outcome| outcome.graph.clone());
        if let Some(what_if) = &mut self.what_if {
            what_if.result = Loadable::Ready(Outcome {
                analysis,
                graph,
                elapsed,
                revision: generation,
            });
        }
        self.select_something();
        true
    }

    /// The hypothetical change `generation` failed.
    pub fn what_if_failed(&mut self, generation: u64, failure: Failure) -> bool {
        if !self.what_if_run.finish(generation) {
            return false;
        }
        if let Some(what_if) = &mut self.what_if {
            what_if.result = Loadable::Failed(failure);
        }
        true
    }

    /// Goes back from the hypothetical change to the comparison.
    pub fn leave_what_if(&mut self) {
        self.what_if_run.cancel();
        if self.what_if.take().is_some() {
            self.drop_stale_selections();
        }
    }

    /// The result on screen: the hypothetical one while there is one.
    pub fn shown(&self) -> &Loadable<Outcome> {
        match &self.what_if {
            Some(what_if) => &what_if.result,
            None => &self.result,
        }
    }

    /// The analysis on screen.
    pub fn analysis(&self) -> Option<&Arc<Analysis>> {
        self.shown().ready().map(|outcome| &outcome.analysis)
    }

    /// The changes on screen: those of the analysis, or those that arrived
    /// before it.
    pub fn shown_changes(&self) -> Option<&DiffResult> {
        if let Some(analysis) = self.analysis() {
            return Some(&analysis.diff);
        }
        if self.what_if.is_some() {
            return None;
        }
        self.changes.as_deref()
    }

    /// The current selection.
    pub fn selection(&self) -> Option<&Selection> {
        self.history.current()
    }

    /// Selects something, remembering it in the history.
    pub fn select(&mut self, selection: Selection) {
        self.history.select(selection);
    }

    /// Makes sure a result never leaves the views without a selection.
    fn select_something(&mut self) {
        let Some(outcome) = self.shown().ready() else {
            return;
        };
        let keep = self.selection().is_some_and(|selection| {
            selection_exists(
                &outcome.analysis,
                outcome.graph.as_deref(),
                &selection.symbol,
            )
        });
        if !keep && let Some(selection) = default_selection(&outcome.analysis) {
            self.select(selection);
        }
    }

    /// Forgets selections the result no longer mentions, when there is no
    /// graph to find them in.
    fn drop_stale_selections(&mut self) {
        let Some(outcome) = self.shown().ready() else {
            return;
        };
        if outcome.graph.is_some() {
            return;
        }
        let analysis = Arc::clone(&outcome.analysis);
        self.history
            .retain(|selection| mentions(&analysis, &selection.symbol));
    }
}

/// Whether the analysis lists `symbol` as an item or a change.
fn mentions(analysis: &Analysis, symbol: &SymbolPath) -> bool {
    analysis.items.iter().any(|item| &item.symbol == symbol)
        || analysis
            .diff
            .changes
            .iter()
            .any(|change| &change.symbol == symbol)
}

/// Whether `symbol` is still something the views can show.
fn selection_exists(
    analysis: &Analysis,
    graph: Option<&RevisionGraph>,
    symbol: &SymbolPath,
) -> bool {
    mentions(analysis, symbol)
        || graph.is_some_and(|graph| graph.symbols.iter().any(|entry| &entry.path == symbol))
}

/// What a fresh result selects: the first item to edit, else the first that
/// may behave differently, else the first change.
fn default_selection(analysis: &Analysis) -> Option<Selection> {
    let open = analysis
        .items
        .iter()
        .find(|item| item.class == ImpactClass::Adaptation && !item.tags.changed)
        .or_else(|| {
            analysis
                .items
                .iter()
                .find(|item| item.class == ImpactClass::Behaviour)
        });
    if let Some(item) = open {
        return Some(Selection::symbol(item.symbol.clone()));
    }
    analysis
        .diff
        .changes
        .first()
        .map(|change| Selection::symbol(change.symbol.clone()))
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        Change, ChangeDetail, ChangeKind, Comparison, ImpactItem, ImpactLimits, ImpactTags,
        Location, RelPath, Source, Span, SymbolKind, TestSelection,
    };

    use super::*;

    fn repo(dirty: bool) -> Repository {
        Repository {
            root: "/code/invoicing".into(),
            name: "invoicing".to_owned(),
            branch: Some("main".to_owned()),
            head: Some(commit("a3f2e1c")),
            dirty,
        }
    }

    fn commit(short: &str) -> Commit {
        Commit {
            id: format!("{short}000000000000000000000000000000000"),
            short: short.to_owned(),
        }
    }

    fn symbol(name: &str) -> SymbolPath {
        SymbolPath::new(format!("billing::{name}"), SymbolKind::Function)
    }

    fn diff(names: &[&str]) -> DiffResult {
        DiffResult {
            comparison: Comparison {
                base: Source::Worktree,
                head: Source::Worktree,
            },
            base_key: None,
            head_key: None,
            changed_files: Vec::new(),
            changes: names
                .iter()
                .map(|name| Change {
                    kind: ChangeKind::Body,
                    symbol: symbol(name),
                    old: None,
                    new: None,
                    detail: ChangeDetail::None,
                })
                .collect(),
            limits: Vec::new(),
        }
    }

    fn item(name: &str, class: ImpactClass, changed: bool) -> ImpactItem {
        ImpactItem {
            symbol: symbol(name),
            location: Location {
                file: RelPath::new("src/lib.rs"),
                span: Span {
                    start: 0,
                    end: 1,
                    start_line: 1,
                    end_line: 1,
                },
            },
            class,
            confidence: blastr_model::Confidence::Likely,
            distance: 1,
            paths: Vec::new(),
            path_count: 0,
            tags: ImpactTags {
                changed,
                ..ImpactTags::default()
            },
        }
    }

    fn analysis_of(names: &[&str], items: Vec<ImpactItem>) -> Arc<Analysis> {
        Arc::new(Analysis {
            diff: diff(names),
            items,
            tests: TestSelection::default(),
            limits: ImpactLimits::default(),
            params: AnalysisParams::default(),
        })
    }

    fn analysis(names: &[&str]) -> Arc<Analysis> {
        analysis_of(names, Vec::new())
    }

    fn opened(dirty: bool) -> Session {
        let mut session = Session::default();
        session.open_repository(repo(dirty));
        session
    }

    fn selected(session: &Session) -> Option<String> {
        session.selection().map(|s| s.symbol.name().to_owned())
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_repository_starts_on_its_changes_or_its_newest_commit() {
        assert_eq!(opened(true).default_compare(), Some(Compare::WorkingTree));
        assert_eq!(
            opened(false).default_compare(),
            Some(Compare::Commit(commit("a3f2e1c")))
        );
        let mut session = Session::default();
        let mut info = repo(false);
        info.head = None;
        session.open_repository(info);
        assert_eq!(session.default_compare(), Some(Compare::WorkingTree));
        assert_eq!(Session::default().default_compare(), None);
    }

    #[test]
    fn comparisons_become_revision_specs() {
        assert!(Compare::WorkingTree.spec().worktree);
        let range = Compare::Range {
            oldest: commit("aaaaaaa"),
            newest: commit("bbbbbbb"),
            from_parent: true,
        };
        assert_eq!(
            range.spec().rev.as_deref(),
            Some(
                "aaaaaaa000000000000000000000000000000000^..bbbbbbb000000000000000000000000000000000"
            )
        );
        assert_eq!(
            range.labels(),
            ("aaaaaaa^".to_owned(), "bbbbbbb".to_owned())
        );
        assert!(Compare::WorkingTree.watches());
        assert!(!range.watches());
    }

    #[test]
    fn changes_arrive_before_the_result() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        assert!(session.is_busy());
        assert!(session.changes_arrived(run, Arc::new(diff(&["vat_rate"]))));
        assert!(session.result.is_loading());
        assert_eq!(session.shown_changes().map(|d| d.changes.len()), Some(1));
        assert_eq!(session.analysis(), None);
    }

    #[test]
    fn a_result_of_a_one_shot_comparison_finishes_the_job() {
        let mut session = opened(false);
        let run = session.choose(Compare::Commit(commit("a3f2e1c")));
        assert!(session.result_arrived(run, analysis(&["vat_rate"]), None, SECOND, false));
        assert!(session.analysis().is_some());
        assert!(
            session.changes.is_none(),
            "the analysis carries the changes"
        );
        assert!(!session.run.is_running());
        assert!(!session.is_busy());
    }

    #[test]
    fn a_watched_comparison_stays_alive_and_counts_its_results() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        session.result_arrived(run, analysis(&["a"]), None, SECOND, true);
        assert!(session.is_watching());
        session.refresh_started(run);
        assert!(session.is_busy());
        assert!(session.analysis().is_some(), "the old result stays shown");
        session.result_arrived(run, analysis(&["a", "b"]), None, SECOND, true);
        assert!(!session.is_busy());
        assert_eq!(session.result.ready().map(|o| o.revision), Some(2));
        assert_eq!(session.shown_changes().map(|d| d.changes.len()), Some(2));
    }

    #[test]
    fn a_refresh_without_a_graph_keeps_the_last_one() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        let graph = Arc::new(RevisionGraph::default());
        session.result_arrived(run, analysis(&["a"]), Some(graph.clone()), SECOND, true);
        session.result_arrived(run, analysis(&["a", "b"]), None, SECOND, true);
        let kept = session.result.ready().and_then(|o| o.graph.clone());
        assert!(kept.is_some_and(|kept| Arc::ptr_eq(&kept, &graph)));
    }

    #[test]
    fn a_new_comparison_cancels_the_previous_one_and_ignores_its_result() {
        let mut session = opened(true);
        let first = session.choose(Compare::WorkingTree);
        let second = session.choose(Compare::Commit(commit("7d9c4b2")));
        assert_eq!(
            session.take_cancelled(),
            vec![JobId {
                kind: JobKind::Compare,
                generation: first
            }]
        );
        assert!(session.take_cancelled().is_empty(), "told once");
        assert!(!session.changes_arrived(first, Arc::new(diff(&["old"]))));
        assert!(!session.result_arrived(first, analysis(&["old"]), None, SECOND, true));
        assert!(!session.failed(first, Failure::new("late"), false));
        assert!(session.result.is_loading());
        assert!(session.run.is_current(second));
    }

    #[test]
    fn a_failure_ends_the_comparison() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        assert!(session.failed(run, Failure::new("cargo failed"), true));
        assert_eq!(
            session.result.failure().map(|f| f.message.as_str()),
            Some("cargo failed")
        );
        assert!(!session.run.is_running());
    }

    #[test]
    fn a_failed_refresh_keeps_the_last_good_result() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        session.result_arrived(run, analysis(&["a"]), None, SECOND, true);
        session.refresh_started(run);
        session.failed(run, Failure::new("does not parse"), true);
        assert!(session.analysis().is_some());
        assert!(session.is_watching());
        assert!(!session.is_busy());
    }

    #[test]
    fn cancelling_a_running_comparison_leaves_it_stopped() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        assert!(session.cancel());
        assert!(session.cancelled);
        assert_eq!(session.result, Loadable::Empty);
        assert_eq!(session.take_cancelled().len(), 1);
        assert!(!session.cancel(), "nothing is left to cancel");
        assert!(!session.result_arrived(run, analysis(&[]), None, SECOND, true));
    }

    #[test]
    fn cancel_does_not_stop_a_finished_watch() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        session.result_arrived(run, analysis(&["a"]), None, SECOND, true);
        assert!(!session.cancel());
        assert!(session.is_watching());
        assert!(session.take_cancelled().is_empty());
    }

    #[test]
    fn retry_runs_the_same_comparison_and_keeps_the_selection() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        session.select(Selection::symbol(symbol("a")));
        session.failed(run, Failure::new("no"), false);
        let again = session.retry().expect("a comparison to retry");
        assert!(session.result.is_loading());
        assert_ne!(again, run);
        assert!(session.selection().is_some());
        assert!(Session::default().retry().is_none());
    }

    #[test]
    fn a_what_if_result_replaces_the_shown_result_until_it_is_left() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        session.result_arrived(run, analysis(&["real"]), None, SECOND, true);
        let what_if = session.begin_what_if(symbol("total"), Assume::Signature);
        assert!(session.shown().is_loading());
        assert_eq!(session.shown_changes(), None);
        assert!(session.what_if_arrived(what_if, analysis(&["total"]), SECOND));
        let first_change = |session: &Session| {
            session
                .shown_changes()
                .map(|d| d.changes[0].symbol.name().to_owned())
        };
        assert_eq!(first_change(&session), Some("total".to_owned()));
        session.leave_what_if();
        assert_eq!(first_change(&session), Some("real".to_owned()));
        assert!(session.is_watching(), "the real comparison kept running");
    }

    #[test]
    fn a_second_what_if_cancels_the_first() {
        let mut session = opened(true);
        let first = session.begin_what_if(symbol("a"), Assume::Body);
        let second = session.begin_what_if(symbol("b"), Assume::Body);
        assert!(!session.what_if_arrived(first, analysis(&["a"]), SECOND));
        assert!(session.what_if_failed(second, Failure::new("ambiguous")));
        assert!(session.shown().failure().is_some());
        assert_eq!(
            session.take_cancelled(),
            vec![JobId {
                kind: JobKind::WhatIf,
                generation: first
            }]
        );
    }

    #[test]
    fn cancel_leaves_a_running_what_if_first() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        session.result_arrived(run, analysis(&["real"]), None, SECOND, true);
        session.begin_what_if(symbol("a"), Assume::Body);
        assert!(session.cancel());
        assert!(session.what_if.is_none());
        assert!(session.analysis().is_some());
    }

    #[test]
    fn stale_progress_is_ignored() {
        let mut session = opened(true);
        let first = session.choose(Compare::WorkingTree);
        let second = session.choose(Compare::WorkingTree);
        let phase = |name: &str| Progress {
            phase: name.to_owned(),
            ..Progress::default()
        };
        session.progress(first, phase("impact"));
        assert_eq!(
            session.result.progress().map(|p| p.label()).as_deref(),
            Some("Starting")
        );
        session.progress(second, phase("impact"));
        assert_eq!(
            session.result.progress().map(|p| p.label()).as_deref(),
            Some("Tracing impact")
        );
    }

    #[test]
    fn selections_of_vanished_symbols_are_dropped_when_no_graph_is_known() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        session.result_arrived(run, analysis(&["a", "b"]), None, SECOND, true);
        session.select(Selection::symbol(symbol("a")));
        session.select(Selection::symbol(symbol("b")));
        session.result_arrived(run, analysis(&["a"]), None, SECOND, true);
        assert_eq!(selected(&session), Some("a".to_owned()));
    }

    #[test]
    fn a_result_selects_the_first_item_to_edit_then_to_behave_then_a_change() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        let items = vec![
            item("behaves", ImpactClass::Behaviour, false),
            item("edited", ImpactClass::Adaptation, true),
            item("adapts", ImpactClass::Adaptation, false),
        ];
        session.result_arrived(run, analysis_of(&["changed"], items), None, SECOND, false);
        assert_eq!(selected(&session), Some("adapts".to_owned()));

        let run = session.choose(Compare::WorkingTree);
        let items = vec![item("behaves", ImpactClass::Behaviour, false)];
        session.result_arrived(run, analysis_of(&["changed"], items), None, SECOND, false);
        assert_eq!(selected(&session), Some("behaves".to_owned()));

        let run = session.choose(Compare::WorkingTree);
        session.result_arrived(run, analysis(&["changed"]), None, SECOND, false);
        assert_eq!(selected(&session), Some("changed".to_owned()));
        assert_eq!(default_selection(&analysis(&[])), None);
    }

    #[test]
    fn a_kept_selection_is_not_replaced_but_a_vanished_one_is() {
        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        let items = || vec![item("adapts", ImpactClass::Adaptation, false)];
        session.result_arrived(run, analysis_of(&["changed"], items()), None, SECOND, true);
        session.select(Selection::symbol(symbol("changed")));
        session.result_arrived(run, analysis_of(&["changed"], items()), None, SECOND, true);
        assert_eq!(selected(&session), Some("changed".to_owned()));

        let mut session = opened(true);
        let run = session.choose(Compare::WorkingTree);
        session.result_arrived(run, analysis_of(&["changed"], items()), None, SECOND, true);
        session.select(Selection::symbol(symbol("gone")));
        session.result_arrived(run, analysis_of(&["changed"], items()), None, SECOND, true);
        assert_eq!(selected(&session), Some("adapts".to_owned()));
    }

    #[test]
    fn closing_the_repository_cancels_everything() {
        let mut session = opened(true);
        session.choose(Compare::WorkingTree);
        session.begin_what_if(symbol("a"), Assume::Body);
        session.close_repository();
        assert_eq!(session.take_cancelled().len(), 2);
        assert_eq!(session.repo, None);
        assert_eq!(session.compare, None);
    }
}
