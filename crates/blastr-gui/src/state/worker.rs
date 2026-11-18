//! The window's end of the engine process.
//!
//! The analysis runs in `blastr-engine`, a console program started with no
//! window of its own: loading a project with rust-analyzer starts Cargo, rustc,
//! `rustup` and the procedural-macro server, and inside a program without a
//! console each of them would open a console window of its own. Editors run
//! rust-analyzer as a windowless child for the same reason, and Blastr does the
//! same.
//!
//! A client thread owns the child process. The window sends [`Command`]s and
//! polls [`Event`]s, so nothing here blocks it. A job that is replaced or
//! cancelled stops at the engine's next cancellation check; a job that is
//! still running [`CANCEL_GRACE`] later ends the process, and the next request
//! starts a new engine. When the engine exits on its own, the running jobs fail
//! with the end of its log as the detail, and the window stays usable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command as Process, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use blastr_core::Progress as _;
use blastr_core::service::protocol::{self, JobId, JobKind, Request, Response};
use blastr_model::{
    Analysis, AnalysisOptions, AnalysisParams, Assume, CancelToken, CommitInfo, DiffResult,
    RevSpec, RevisionGraph, SymbolPath,
};

use super::loadable::Failure;
use super::session::{CommitRef, RepoInfo};
use super::tasks::ProgressCell;
use crate::editor::{Detected, Editor};

/// How often the client looks at its channels and its cancellation flags.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long a cancelled job may keep running before the engine is ended.
const CANCEL_GRACE: Duration = Duration::from_secs(3);

/// How many lines of the engine's log a failure repeats.
const LOG_TAIL_LINES: usize = 8;

/// The file name of the engine, installed beside the window.
const ENGINE_NAME: &str = "blastr-engine";

/// The file name of the engine's log, in the cache directory.
const LOG_NAME: &str = "engine.log";

/// Windows' `CREATE_NO_WINDOW`: start the child without a console of its own.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A repository that was opened, as the window's own state holds it.
impl From<protocol::OpenedRepository> for RepoInfo {
    fn from(opened: protocol::OpenedRepository) -> RepoInfo {
        let repository = opened.repository;
        RepoInfo {
            root: repository.root,
            name: repository.name,
            branch: repository.branch,
            head: repository.head.map(|commit| CommitRef {
                id: commit.id,
                short: commit.short,
            }),
            dirty: repository.dirty,
        }
    }
}

impl From<protocol::Failure> for Failure {
    fn from(failure: protocol::Failure) -> Failure {
        Failure {
            message: failure.message,
            hint: failure.hint,
        }
    }
}

/// What the window asks the engine to do.
#[derive(Debug)]
pub(crate) enum Command {
    /// Find the repository above a path and read its identity.
    Open {
        /// The folder the user picked.
        path: PathBuf,
    },
    /// Compare two revisions and trace the impact.
    Analyze {
        /// Identifies the job; results carry it back.
        generation: u64,
        /// The working tree of the repository.
        repo: PathBuf,
        /// The two sides to compare.
        spec: RevSpec,
        /// The configuration to analyse under.
        options: AnalysisOptions,
        /// The parameters of the impact analysis.
        params: AnalysisParams,
        /// Cancelled when the job is replaced or stopped.
        cancel: CancelToken,
        /// Where the job reports its progress.
        progress: ProgressCell,
    },
    /// Trace what an assumed change of one symbol would reach.
    WhatIf {
        /// Identifies the job; results carry it back.
        generation: u64,
        /// The working tree of the repository.
        repo: PathBuf,
        /// The symbol assumed to change.
        symbol: SymbolPath,
        /// The kind of change assumed.
        assume: Assume,
        /// The configuration to analyse under.
        options: AnalysisOptions,
        /// The parameters of the impact analysis.
        params: AnalysisParams,
        /// Cancelled when the job is replaced or stopped.
        cancel: CancelToken,
        /// Where the job reports its progress.
        progress: ProgressCell,
    },
    /// Read a page of commits.
    Commits {
        /// The working tree of the repository.
        repo: PathBuf,
        /// How many of the newest commits to leave out.
        skip: usize,
        /// How many commits to return.
        limit: usize,
    },
    /// Open a file in the configured editor.
    OpenEditor {
        /// The absolute file to open.
        file: PathBuf,
        /// The line to put the caret on.
        line: u32,
        /// The column to put the caret on.
        column: u32,
        /// The editor the user chose.
        editor: Editor,
        /// The command of [`Editor::Custom`].
        custom: String,
    },
    /// Find the editors installed on this machine.
    DetectEditors,
    /// Stop the client and its engine.
    Quit,
}

/// What the engine reports back.
#[derive(Debug)]
pub(crate) enum Event {
    /// A repository was opened, or could not be.
    Opened {
        /// The repository, or why it cannot be used.
        result: Result<Box<RepoInfo>, String>,
    },
    /// The editors installed on this machine were found.
    Editors {
        /// What was found, in a stable order.
        editors: Vec<Detected>,
    },
    /// Something to tell the user briefly, such as an editor that would not
    /// start.
    Notice {
        /// The sentence the window shows.
        text: String,
    },
    /// A page of commits arrived.
    Commits {
        /// How many commits were skipped; identifies the page.
        skip: usize,
        /// The page, or why it could not be read.
        result: Result<Vec<CommitInfo>, String>,
    },
    /// The classified changes of a comparison arrived before its impact.
    Changes {
        /// Identifies the job.
        generation: u64,
        /// The changes.
        diff: Arc<DiffResult>,
    },
    /// A comparison finished.
    Result {
        /// Identifies the job.
        generation: u64,
        /// The analysis.
        analysis: Arc<Analysis>,
        /// The graph of the newer side, for symbol search.
        graph: Option<Arc<RevisionGraph>>,
        /// How long the engine took.
        elapsed: Duration,
    },
    /// A comparison failed.
    Failed {
        /// Identifies the job.
        generation: u64,
        /// What went wrong.
        failure: Failure,
        /// Whether the comparison was already showing a result: a failure
        /// while watching then keeps that result on screen.
        watching: bool,
    },
    /// A watching session noticed a save and started recomputing.
    Refreshing {
        /// Identifies the job.
        generation: u64,
    },
    /// The base side of a watching session moved: it has to be built again.
    BaseMoved {
        /// Identifies the job.
        generation: u64,
    },
    /// A hypothetical change finished.
    WhatIfResult {
        /// Identifies the job.
        generation: u64,
        /// The hypothetical analysis.
        analysis: Arc<Analysis>,
        /// How long the engine took.
        elapsed: Duration,
    },
    /// A hypothetical change failed.
    WhatIfFailed {
        /// Identifies the job.
        generation: u64,
        /// What went wrong.
        failure: Failure,
    },
}

/// The window's end of the engine process.
pub(crate) struct EngineWorker {
    commands: Sender<Command>,
    events: Receiver<Event>,
}

impl EngineWorker {
    /// Starts the client thread; the engine itself starts with the first
    /// request.
    pub(crate) fn spawn() -> EngineWorker {
        let (commands, command_receiver) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        thread::Builder::new()
            .name("blastr-engine-client".to_owned())
            .spawn(move || Client::new(event_sender).run(command_receiver))
            .expect("the engine client thread starts");
        EngineWorker { commands, events }
    }

    /// Sends a command. A closed client is ignored: the window is closing.
    pub(crate) fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// The next event, when one has arrived. Never blocks.
    pub(crate) fn next_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

impl Drop for EngineWorker {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
    }
}

/// A job that is waiting for its answer.
struct Job {
    /// Set by the window when the job is replaced or stopped.
    cancel: CancelToken,
    /// Where its progress goes.
    progress: ProgressCell,
    /// Whether the engine keeps following the working tree after the first
    /// result, so that failures and results keep coming.
    watching: bool,
}

/// The client thread: it owns the engine process and the jobs it is running.
struct Client {
    events: Sender<Event>,
    process: Option<EngineProcess>,
    jobs: HashMap<JobId, Job>,
    /// Whether an [`Command::Open`] is waiting for its answer.
    opening: bool,
    /// When the engine has to be ended if it has not answered the question
    /// asked after a cancellation.
    probe: Option<Instant>,
}

/// The engine process, with the thread that reads its answers.
struct EngineProcess {
    child: Child,
    input: ChildStdin,
    answers: Receiver<Response>,
    log: PathBuf,
}

impl EngineProcess {
    /// Starts the engine beside the window.
    fn spawn() -> Result<EngineProcess, String> {
        let program = engine_program()?;
        let log = log_path();
        let mut command = Process::new(&program);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log_stream(&log));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|error| {
            format!(
                "could not start the analysis engine `{}`: {error}",
                program.display()
            )
        })?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "the analysis engine could not be given its input stream".to_owned())?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| "the analysis engine could not be given its output stream".to_owned())?;
        let (sender, answers) = mpsc::channel();
        thread::Builder::new()
            .name("blastr-engine-reader".to_owned())
            .spawn(move || read_answers(output, sender))
            .map_err(|error| format!("could not start reading the engine's answers: {error}"))?;
        Ok(EngineProcess {
            child,
            input,
            answers,
            log,
        })
    }

    /// Sends one request to the engine.
    fn send(&mut self, request: &Request) -> Result<(), String> {
        protocol::write_message(&mut self.input, request)
            .map_err(|error| format!("the analysis engine could not be reached: {error}"))
    }
}

impl Drop for EngineProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Client {
    /// A client that reports into `events`.
    fn new(events: Sender<Event>) -> Client {
        Client {
            events,
            process: None,
            jobs: HashMap::new(),
            opening: false,
            probe: None,
        }
    }

    /// The client thread's loop: commands, answers and cancellations, until the
    /// window closes.
    fn run(mut self, commands: Receiver<Command>) {
        loop {
            let mut quit = false;
            loop {
                match commands.try_recv() {
                    Ok(Command::Quit) => {
                        quit = true;
                        break;
                    }
                    Ok(command) => self.handle(command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        quit = true;
                        break;
                    }
                }
            }
            if quit {
                return;
            }
            self.receive();
            self.enforce_cancels();
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Does what one command asks for.
    fn handle(&mut self, command: Command) {
        match command {
            Command::Quit => {}
            Command::Open { path } => match self.start() {
                Ok(()) => {
                    self.opening = true;
                    self.forward(Request::Open { path });
                }
                Err(message) => {
                    let _ = self.events.send(Event::Opened {
                        result: Err(message),
                    });
                }
            },
            Command::Analyze {
                generation,
                repo,
                spec,
                options,
                params,
                cancel,
                progress,
            } => {
                let id = JobId {
                    kind: JobKind::Compare,
                    generation,
                };
                let watching = spec.worktree && options.watch;
                self.begin_job(id, cancel, progress, watching);
                let request = Request::Compare {
                    id,
                    repo,
                    spec,
                    options,
                    params,
                };
                self.start_job(id, request);
            }
            Command::WhatIf {
                generation,
                repo,
                symbol,
                assume,
                options,
                params,
                cancel,
                progress,
            } => {
                let id = JobId {
                    kind: JobKind::WhatIf,
                    generation,
                };
                self.begin_job(id, cancel, progress, false);
                let request = Request::WhatIf {
                    id,
                    repo,
                    symbol,
                    assume,
                    options,
                    params,
                };
                self.start_job(id, request);
            }
            Command::Commits { repo, skip, limit } => {
                let result = read_commits(&repo, skip, limit);
                let _ = self.events.send(Event::Commits { skip, result });
            }
            Command::OpenEditor {
                file,
                line,
                column,
                editor,
                custom,
            } => {
                // Detection reads the file system, so it happens here and not
                // while the window draws; a second look is cheap.
                let detected = crate::editor::detect();
                if let Err(text) =
                    crate::editor::open(editor, &custom, &detected, &file, line, column)
                {
                    let _ = self.events.send(Event::Notice { text });
                }
            }
            Command::DetectEditors => {
                let _ = self.events.send(Event::Editors {
                    editors: crate::editor::detect(),
                });
            }
        }
    }

    /// Notes a new job and forgets the older ones of its kind: their results
    /// are of no interest any more.
    fn begin_job(
        &mut self,
        id: JobId,
        cancel: CancelToken,
        progress: ProgressCell,
        watching: bool,
    ) {
        self.jobs
            .retain(|other, _| other.kind != id.kind || other.generation >= id.generation);
        self.jobs.insert(
            id,
            Job {
                cancel,
                progress,
                watching,
            },
        );
    }

    /// Hands a job's request to the engine, reporting a failure when there is
    /// no engine to hand it to.
    fn start_job(&mut self, id: JobId, request: Request) {
        match self.start() {
            Ok(()) => self.forward(request),
            Err(message) => self.fail_job(id, Failure::new(message)),
        }
    }

    /// The engine, started when there is none.
    fn start(&mut self) -> Result<(), String> {
        if self.process.is_some() {
            return Ok(());
        }
        let mut engine = EngineProcess::spawn()?;
        engine.send(&Request::Hello {
            protocol: protocol::PROTOCOL_VERSION,
        })?;
        self.process = Some(engine);
        Ok(())
    }

    /// Writes one request to the engine.
    fn forward(&mut self, request: Request) {
        let sent = match self.process.as_mut() {
            Some(engine) => engine.send(&request),
            None => return,
        };
        if let Err(message) = sent {
            self.stopped(&message);
        }
    }

    /// Reads the answers that have arrived.
    fn receive(&mut self) {
        loop {
            let next = match self.process.as_ref() {
                Some(engine) => engine.answers.try_recv(),
                None => return,
            };
            match next {
                Ok(response) => self.deliver(response),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.stopped("its output stream closed");
                    return;
                }
            }
        }
    }

    /// Folds one answer into the events the window reads.
    fn deliver(&mut self, response: Response) {
        match response {
            Response::Hello { protocol, build } => {
                if let Some(message) = mismatch(protocol, &build) {
                    self.stopped(&message);
                }
            }
            Response::Opened { result } => {
                self.opening = false;
                let result = result
                    .map(|opened| Box::new(RepoInfo::from(opened)))
                    .map_err(|failure| failure.message);
                let _ = self.events.send(Event::Opened { result });
            }
            Response::Progress { id, report } => {
                if let Some(job) = self.jobs.get(&id) {
                    job.progress
                        .report(&report.phase, report.done, report.total);
                }
            }
            Response::Changes { id, diff } => {
                let _ = self.events.send(Event::Changes {
                    generation: id.generation,
                    diff: Arc::new(diff),
                });
            }
            Response::Result {
                id,
                analysis,
                graph,
                elapsed_ms,
            } => {
                if !self.watching(id) {
                    self.jobs.remove(&id);
                }
                let _ = self.events.send(Event::Result {
                    generation: id.generation,
                    analysis: Arc::new(*analysis),
                    graph: graph.map(|graph| Arc::new(*graph)),
                    elapsed: Duration::from_millis(elapsed_ms),
                });
            }
            Response::Failed {
                id,
                failure,
                watching,
            } => {
                if !watching {
                    self.jobs.remove(&id);
                }
                let _ = self.events.send(Event::Failed {
                    generation: id.generation,
                    failure: Failure::from(failure),
                    watching,
                });
            }
            Response::Refreshing { id } => {
                let _ = self.events.send(Event::Refreshing {
                    generation: id.generation,
                });
            }
            Response::BaseMoved { id } => {
                self.jobs.remove(&id);
                let _ = self.events.send(Event::BaseMoved {
                    generation: id.generation,
                });
            }
            Response::WhatIfResult {
                id,
                analysis,
                elapsed_ms,
            } => {
                self.jobs.remove(&id);
                let _ = self.events.send(Event::WhatIfResult {
                    generation: id.generation,
                    analysis: Arc::new(*analysis),
                    elapsed: Duration::from_millis(elapsed_ms),
                });
            }
            Response::WhatIfFailed { id, failure } => {
                self.jobs.remove(&id);
                let _ = self.events.send(Event::WhatIfFailed {
                    generation: id.generation,
                    failure: Failure::from(failure),
                });
            }
            // The window has no use for the cache yet; the answer is only
            // interesting to a front end that asks for it. It does tell the
            // client that the engine has worked through its requests.
            Response::Cache { .. } => self.probe = None,
        }
    }

    /// Whether the job for `id` is one that keeps following the working tree.
    fn watching(&self, id: JobId) -> bool {
        self.jobs.get(&id).is_some_and(|job| job.watching)
    }

    /// Reports a failure of one job and forgets it.
    fn fail_job(&mut self, id: JobId, failure: Failure) {
        self.jobs.remove(&id);
        let event = match id.kind {
            JobKind::Compare => Event::Failed {
                generation: id.generation,
                failure,
                watching: false,
            },
            JobKind::WhatIf => Event::WhatIfFailed {
                generation: id.generation,
                failure,
            },
        };
        let _ = self.events.send(event);
    }

    /// Ends the engine after a failure to reach it, telling the window why.
    ///
    /// Every running job fails; the next request starts a new engine.
    fn stopped(&mut self, reason: &str) {
        let Some(engine) = self.process.take() else {
            return;
        };
        let detail = failure_detail(Some(&engine.log));
        drop(engine);
        self.probe = None;
        let mut failure = Failure::new(format!("The analysis engine stopped: {reason}."));
        if !detail.is_empty() {
            failure.hint = Some(detail);
        }
        for (id, _) in std::mem::take(&mut self.jobs) {
            let event = match id.kind {
                JobKind::Compare => Event::Failed {
                    generation: id.generation,
                    failure: failure.clone(),
                    watching: false,
                },
                JobKind::WhatIf => Event::WhatIfFailed {
                    generation: id.generation,
                    failure: failure.clone(),
                },
            };
            let _ = self.events.send(event);
        }
        if self.opening {
            self.opening = false;
            let _ = self.events.send(Event::Opened {
                result: Err(failure.message),
            });
        }
    }

    /// Passes cancellations on, and ends an engine that does not stop.
    fn enforce_cancels(&mut self) {
        let mut cancel = Vec::new();
        for (id, job) in self.jobs.iter() {
            if job.cancel.is_cancelled() {
                cancel.push(*id);
            }
        }
        for id in cancel {
            self.jobs.remove(&id);
            self.forward(Request::Cancel { id });
            // The engine answers its next request only once the cancelled job
            // has stopped, so a cheap question tells the client whether it did.
            self.forward(Request::CacheStatus);
            self.probe = Some(Instant::now() + CANCEL_GRACE);
        }
        if self
            .probe
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.probe = None;
            // Nothing waits for the abandoned job any more, so the engine is
            // replaced without further ado.
            self.process = None;
            self.opening = false;
        }
    }
}

/// Reads the engine's answers until the stream ends.
fn read_answers(mut output: ChildStdout, answers: Sender<Response>) {
    loop {
        match protocol::read_message::<_, Response>(&mut output) {
            Ok(Some(response)) => {
                if answers.send(response).is_err() {
                    return;
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}

/// Why an engine of another build cannot be used, when it cannot.
fn mismatch(protocol: u32, build: &str) -> Option<String> {
    if protocol != protocol::PROTOCOL_VERSION {
        return Some(format!(
            "it speaks protocol {protocol} while this window speaks {}; \
             install both programs from the same build of Blastr",
            protocol::PROTOCOL_VERSION
        ));
    }
    let version = build.split('+').next().unwrap_or(build);
    if version != blastr_core::VERSION {
        return Some(format!(
            "it is version {version} while this window is version {}; \
             install both programs from the same build of Blastr",
            blastr_core::VERSION
        ));
    }
    None
}

/// The engine program: `blastr-engine` beside the running window.
fn engine_program() -> Result<PathBuf, String> {
    let folder = std::env::current_exe()
        .map_err(|error| format!("could not tell where Blastr is installed: {error}"))?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "could not tell where Blastr is installed".to_owned())?;
    let program = folder.join(format!("{ENGINE_NAME}{}", std::env::consts::EXE_SUFFIX));
    if !program.is_file() {
        return Err(format!(
            "the analysis engine `{}` is missing. Reinstall Blastr, so that both \
             programs are installed together.",
            program.display()
        ));
    }
    Ok(program)
}

/// The file the engine's log is written to.
fn log_path() -> PathBuf {
    match blastr_store::paths() {
        Ok(paths) => paths.cache_dir.join(LOG_NAME),
        Err(_) => std::env::temp_dir().join(format!("blastr-{LOG_NAME}")),
    }
}

/// A stream for the engine's log, truncated so that it describes this run.
fn log_stream(path: &Path) -> Stdio {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::File::create(path) {
        Ok(file) => Stdio::from(file),
        Err(_) => Stdio::null(),
    }
}

/// The end of the engine's log, as the detail of a failure.
fn failure_detail(log: Option<&Path>) -> String {
    let Some(log) = log else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(log) else {
        return String::new();
    };
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n")
}

/// Reads a page of commits. This needs no analysis, so it stays in the window's
/// own process.
fn read_commits(root: &Path, skip: usize, limit: usize) -> Result<Vec<CommitInfo>, String> {
    let root = blastr_git::discover(root).map_err(|error| error.to_string())?;
    let repo = blastr_git::Repo::open(&root).map_err(|error| error.to_string())?;
    repo.log(skip, limit).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use blastr_core::service::protocol::{Failure as WireFailure, JobKind, Report};

    use super::*;

    /// A client whose events the test reads, with no engine behind it.
    fn client() -> (Client, Receiver<Event>) {
        let (sender, events) = mpsc::channel();
        (Client::new(sender), events)
    }

    fn job(kind: JobKind, generation: u64) -> (JobId, ProgressCell) {
        let id = JobId { kind, generation };
        let cell = ProgressCell::default();
        (id, cell)
    }

    #[test]
    fn a_failure_answer_reaches_the_window() {
        let (mut client, events) = client();
        let (id, cell) = job(JobKind::Compare, 3);
        client.begin_job(id, CancelToken::new(), cell, false);

        client.deliver(Response::Failed {
            id,
            failure: WireFailure {
                message: "the project could not be loaded".to_owned(),
                hint: Some("check that `cargo metadata` works".to_owned()),
            },
            watching: false,
        });

        match events.try_recv().expect("one event") {
            Event::Failed {
                generation,
                failure,
                watching,
            } => {
                assert_eq!(generation, 3);
                assert_eq!(failure.message, "the project could not be loaded");
                assert_eq!(
                    failure.hint.as_deref(),
                    Some("check that `cargo metadata` works")
                );
                assert!(!watching);
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        assert!(client.jobs.is_empty(), "a finished job is forgotten");
    }

    #[test]
    fn a_failure_while_watching_keeps_the_job() {
        let (mut client, _events) = client();
        let (id, cell) = job(JobKind::Compare, 2);
        client.begin_job(id, CancelToken::new(), cell, true);

        client.deliver(Response::Failed {
            id,
            failure: WireFailure::new("the saved file does not parse"),
            watching: true,
        });

        assert!(client.jobs.contains_key(&id), "a watching job lives on");
    }

    #[test]
    fn progress_reaches_the_job_it_names_and_no_other() {
        let (mut client, _events) = client();
        let (id, cell) = job(JobKind::Compare, 7);
        client.begin_job(id, CancelToken::new(), cell.clone(), true);
        let mut seen = 0;
        assert!(cell.take_if_changed(&mut seen).is_none());

        client.deliver(Response::Progress {
            id,
            report: Report {
                phase: "extract head".to_owned(),
                done: 4,
                total: Some(9),
            },
        });
        let progress = cell.take_if_changed(&mut seen).expect("a report");
        assert_eq!(progress.phase, "extract head");
        assert_eq!((progress.done, progress.total), (4, Some(9)));

        let stranger = JobId {
            kind: JobKind::Compare,
            generation: 99,
        };
        client.deliver(Response::Progress {
            id: stranger,
            report: Report {
                phase: "impact".to_owned(),
                done: 0,
                total: None,
            },
        });
        assert!(cell.take_if_changed(&mut seen).is_none());
    }

    #[test]
    fn a_newer_job_forgets_the_older_ones_of_its_kind() {
        let (mut client, _events) = client();
        let (first, cell) = job(JobKind::Compare, 1);
        client.begin_job(first, CancelToken::new(), cell, true);
        let (what_if, cell) = job(JobKind::WhatIf, 1);
        client.begin_job(what_if, CancelToken::new(), cell, false);
        let (second, cell) = job(JobKind::Compare, 2);
        client.begin_job(second, CancelToken::new(), cell, true);

        assert!(
            !client.jobs.contains_key(&first),
            "the old comparison is gone"
        );
        assert!(
            client.jobs.contains_key(&what_if),
            "what-if counts on its own"
        );
        assert!(client.jobs.contains_key(&second));
    }

    #[test]
    fn a_cancelled_job_is_passed_on_with_a_question_behind_it() {
        let (mut client, _events) = client();
        let (id, cell) = job(JobKind::Compare, 1);
        let cancel = CancelToken::new();
        client.begin_job(id, cancel.clone(), cell, true);

        cancel.cancel();
        client.enforce_cancels();

        assert!(client.jobs.is_empty(), "an abandoned job is forgotten");
        assert!(
            client.probe.is_some(),
            "the engine is asked whether it stopped"
        );
    }

    #[test]
    fn an_engine_that_does_not_stop_is_replaced() {
        let (mut client, _events) = client();
        client.probe = Some(Instant::now());

        client.enforce_cancels();

        assert!(client.probe.is_none());
        assert!(client.process.is_none());
    }

    #[test]
    fn a_job_that_is_not_cancelled_is_left_alone() {
        let (mut client, _events) = client();
        let (id, cell) = job(JobKind::WhatIf, 1);
        client.begin_job(id, CancelToken::new(), cell, false);

        client.enforce_cancels();

        assert!(client.jobs.contains_key(&id));
        assert!(client.probe.is_none());
    }

    #[test]
    fn an_engine_of_another_build_is_refused() {
        assert!(mismatch(protocol::PROTOCOL_VERSION, "0.1.0+abcd1234").is_none());
        let wrong_protocol = mismatch(protocol::PROTOCOL_VERSION + 1, "0.1.0+abcd1234")
            .expect("a mismatched protocol");
        assert!(wrong_protocol.contains("protocol"), "{wrong_protocol}");
        let wrong_version =
            mismatch(protocol::PROTOCOL_VERSION, "9.9.9+abcd1234").expect("a mismatched version");
        assert!(wrong_version.contains("9.9.9"), "{wrong_version}");
    }

    #[test]
    fn the_end_of_the_log_is_the_detail_of_a_failure() {
        let directory = tempfile::tempdir().expect("a directory");
        let log = directory.path().join("engine.log");
        let mut text = String::new();
        for line in 0..12 {
            text.push_str(&format!("line {line}\n"));
        }
        std::fs::write(&log, text).expect("a log");

        let detail = failure_detail(Some(&log));

        assert!(!detail.contains("line 3"), "{detail}");
        assert!(detail.contains("line 11"), "{detail}");
        assert_eq!(detail.lines().count(), LOG_TAIL_LINES);
        assert_eq!(failure_detail(None), "");
        assert_eq!(failure_detail(Some(&directory.path().join("none"))), "");
        std::fs::write(&log, "\n\n").expect("an empty log");
        assert_eq!(failure_detail(Some(&log)), "");
    }

    #[test]
    fn a_repository_above_the_path_is_found_for_its_commits() {
        let directory = tempfile::tempdir().expect("a directory");
        let error = read_commits(directory.path(), 0, 1).expect_err("no repository");
        assert!(error.contains("repository"), "{error}");
    }
}
