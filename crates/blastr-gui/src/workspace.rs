//! The state every view of the window shares: the session, the settings, the
//! rows of the lists and the connection to the engine.
//!
//! The views observe this entity and redraw when it notifies. Every change is
//! made by one of its methods, which rebuilds what depends on it at once.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use blastr_model::protocol::{Failure, JobId, JobKind, Request, Response};
use blastr_model::{
    Assume, Change, CommitInfo, Confidence, ImpactItem, Location, Source, SymbolPath,
};
use gpui_kit::{ClipboardItem, Context, EventEmitter, Task};

use crate::engine::{self, Engine, Incoming};
use crate::settings::Settings;
use crate::state::outline::{Block, Filter, Outline, TestRow, test_rows};
use crate::state::search::SymbolIndex;
use crate::state::selection::Selection;
use crate::state::session::{Compare, Session};

/// How long the engine has to stop a cancelled job before it is replaced.
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// How long the settings wait for more changes before they are written.
const SAVE_DELAY: Duration = Duration::from_millis(600);

/// How many commits the compare menu lists at a time.
const COMMIT_PAGE: usize = 200;

/// What the workspace asks the window to show.
pub(crate) enum WorkspaceEvent {
    /// A short message.
    Notice(String),
    /// Something went wrong that the user should know of.
    Problem(String),
    /// A repository opens for the first time: ask before its build scripts run.
    TrustNeeded,
}

/// The commits of the open repository, newest first.
#[derive(Debug, Default)]
pub(crate) struct Commits {
    pub list: Vec<CommitInfo>,
    pub loading: bool,
    pub exhausted: bool,
    pub failure: Option<String>,
}

/// The shared state of the window.
pub(crate) struct Workspace {
    pub session: Session,
    pub settings: Settings,
    settings_path: PathBuf,
    engine: Engine,
    /// The last request of each kind of job, sent again when the engine has
    /// to be replaced while it runs.
    jobs: Vec<Request>,
    /// The folder being opened.
    opening: Option<PathBuf>,
    pub open_error: Option<String>,
    /// The repository whose trust question is open.
    pub pending_trust: Option<PathBuf>,
    /// The comparison to run again once the repository is opened again.
    restore: Option<Compare>,
    /// Cancelled jobs the engine has not acknowledged yet.
    cancelling: HashSet<JobId>,
    pub search: SymbolIndex,
    pub filter: Filter,
    pub outline: Outline,
    pub tests: Vec<TestRow>,
    tests_expanded: Vec<usize>,
    pub commits: Commits,
    /// Whether a setting the analysis depends on changed since it ran.
    pub rerun_needed: bool,
    /// The change whose propagation the graph shows.
    focus: Option<usize>,
    save: Task<()>,
    _receive: Task<()>,
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

impl Workspace {
    pub(crate) fn new(cx: &mut Context<Self>) -> Workspace {
        let settings_path = Settings::path();
        let settings = Settings::load(&settings_path);
        let (engine, answers) = Engine::new();
        let receive = cx.spawn(async move |this, cx| {
            while let Ok(incoming) = answers.recv().await {
                if this
                    .update(cx, |this, cx| this.receive(incoming, cx))
                    .is_err()
                {
                    return;
                }
            }
        });
        if settings.editor.is_empty() {
            let found = cx
                .background_executor()
                .spawn(async { crate::editor::detect() });
            cx.spawn(async move |this, cx| {
                if let Some(command) = found.await {
                    this.update(cx, |this, cx| {
                        this.update_settings(cx, |settings| settings.editor = command);
                    })
                    .ok();
                }
            })
            .detach();
        }
        let filter = Filter {
            min_confidence: settings.analysis.min_confidence,
            ..Filter::default()
        };
        Workspace {
            session: Session::default(),
            settings,
            settings_path,
            engine,
            jobs: Vec::new(),
            opening: None,
            open_error: None,
            pending_trust: None,
            restore: None,
            cancelling: HashSet::new(),
            search: SymbolIndex::default(),
            filter,
            outline: Outline::default(),
            tests: Vec::new(),
            tests_expanded: Vec::new(),
            commits: Commits::default(),
            rerun_needed: false,
            focus: None,
            save: Task::ready(()),
            _receive: receive,
        }
    }

    /// Whether a repository is being opened.
    pub(crate) fn is_opening(&self) -> bool {
        self.opening.is_some()
    }

    /// The root of the open repository.
    pub(crate) fn root(&self) -> Option<&Path> {
        self.session.repo.as_ref().map(|repo| repo.root.as_path())
    }

    // -- the engine --------------------------------------------------------

    /// Sends a request, reporting a failure to start the engine against the
    /// job the request belongs to.
    fn send(&mut self, request: Request, cx: &mut Context<Self>) {
        if let Some(id) = request.job() {
            self.jobs
                .retain(|known| known.job().map(|job| job.kind) != Some(id.kind));
            self.jobs.push(request.clone());
        }
        if let Err(message) = self.engine.send(&request) {
            self.engine_failed(Failure::new(message), cx);
        }
    }

    /// Tells the engine about the jobs the session stopped, and replaces it if
    /// it does not acknowledge them in time.
    fn flush_cancels(&mut self, cx: &mut Context<Self>) {
        let cancelled = self.session.take_cancelled();
        let Some(number) = self.engine.number() else {
            return;
        };
        if cancelled.is_empty() {
            return;
        }
        for &id in &cancelled {
            let _ = self.engine.send(&Request::Cancel { id });
            self.cancelling.insert(id);
        }
        let deadline = cx.background_executor().timer(CANCEL_GRACE);
        cx.spawn(async move |this, cx| {
            deadline.await;
            this.update(cx, |this, cx| {
                let mut pending = false;
                for id in &cancelled {
                    pending |= this.cancelling.remove(id);
                }
                // An engine that started since has nothing to acknowledge.
                if pending && this.engine.is_current(number) {
                    this.engine.kill();
                    this.resend(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Sends the jobs that still run to a new engine.
    fn resend(&mut self, cx: &mut Context<Self>) {
        let current: Vec<Request> = std::mem::take(&mut self.jobs)
            .into_iter()
            .filter(|request| request.job().is_some_and(|id| self.is_current(id)))
            .collect();
        if let Some(path) = self.opening.clone() {
            self.send(Request::Open { path }, cx);
        }
        for request in current {
            self.send(request, cx);
        }
    }

    fn is_current(&self, id: JobId) -> bool {
        match id.kind {
            JobKind::Compare => self.session.run.is_current(id.generation),
            JobKind::WhatIf => self.session.what_if_run.is_current(id.generation),
        }
    }

    /// The engine stopped or could not be started: every job that runs fails.
    fn engine_failed(&mut self, failure: Failure, cx: &mut Context<Self>) {
        let run = self.session.run.generation();
        self.session.failed(run, failure.clone(), false);
        let what_if = self.session.what_if_run.generation();
        self.session.what_if_failed(what_if, failure.clone());
        if self.opening.take().is_some() {
            self.open_error = Some(failure.message);
        }
        self.jobs.clear();
        self.rebuild_rows();
        cx.notify();
    }

    /// Handles what the engine sent.
    fn receive(&mut self, incoming: Incoming, cx: &mut Context<Self>) {
        let response = match incoming {
            Incoming::Response(response) => *response,
            Incoming::Stopped(number) => {
                if self.engine.is_current(number) {
                    let detail = self.engine.stopped();
                    let failure = Failure {
                        message: "The analysis engine stopped.".to_owned(),
                        hint: (!detail.is_empty()).then_some(detail),
                    };
                    self.engine_failed(failure, cx);
                }
                return;
            }
        };
        match response {
            Response::Hello { protocol, build } => {
                if let Some(problem) = engine::mismatch(protocol, &build) {
                    self.engine.kill();
                    let message = format!("The analysis engine cannot be used: {problem}.");
                    self.engine_failed(Failure::new(message), cx);
                }
            }
            Response::Opened { result } => {
                self.opening = None;
                match result {
                    Ok(opened) => self.opened(opened.repository, cx),
                    Err(failure) => self.open_error = Some(failure.message),
                }
            }
            Response::Progress { id, report } => match id.kind {
                JobKind::Compare => self.session.progress(id.generation, report.into()),
                JobKind::WhatIf => self.session.what_if_progress(id.generation, report.into()),
            },
            Response::Changes { id, diff } => {
                self.session.changes_arrived(id.generation, Arc::new(diff));
                self.rebuild_rows();
            }
            Response::Result {
                id,
                analysis,
                graph,
                elapsed_ms,
            } => {
                let watching = self.session.options.watch
                    && self.session.compare.as_ref().is_some_and(Compare::watches);
                let graph = graph.map(|graph| Arc::new(*graph));
                if let Some(graph) = &graph {
                    self.search = SymbolIndex::from_graph(graph);
                }
                self.session.result_arrived(
                    id.generation,
                    Arc::new(*analysis),
                    graph,
                    Duration::from_millis(elapsed_ms),
                    watching,
                );
                self.rebuild_rows();
            }
            Response::Failed {
                id,
                failure,
                watching,
            } => {
                self.session.failed(id.generation, failure, watching);
                self.rebuild_rows();
            }
            Response::Refreshing { id } => self.session.refresh_started(id.generation),
            Response::BaseMoved { id } => {
                if self.session.run.is_current(id.generation)
                    && let Some(root) = self.root().map(Path::to_path_buf)
                {
                    self.restore = self.session.compare.clone();
                    self.open_path(root, cx);
                }
            }
            Response::WhatIfResult {
                id,
                analysis,
                elapsed_ms,
            } => {
                let elapsed = Duration::from_millis(elapsed_ms);
                self.session
                    .what_if_arrived(id.generation, Arc::new(*analysis), elapsed);
                self.rebuild_rows();
            }
            Response::WhatIfFailed { id, failure } => {
                self.session.what_if_failed(id.generation, failure);
                self.rebuild_rows();
            }
            Response::Cancelled { id } => _ = self.cancelling.remove(&id),
            Response::Cache { .. } => {}
        }
        self.refocus();
        cx.notify();
    }

    // -- repositories ------------------------------------------------------

    /// Opens the repository above `path`.
    pub(crate) fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.opening = Some(path.clone());
        self.open_error = None;
        self.send(Request::Open { path }, cx);
        cx.notify();
    }

    /// A repository was opened: remember it and start comparing.
    fn opened(&mut self, repo: blastr_model::protocol::Repository, cx: &mut Context<Self>) {
        let root = repo.root.clone();
        self.settings.opened(&root, repo.branch.clone());
        self.schedule_save(cx);
        self.session.open_repository(repo);
        self.flush_cancels(cx);
        (self.session.options, self.session.params) = self.settings.analysis_for(&root);
        self.search = SymbolIndex::default();
        self.tests_expanded.clear();
        self.commits = Commits::default();
        self.load_commits(cx);
        if self.settings.exec_mode(&root).is_some() {
            self.start_default(cx);
        } else {
            self.pending_trust = Some(root);
            cx.emit(WorkspaceEvent::TrustNeeded);
        }
        self.rebuild_rows();
    }

    /// Runs what was compared before the repository was opened again, or
    /// what a fresh repository compares.
    fn start_default(&mut self, cx: &mut Context<Self>) {
        if let Some(compare) = self
            .restore
            .take()
            .or_else(|| self.session.default_compare())
        {
            self.choose(compare, cx);
        }
    }

    /// Answers the trust question: whether build scripts and procedural
    /// macros may run.
    pub(crate) fn answer_trust(&mut self, restricted: bool, cx: &mut Context<Self>) {
        let Some(root) = self.pending_trust.take() else {
            return;
        };
        self.update_settings(cx, |settings| {
            settings.repository_mut(&root).restricted = Some(restricted);
        });
        (self.session.options, self.session.params) = self.settings.analysis_for(&root);
        self.rerun_needed = false;
        self.start_default(cx);
    }

    /// Reads the next page of commits, off the window's thread.
    pub(crate) fn load_commits(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root().map(Path::to_path_buf) else {
            return;
        };
        if self.commits.loading || self.commits.exhausted {
            return;
        }
        self.commits.loading = true;
        let skip = self.commits.list.len();
        let read = cx.background_executor().spawn(async move {
            let root = blastr_git::discover(&root).map_err(|error| error.to_string())?;
            let repo = blastr_git::Repo::open(&root).map_err(|error| error.to_string())?;
            repo.log(skip, COMMIT_PAGE)
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                let commits = &mut this.commits;
                commits.loading = false;
                match result {
                    Ok(page) if skip == commits.list.len() => {
                        commits.exhausted = page.len() < COMMIT_PAGE;
                        commits.list.extend(page);
                    }
                    Ok(_) => {}
                    Err(message) => commits.failure = Some(message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // -- comparisons -------------------------------------------------------

    /// Starts a comparison.
    pub(crate) fn choose(&mut self, compare: Compare, cx: &mut Context<Self>) {
        let spec = compare.spec();
        let generation = self.session.choose(compare);
        self.start_compare(generation, spec, cx);
    }

    /// Runs the current comparison again, with the settings as they are now.
    pub(crate) fn retry(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root().map(Path::to_path_buf) else {
            return;
        };
        (self.session.options, self.session.params) = self.settings.analysis_for(&root);
        self.rerun_needed = false;
        let Some(spec) = self.session.compare.as_ref().map(Compare::spec) else {
            return;
        };
        if let Some(generation) = self.session.retry() {
            self.start_compare(generation, spec, cx);
        }
    }

    fn start_compare(
        &mut self,
        generation: u64,
        spec: blastr_model::RevSpec,
        cx: &mut Context<Self>,
    ) {
        self.flush_cancels(cx);
        let Some(repo) = self.root().map(Path::to_path_buf) else {
            return;
        };
        let request = Request::Compare {
            id: JobId {
                kind: JobKind::Compare,
                generation,
            },
            repo,
            spec,
            options: self.session.options.clone(),
            params: self.session.params.clone(),
        };
        self.send(request, cx);
        self.rebuild_rows();
        cx.notify();
    }

    /// Cancels what runs; whether something stopped.
    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) -> bool {
        let stopped = self.session.cancel();
        if stopped {
            self.flush_cancels(cx);
            self.rebuild_rows();
            cx.notify();
        }
        stopped
    }

    /// Traces what a change of the selected symbol would reach.
    pub(crate) fn begin_what_if(&mut self, assume: Assume, cx: &mut Context<Self>) {
        let (Some(repo), Some(selection)) = (
            self.root().map(Path::to_path_buf),
            self.session.selection().cloned(),
        ) else {
            return;
        };
        let generation = self.session.begin_what_if(selection.symbol.clone(), assume);
        self.flush_cancels(cx);
        let request = Request::WhatIf {
            id: JobId {
                kind: JobKind::WhatIf,
                generation,
            },
            repo,
            symbol: selection.symbol,
            assume,
            options: self.session.options.clone(),
            params: self.session.params.clone(),
        };
        self.send(request, cx);
        self.rebuild_rows();
        cx.notify();
    }

    /// Goes back from a hypothetical change to the comparison.
    pub(crate) fn leave_what_if(&mut self, cx: &mut Context<Self>) {
        self.session.leave_what_if();
        self.flush_cancels(cx);
        self.rebuild_rows();
        cx.notify();
    }

    // -- the selection -----------------------------------------------------

    /// Selects a symbol, at the reference `site` when one was clicked.
    pub(crate) fn select(
        &mut self,
        symbol: SymbolPath,
        site: Option<Location>,
        cx: &mut Context<Self>,
    ) {
        self.session.select(Selection { symbol, site });
        self.refocus();
        cx.notify();
    }

    /// Goes back or forward in the selection history.
    pub(crate) fn walk_history(&mut self, forward: bool, cx: &mut Context<Self>) {
        let moved = if forward {
            self.session.history.forward().is_some()
        } else {
            self.session.history.back().is_some()
        };
        if moved {
            self.refocus();
            cx.notify();
        }
    }

    /// The impact item the selection names.
    pub(crate) fn selected_item(&self) -> Option<&ImpactItem> {
        let symbol = &self.session.selection()?.symbol;
        let analysis = self.session.analysis()?;
        analysis.items.iter().find(|item| &item.symbol == symbol)
    }

    /// The change the selection names, with its index.
    pub(crate) fn selected_change(&self) -> Option<(usize, &Change)> {
        let symbol = &self.session.selection()?.symbol;
        self.session
            .shown_changes()?
            .changes
            .iter()
            .enumerate()
            .find(|(_, change)| &change.symbol == symbol)
    }

    /// The change whose propagation the graph shows: the selected change, or
    /// the one the selected item's best path starts from.
    pub(crate) fn focus_change(&self) -> Option<usize> {
        self.focus
    }

    /// Follows the selection with the focused change, keeping the one shown
    /// while it still reaches a selected item.
    fn refocus(&mut self) {
        self.focus = match (self.selected_change(), self.selected_item()) {
            (Some((index, _)), _) => Some(index),
            (None, Some(item)) => {
                let reaches =
                    |index: usize| item.paths.iter().any(|path| path.change as usize == index);
                match self.focus {
                    Some(index) if reaches(index) => Some(index),
                    _ => item.paths.first().map(|path| path.change as usize),
                }
            }
            (None, None) => None,
        };
    }

    /// Where the selection is: the revision and the location to read.
    pub(crate) fn selection_location(&self) -> Option<(Source, Location)> {
        let selection = self.session.selection()?;
        let diff = self.session.shown_changes()?;
        let head = diff.comparison.head.clone();
        if let Some(site) = &selection.site {
            return Some((head, site.clone()));
        }
        // A symbol that is both changed and reached is shown as the change.
        if let Some((_, change)) = self.selected_change() {
            return match (&change.new, &change.old) {
                (Some(new), _) => Some((head, new.clone())),
                (None, Some(old)) => Some((diff.comparison.base.clone(), old.clone())),
                (None, None) => None,
            };
        }
        if let Some(item) = self.selected_item() {
            return Some((head, item.location.clone()));
        }
        let entry = self.search.find(&selection.symbol)?;
        Some((head, entry.location.clone()))
    }

    // -- the lists ---------------------------------------------------------

    /// Rebuilds the rows of the lists from the session and the filter.
    fn rebuild_rows(&mut self) {
        self.refocus();
        let analysis = self.session.analysis().cloned();
        let loading = self.session.shown().is_loading();
        self.outline = Outline::build(
            self.session.shown_changes(),
            analysis.as_deref(),
            loading,
            &self.filter,
        );
        self.tests = match &analysis {
            Some(analysis) => test_rows(analysis, &self.filter, &self.tests_expanded),
            None => Vec::new(),
        };
    }

    /// Filters the lists by text.
    pub(crate) fn set_filter_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.filter.text = text;
        self.rebuild_rows();
        cx.notify();
    }

    /// Folds or unfolds a block of the outline.
    pub(crate) fn toggle_block(&mut self, block: Block, cx: &mut Context<Self>) {
        self.filter.toggle(block);
        self.rebuild_rows();
        cx.notify();
    }

    /// Lists only items at or above `confidence`, in the lists and the graph.
    pub(crate) fn set_min_confidence(&mut self, confidence: Confidence, cx: &mut Context<Self>) {
        self.filter.min_confidence = confidence;
        self.update_settings(cx, |settings| settings.analysis.min_confidence = confidence);
        self.rebuild_rows();
    }

    /// Shows or hides the tests of one target.
    pub(crate) fn toggle_test_target(&mut self, command: usize, cx: &mut Context<Self>) {
        if let Some(position) = self
            .tests_expanded
            .iter()
            .position(|known| *known == command)
        {
            self.tests_expanded.remove(position);
        } else {
            self.tests_expanded.push(command);
        }
        self.rebuild_rows();
        cx.notify();
    }

    // -- settings ----------------------------------------------------------

    /// Changes the settings and saves them a moment later. A change that the
    /// analysis depends on asks for the comparison to run again.
    pub(crate) fn update_settings(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Settings),
    ) {
        change(&mut self.settings);
        // Asks to run again while the settings differ from those the
        // comparison on screen ran with.
        let ran = (self.session.options.clone(), self.session.params.clone());
        self.rerun_needed = self.session.compare.is_some()
            && self.pending_trust.is_none()
            && self
                .root()
                .is_some_and(|root| self.settings.analysis_for(root) != ran);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Writes the settings once they stop changing for a moment.
    pub(crate) fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let delay = cx.background_executor().timer(SAVE_DELAY);
        self.save = cx.spawn(async move |this, cx| {
            delay.await;
            this.update(cx, |this, cx| {
                if let Err(error) = this.settings.save(&this.settings_path) {
                    cx.emit(WorkspaceEvent::Problem(format!(
                        "The settings could not be saved: {error}"
                    )));
                }
            })
            .ok();
        });
    }

    // -- outside the window ------------------------------------------------

    /// Opens the selection in the editor.
    pub(crate) fn open_selection(&mut self, cx: &mut Context<Self>) {
        if let Some((_, location)) = self.selection_location() {
            self.open_in_editor(&location, cx);
        }
    }

    /// Opens a location in the editor, off the window's thread.
    pub(crate) fn open_in_editor(&mut self, location: &Location, cx: &mut Context<Self>) {
        let Some(root) = self.root() else {
            return;
        };
        let file: PathBuf = root.join(location.file.as_str().split('/').collect::<PathBuf>());
        let line = location.span.start_line.max(1);
        let command = self.settings.editor.clone();
        let opened = cx
            .background_executor()
            .spawn(async move { crate::editor::open(&command, &file, line, 1) });
        cx.spawn(async move |this, cx| {
            if let Err(message) = opened.await {
                this.update(cx, |_, cx| cx.emit(WorkspaceEvent::Problem(message)))
                    .ok();
            }
        })
        .detach();
    }

    /// Copies `text` and says so.
    pub(crate) fn copy(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        cx.emit(WorkspaceEvent::Notice(format!("Copied {text}")));
    }
}
