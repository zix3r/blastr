//! The open repository, what is being compared and what came back.
//!
//! Everything a view shows is read from [`Session`]. Its methods are the
//! state transitions of the window: opening a repository, choosing a
//! comparison, a result arriving, a task being cancelled, a failure. None of
//! them does any work or touches the screen, so they are tested as plain
//! functions.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use blastr_model::{
    Analysis, AnalysisOptions, AnalysisParams, Assume, DiffResult, ExecMode, ImpactClass, RevSpec,
    RevisionGraph, SymbolPath,
};

use super::loadable::{Failure, Loadable, Progress};
use super::selection::{History, Selection};
use super::tasks::{TaskSlot, Ticket};

/// A commit named by its full and its abbreviated id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRef {
    /// Full object id, in hexadecimal.
    pub id: String,
    /// Abbreviated object id.
    pub short: String,
}

/// The repository the window is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    /// The root of the working tree.
    pub root: PathBuf,
    /// The directory name, shown as the repository's name.
    pub name: String,
    /// The branch `HEAD` is on; `None` when it is detached.
    pub branch: Option<String>,
    /// The commit `HEAD` points to; `None` in a repository without commits.
    pub head: Option<CommitRef>,
    /// Whether something tracked is modified or staged.
    pub dirty: bool,
}

/// What is being compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compare {
    /// `HEAD` against the working tree, kept up to date as files are saved.
    WorkingTree,
    /// One commit against its first parent.
    Commit(CommitRef),
    /// A run of commits, from the oldest selected to the newest.
    Range {
        /// The oldest selected commit.
        oldest: CommitRef,
        /// The newest selected commit.
        newest: CommitRef,
        /// Whether the oldest commit's own changes are part of the range: the
        /// comparison then starts at its first parent. False for a commit
        /// without a parent.
        from_parent: bool,
    },
}

impl Compare {
    /// What the engine is asked to compare.
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

    /// The two sides as they are named before a result says more.
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

    /// Whether the newer side is the working tree, which is watched for saves.
    pub fn watches(&self) -> bool {
        matches!(self, Compare::WorkingTree)
    }
}

/// A result together with what the views need next to it.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The analysis.
    pub analysis: Arc<Analysis>,
    /// The graph of the newer side, for symbol search; absent when it could
    /// not be read.
    pub graph: Option<Arc<RevisionGraph>>,
    /// How long the engine took.
    pub elapsed: Duration,
    /// Counts results of this comparison: 1 for the first, more after saves.
    pub revision: u64,
}

impl PartialEq for Outcome {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.analysis, &other.analysis) && self.revision == other.revision
    }
}

/// A hypothetical change and what it would reach.
#[derive(Debug, Clone, PartialEq)]
pub struct WhatIf {
    /// The symbol assumed to change.
    pub symbol: SymbolPath,
    /// The change assumed.
    pub assume: Assume,
    /// The hypothetical result.
    pub result: Loadable<Outcome>,
}

/// How a comparison ended when it did not produce a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stopped {
    /// It is running, finished, or was never started.
    #[default]
    No,
    /// The user cancelled it.
    Cancelled,
}

/// The state of the window, without anything that draws.
#[derive(Debug, Default)]
pub struct Session {
    /// The open repository.
    pub repo: Option<RepoInfo>,
    /// The configuration the project is analysed under.
    pub options: AnalysisOptions,
    /// The parameters of the impact analysis.
    pub params: AnalysisParams,
    /// What is being compared; `None` until a repository is open.
    pub compare: Option<Compare>,
    /// The classified changes, which arrive before the impact.
    pub changes: Option<Arc<DiffResult>>,
    /// The result of the comparison.
    pub result: Loadable<Outcome>,
    /// Whether the comparison was cancelled by the user.
    pub stopped: Stopped,
    /// A newer result is being computed after a save; the old one stays shown.
    pub refreshing: Option<Progress>,
    /// The hypothetical change being explored, if any.
    pub what_if: Option<WhatIf>,
    /// The selection and its history.
    pub history: History,
    /// The comparison job.
    pub run: TaskSlot,
    /// The what-if job.
    pub what_if_run: TaskSlot,
}

impl Session {
    /// Opens a repository: forgets everything about the previous one.
    ///
    /// Nothing is compared yet; the caller sets the options the project is
    /// analysed under and chooses a comparison next.
    pub fn open_repository(&mut self, repo: RepoInfo) {
        self.close_repository();
        self.repo = Some(repo);
    }

    /// Closes the repository and cancels whatever was running for it.
    pub fn close_repository(&mut self) {
        self.run.cancel();
        self.what_if_run.cancel();
        *self = Session::default();
    }

    /// The comparison a freshly opened repository starts with: the working
    /// tree when it has uncommitted changes, else the newest commit.
    pub fn default_compare(&self) -> Option<Compare> {
        let repo = self.repo.as_ref()?;
        match &repo.head {
            Some(head) if !repo.dirty => Some(Compare::Commit(head.clone())),
            _ => Some(Compare::WorkingTree),
        }
    }

    /// Whether build scripts and procedural macros run.
    pub fn exec(&self) -> ExecMode {
        self.options.exec
    }

    /// Starts a comparison, cancelling the one that was running.
    ///
    /// The previous result, the selection and any hypothetical change belong
    /// to the previous comparison and are dropped.
    pub fn choose(&mut self, compare: Compare) -> Ticket {
        self.compare = Some(compare);
        self.changes = None;
        self.result = Loadable::Loading(Progress::phase(""));
        self.stopped = Stopped::No;
        self.refreshing = None;
        self.what_if_run.cancel();
        self.what_if = None;
        self.history.clear();
        self.run.begin()
    }

    /// Runs the current comparison again, keeping the selection.
    pub fn retry(&mut self) -> Option<Ticket> {
        let compare = self.compare.clone()?;
        let history = std::mem::take(&mut self.history);
        let ticket = self.choose(compare);
        self.history = history;
        Some(ticket)
    }

    /// Records progress of the comparison job. Stale reports are ignored.
    pub fn progress(&mut self, generation: u64, progress: Progress) {
        if !self.run.is_current(generation) {
            return;
        }
        match &mut self.result {
            Loadable::Loading(current) => *current = progress,
            Loadable::Ready(_) if self.refreshing.is_some() => {
                self.refreshing = Some(progress);
            }
            _ => {}
        }
    }

    /// The classified changes arrived; the impact is still being traced.
    pub fn changes_arrived(&mut self, generation: u64, diff: Arc<DiffResult>) -> bool {
        if !self.run.is_current(generation) {
            return false;
        }
        self.changes = Some(diff);
        true
    }

    /// A result arrived.
    ///
    /// `watching` says the job stays alive to follow saved files; otherwise it
    /// is finished. Returns whether the result was accepted.
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
        let revision = self
            .result
            .ready()
            .map_or(1, |outcome| outcome.revision + 1);
        let graph = graph.or_else(|| {
            self.result
                .ready()
                .and_then(|outcome| outcome.graph.clone())
        });
        self.changes = Some(Arc::new(analysis.diff.clone()));
        self.result = Loadable::Ready(Outcome {
            analysis,
            graph,
            elapsed,
            revision,
        });
        self.refreshing = None;
        self.stopped = Stopped::No;
        if !watching {
            self.run.finish(generation);
        }
        self.drop_stale_selections();
        self.select_something();
        true
    }

    /// The watching job noticed a save and is computing a newer result.
    pub fn refresh_started(&mut self, generation: u64) {
        if self.run.is_current(generation) && self.result.ready().is_some() {
            self.refreshing = Some(Progress::default());
        }
    }

    /// The comparison job failed.
    ///
    /// A failure while refreshing keeps the last good result on screen, the
    /// way a watching session keeps going when a saved file does not parse.
    pub fn failed(&mut self, generation: u64, failure: Failure, watching: bool) -> bool {
        if !self.run.is_current(generation) {
            return false;
        }
        if watching && self.result.ready().is_some() {
            self.refreshing = None;
            return true;
        }
        self.result = Loadable::Failed(failure);
        self.refreshing = None;
        self.run.finish(generation);
        true
    }

    /// The user cancels the running comparison. Returns whether one was
    /// still computing.
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
        self.stopped = Stopped::Cancelled;
        true
    }

    /// Whether the comparison job is computing something right now.
    pub fn is_busy(&self) -> bool {
        self.result.is_loading()
            || self.refreshing.is_some()
            || self
                .what_if
                .as_ref()
                .is_some_and(|what_if| what_if.result.is_loading())
    }

    /// Whether the working tree is being watched with an up-to-date result.
    pub fn is_watching(&self) -> bool {
        self.run.is_running() && self.result.ready().is_some()
    }

    /// Starts a what-if query on `symbol`.
    pub fn begin_what_if(&mut self, symbol: SymbolPath, assume: Assume) -> Ticket {
        self.what_if = Some(WhatIf {
            symbol,
            assume,
            result: Loadable::Loading(Progress::phase("")),
        });
        self.what_if_run.begin()
    }

    /// Records progress of the what-if job.
    pub fn what_if_progress(&mut self, generation: u64, progress: Progress) {
        if !self.what_if_run.is_current(generation) {
            return;
        }
        if let Some(what_if) = &mut self.what_if
            && let Loadable::Loading(current) = &mut what_if.result
        {
            *current = progress;
        }
    }

    /// The hypothetical result arrived.
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

    /// The what-if job failed.
    pub fn what_if_failed(&mut self, generation: u64, failure: Failure) -> bool {
        if !self.what_if_run.finish(generation) {
            return false;
        }
        if let Some(what_if) = &mut self.what_if {
            what_if.result = Loadable::Failed(failure);
        }
        true
    }

    /// Returns from the hypothetical change to the real comparison.
    pub fn leave_what_if(&mut self) {
        self.what_if_run.cancel();
        if self.what_if.take().is_some() {
            self.drop_stale_selections();
        }
    }

    /// Whether the views show a hypothetical result.
    pub fn is_hypothetical(&self) -> bool {
        self.what_if.is_some()
    }

    /// What the views show: the hypothetical result while one is explored,
    /// else the result of the comparison.
    pub fn shown(&self) -> &Loadable<Outcome> {
        match &self.what_if {
            Some(what_if) => &what_if.result,
            None => &self.result,
        }
    }

    /// The analysis the views show, when it has arrived.
    pub fn analysis(&self) -> Option<&Arc<Analysis>> {
        self.shown().ready().map(|outcome| &outcome.analysis)
    }

    /// The changes the views show: those of the shown analysis, or the ones
    /// that arrived ahead of it.
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

    /// Selects a symbol.
    pub fn select(&mut self, selection: Selection) {
        self.history.select(selection);
    }

    /// Selects the first item worth reading when nothing is selected.
    ///
    /// A result that arrives with an empty selection leaves the code view and
    /// the evidence panel blank, which reads as a bug. The window therefore
    /// falls back to what most needs attention: an item that needs editing,
    /// then one that may behave differently, then the change itself.
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
        if keep {
            return;
        }
        let next = default_selection(&outcome.analysis);
        if let Some(selection) = next {
            self.select(selection);
        }
    }

    /// Forgets selections of symbols the shown result no longer mentions.
    ///
    /// Symbols of the newer side's graph stay selectable even when nothing
    /// reaches them, so that a search result can be looked at.
    fn drop_stale_selections(&mut self) {
        let Some(outcome) = self.shown().ready() else {
            return;
        };
        if outcome.graph.is_some() {
            // Any symbol of the project may be selected through the search.
            return;
        }
        let analysis = Arc::clone(&outcome.analysis);
        self.history.retain(|selection| {
            analysis
                .items
                .iter()
                .any(|item| item.symbol == selection.symbol)
                || analysis
                    .diff
                    .changes
                    .iter()
                    .any(|change| change.symbol == selection.symbol)
        });
    }
}

/// Whether the shown result still mentions `symbol`: as an item, as a change,
/// or anywhere in the newer side's graph.
fn selection_exists(
    analysis: &Analysis,
    graph: Option<&RevisionGraph>,
    symbol: &SymbolPath,
) -> bool {
    if analysis.items.iter().any(|item| &item.symbol == symbol)
        || analysis
            .diff
            .changes
            .iter()
            .any(|change| &change.symbol == symbol)
    {
        return true;
    }
    graph.is_some_and(|graph| graph.symbols.iter().any(|entry| &entry.path == symbol))
}

/// The selection to make when a result arrives with nothing selected.
///
/// The first item that still needs editing comes first, then the first that
/// may behave differently, then the first change: the answer the window can
/// act on before anything else.
fn default_selection(analysis: &Analysis) -> Option<Selection> {
    let open = analysis
        .items
        .iter()
        .find(|item| item.class == ImpactClass::Adaptation && !item.tags.changed);
    if let Some(item) = open {
        return Some(Selection::symbol(item.symbol.clone()));
    }
    let behaving = analysis
        .items
        .iter()
        .find(|item| item.class == ImpactClass::Behaviour);
    if let Some(item) = behaving {
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
        AnalysisParams, Change, ChangeDetail, ChangeKind, Comparison, ImpactClass, ImpactItem,
        ImpactLimits, ImpactTags, Location, RelPath, Source, Span, SymbolKind, TestSelection,
    };

    use super::*;

    fn repo(dirty: bool) -> RepoInfo {
        RepoInfo {
            root: PathBuf::from("/code/invoicing"),
            name: "invoicing".to_owned(),
            branch: Some("main".to_owned()),
            head: Some(commit("a3f2e1c")),
            dirty,
        }
    }

    fn commit(short: &str) -> CommitRef {
        CommitRef {
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

    fn analysis(names: &[&str]) -> Arc<Analysis> {
        Arc::new(Analysis {
            diff: diff(names),
            items: Vec::new(),
            tests: TestSelection::default(),
            limits: ImpactLimits::default(),
            params: AnalysisParams::default(),
        })
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
            score: 0.5,
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

    fn opened(dirty: bool) -> Session {
        let mut session = Session::default();
        session.open_repository(repo(dirty));
        session
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_dirty_repository_starts_on_the_working_tree() {
        assert_eq!(opened(true).default_compare(), Some(Compare::WorkingTree));
    }

    #[test]
    fn a_clean_repository_starts_on_its_newest_commit() {
        assert_eq!(
            opened(false).default_compare(),
            Some(Compare::Commit(commit("a3f2e1c")))
        );
    }

    #[test]
    fn a_repository_without_commits_starts_on_the_working_tree() {
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
        assert_eq!(
            Compare::Commit(commit("abc1234")).spec().rev.as_deref(),
            Some("abc1234000000000000000000000000000000000")
        );
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
        let from_root = Compare::Range {
            oldest: commit("aaaaaaa"),
            newest: commit("bbbbbbb"),
            from_parent: false,
        };
        assert_eq!(
            from_root.labels(),
            ("aaaaaaa".to_owned(), "bbbbbbb".to_owned())
        );
        assert!(Compare::WorkingTree.watches());
        assert!(!range.watches());
    }

    #[test]
    fn choosing_a_comparison_starts_loading() {
        let mut session = opened(true);

        let ticket = session.choose(Compare::WorkingTree);

        assert!(session.result.is_loading());
        assert!(session.is_busy());
        assert!(session.run.is_current(ticket.generation));
        assert_eq!(session.shown_changes(), None);
    }

    #[test]
    fn changes_arrive_before_the_result() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);

        assert!(session.changes_arrived(ticket.generation, Arc::new(diff(&["vat_rate"]))));

        assert!(session.result.is_loading());
        assert_eq!(session.shown_changes().map(|d| d.changes.len()), Some(1));
        assert_eq!(session.analysis(), None);
    }

    #[test]
    fn a_result_of_a_one_shot_comparison_finishes_the_job() {
        let mut session = opened(false);
        let ticket = session.choose(Compare::Commit(commit("a3f2e1c")));

        assert!(session.result_arrived(
            ticket.generation,
            analysis(&["vat_rate"]),
            None,
            SECOND,
            false
        ));

        assert!(session.analysis().is_some());
        assert!(!session.run.is_running());
        assert!(!session.is_busy());
        assert!(!session.is_watching());
    }

    #[test]
    fn a_watched_comparison_stays_alive_and_counts_its_results() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        session.result_arrived(ticket.generation, analysis(&["a"]), None, SECOND, true);
        assert!(session.is_watching());
        assert_eq!(session.result.ready().map(|o| o.revision), Some(1));

        session.refresh_started(ticket.generation);
        assert!(session.is_busy());
        assert!(session.analysis().is_some(), "the old result stays shown");

        session.result_arrived(ticket.generation, analysis(&["a", "b"]), None, SECOND, true);

        assert!(!session.is_busy());
        assert_eq!(session.result.ready().map(|o| o.revision), Some(2));
        assert_eq!(session.shown_changes().map(|d| d.changes.len()), Some(2));
    }

    #[test]
    fn a_new_comparison_cancels_the_previous_one_and_ignores_its_result() {
        let mut session = opened(true);
        let first = session.choose(Compare::WorkingTree);
        let second = session.choose(Compare::Commit(commit("7d9c4b2")));

        assert!(first.cancel.is_cancelled());
        assert!(!session.changes_arrived(first.generation, Arc::new(diff(&["old"]))));
        assert!(!session.result_arrived(first.generation, analysis(&["old"]), None, SECOND, true));
        assert!(!session.failed(first.generation, Failure::new("late"), false));

        assert!(session.result.is_loading());
        assert!(session.run.is_current(second.generation));
    }

    #[test]
    fn a_failure_ends_the_comparison() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);

        assert!(session.failed(ticket.generation, Failure::new("cargo failed"), true));

        assert_eq!(
            session.result.failure().map(|f| f.message.as_str()),
            Some("cargo failed")
        );
        assert!(!session.run.is_running());
    }

    #[test]
    fn a_failed_refresh_keeps_the_last_good_result() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        session.result_arrived(ticket.generation, analysis(&["a"]), None, SECOND, true);
        session.refresh_started(ticket.generation);

        session.failed(ticket.generation, Failure::new("does not parse"), true);

        assert!(session.analysis().is_some());
        assert!(session.is_watching());
        assert!(!session.is_busy());
    }

    #[test]
    fn cancelling_a_running_comparison_leaves_it_stopped() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);

        assert!(session.cancel());

        assert!(ticket.cancel.is_cancelled());
        assert_eq!(session.stopped, Stopped::Cancelled);
        assert_eq!(session.result, Loadable::Empty);
        assert!(!session.cancel(), "nothing is left to cancel");
        assert!(!session.result_arrived(ticket.generation, analysis(&[]), None, SECOND, true));
    }

    #[test]
    fn cancel_does_not_stop_a_finished_watch() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        session.result_arrived(ticket.generation, analysis(&["a"]), None, SECOND, true);

        assert!(!session.cancel());

        assert!(session.is_watching());
        assert!(!ticket.cancel.is_cancelled());
    }

    #[test]
    fn retry_runs_the_same_comparison_and_keeps_the_selection() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        session.select(Selection::symbol(symbol("a")));
        session.failed(ticket.generation, Failure::new("no"), false);

        let again = session.retry().expect("a comparison to retry");

        assert!(session.result.is_loading());
        assert_ne!(again.generation, ticket.generation);
        assert_eq!(session.compare, Some(Compare::WorkingTree));
        assert!(session.selection().is_some());
        assert!(Session::default().retry().is_none());
    }

    #[test]
    fn a_what_if_result_replaces_the_shown_result_until_it_is_left() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        session.result_arrived(ticket.generation, analysis(&["real"]), None, SECOND, true);

        let what_if = session.begin_what_if(symbol("total"), Assume::Signature);
        assert!(session.is_hypothetical());
        assert!(session.shown().is_loading());
        assert_eq!(session.shown_changes(), None);

        assert!(session.what_if_arrived(what_if.generation, analysis(&["total"]), SECOND));
        assert_eq!(
            session
                .shown_changes()
                .map(|d| d.changes[0].symbol.name().to_owned()),
            Some("total".to_owned())
        );

        session.leave_what_if();

        assert!(!session.is_hypothetical());
        assert_eq!(
            session
                .shown_changes()
                .map(|d| d.changes[0].symbol.name().to_owned()),
            Some("real".to_owned())
        );
        assert!(session.is_watching(), "the real comparison kept running");
    }

    #[test]
    fn a_second_what_if_cancels_the_first() {
        let mut session = opened(true);
        let first = session.begin_what_if(symbol("a"), Assume::Body);
        let second = session.begin_what_if(symbol("b"), Assume::Body);

        assert!(first.cancel.is_cancelled());
        assert!(!session.what_if_arrived(first.generation, analysis(&["a"]), SECOND));
        assert!(session.what_if_failed(second.generation, Failure::new("ambiguous")));
        assert!(session.shown().failure().is_some());
    }

    #[test]
    fn cancel_leaves_a_running_what_if_first() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        session.result_arrived(ticket.generation, analysis(&["real"]), None, SECOND, true);
        let what_if = session.begin_what_if(symbol("a"), Assume::Body);

        assert!(session.cancel());

        assert!(what_if.cancel.is_cancelled());
        assert!(!session.is_hypothetical());
        assert!(session.analysis().is_some());
    }

    #[test]
    fn choosing_a_comparison_leaves_the_what_if() {
        let mut session = opened(true);
        let what_if = session.begin_what_if(symbol("a"), Assume::Body);

        session.choose(Compare::WorkingTree);

        assert!(what_if.cancel.is_cancelled());
        assert!(!session.is_hypothetical());
    }

    #[test]
    fn stale_progress_is_ignored() {
        let mut session = opened(true);
        let first = session.choose(Compare::WorkingTree);
        let second = session.choose(Compare::WorkingTree);

        session.progress(first.generation, Progress::phase("impact"));
        assert_eq!(
            session.result.progress().map(|p| p.phase.as_str()),
            Some("")
        );

        session.progress(second.generation, Progress::phase("impact"));
        assert_eq!(
            session.result.progress().map(|p| p.phase.as_str()),
            Some("impact")
        );
    }

    #[test]
    fn selections_of_vanished_symbols_are_dropped_when_no_graph_is_known() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        session.result_arrived(ticket.generation, analysis(&["a", "b"]), None, SECOND, true);
        session.select(Selection::symbol(symbol("a")));
        session.select(Selection::symbol(symbol("b")));

        session.result_arrived(ticket.generation, analysis(&["a"]), None, SECOND, true);

        assert_eq!(
            session.selection().map(|s| s.symbol.name().to_owned()),
            Some("a".to_owned())
        );
    }

    #[test]
    fn a_result_with_nothing_selected_selects_the_first_item_to_edit() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);

        session.result_arrived(
            ticket.generation,
            analysis_of(
                &["changed"],
                vec![
                    item("behaves", ImpactClass::Behaviour, false),
                    item("adapts", ImpactClass::Adaptation, false),
                    item("also", ImpactClass::Adaptation, false),
                ],
            ),
            None,
            SECOND,
            true,
        );

        assert_eq!(
            session.selection().map(|s| s.symbol.name().to_owned()),
            Some("adapts".to_owned())
        );
    }

    #[test]
    fn a_result_with_only_behaving_items_selects_one_of_them() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);

        session.result_arrived(
            ticket.generation,
            analysis_of(
                &["changed"],
                vec![item("behaves", ImpactClass::Behaviour, false)],
            ),
            None,
            SECOND,
            true,
        );

        assert_eq!(
            session.selection().map(|s| s.symbol.name().to_owned()),
            Some("behaves".to_owned())
        );
    }

    #[test]
    fn a_result_with_nothing_to_read_selects_the_first_change() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);

        session.result_arrived(
            ticket.generation,
            analysis_of(&["changed"], Vec::new()),
            None,
            SECOND,
            true,
        );

        assert_eq!(
            session.selection().map(|s| s.symbol.name().to_owned()),
            Some("changed".to_owned())
        );
    }

    #[test]
    fn a_kept_selection_is_not_replaced() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        session.result_arrived(
            ticket.generation,
            analysis_of(
                &["changed"],
                vec![item("adapts", ImpactClass::Adaptation, false)],
            ),
            None,
            SECOND,
            true,
        );
        session.select(Selection::symbol(symbol("changed")));

        session.result_arrived(
            ticket.generation,
            analysis_of(
                &["changed"],
                vec![
                    item("first", ImpactClass::Adaptation, false),
                    item("changed", ImpactClass::Adaptation, false),
                ],
            ),
            None,
            SECOND,
            true,
        );

        assert_eq!(
            session.selection().map(|s| s.symbol.name().to_owned()),
            Some("changed".to_owned())
        );
    }

    #[test]
    fn an_empty_result_selects_nothing() {
        assert_eq!(default_selection(&analysis_of(&[], Vec::new())), None);
    }

    fn graph_with(paths: &[&str]) -> RevisionGraph {
        let mut graph = RevisionGraph::default();
        for path in paths {
            graph.symbols.push(blastr_model::Symbol {
                path: SymbolPath::new(*path, SymbolKind::Function),
                krate: blastr_model::CrateId(0),
                location: Location {
                    file: RelPath::new("src/lib.rs"),
                    span: Span {
                        start: 0,
                        end: 1,
                        start_line: 1,
                        end_line: 1,
                    },
                },
                visibility: blastr_model::Visibility::Public,
                signature_hash: 0,
                body_hash: 0,
                attr_hash: 0,
                body_sketch: Vec::new(),
                parent: None,
                flags: blastr_model::SymbolFlags::default(),
                impl_info: None,
            });
        }
        graph
    }

    #[test]
    fn a_selection_exists_while_the_result_mentions_it() {
        let analysis = analysis_of(
            &["changed"],
            vec![item("adapts", ImpactClass::Adaptation, false)],
        );

        assert!(selection_exists(&analysis, None, &symbol("adapts")));
        assert!(selection_exists(&analysis, None, &symbol("changed")));
        assert!(!selection_exists(&analysis, None, &symbol("gone")));
        assert!(selection_exists(
            &analysis,
            Some(&graph_with(&["billing::gone"])),
            &symbol("gone")
        ));
    }

    #[test]
    fn a_selection_of_a_vanished_symbol_is_replaced() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        session.result_arrived(
            ticket.generation,
            analysis_of(
                &["changed"],
                vec![item("adapts", ImpactClass::Adaptation, false)],
            ),
            None,
            SECOND,
            true,
        );
        session.select(Selection::symbol(symbol("gone")));

        session.result_arrived(
            ticket.generation,
            analysis_of(
                &["changed"],
                vec![item("adapts", ImpactClass::Adaptation, false)],
            ),
            None,
            SECOND,
            true,
        );

        assert_eq!(
            session.selection().map(|s| s.symbol.name().to_owned()),
            Some("adapts".to_owned())
        );
    }

    #[test]
    fn closing_the_repository_cancels_everything() {
        let mut session = opened(true);
        let ticket = session.choose(Compare::WorkingTree);
        let what_if = session.begin_what_if(symbol("a"), Assume::Body);

        session.close_repository();

        assert!(ticket.cancel.is_cancelled());
        assert!(what_if.cancel.is_cancelled());
        assert_eq!(session.repo, None);
        assert_eq!(session.compare, None);
    }
}
