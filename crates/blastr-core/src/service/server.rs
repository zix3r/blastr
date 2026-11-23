//! The engine's side of the protocol.
//!
//! One thread reads requests and hands them on; another owns the [`Engine`] and
//! the [`Session`] and does the work, one request at a time. They share only
//! two things: the job that runs now, so that a cancellation reaches it at
//! once, and the jobs cancelled before they started.
//!
//! A comparison of the working tree keeps watching after its first result:
//! every 250 ms it asks the session whether a saved file changed it, and asks
//! the repository whether `HEAD` moved, exactly as the window needs.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use blastr_git::Repo;
use blastr_model::{
    AnalysisOptions, AnalysisParams, Assume, CancelToken, DiffResult, Progress, RevSpec,
    RevisionGraph, SymbolPath,
};

use super::protocol::{
    self, Commit, Failure, JobId, OpenedRepository, Report, Repository, Request, Response,
};
use crate::engine::{CacheStatus, Engine, build_id};
use crate::session::{Session, SessionRequest};

/// How often a watching session asks whether anything changed.
const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// Shortest interval between two progress reports sent to the window.
///
/// A large project reports progress for every file it extracts; the window
/// redraws its status bar a few times a second, so more would be wasted.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// How the engine is obtained.
///
/// The server asks for it on the first request that needs it, so that an engine
/// which cannot be opened says so once and keeps saying the same thing. Tests
/// pass a factory that opens a cache in memory.
pub type EngineFactory = Box<dyn FnOnce() -> crate::Result<Engine> + Send>;

/// Serves the protocol on `input` and `output` until the input closes or a
/// [`Request::Shutdown`] arrives.
///
/// This is what the engine process runs, on its standard streams.
pub fn serve(
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
) -> io::Result<()> {
    serve_with(input, output, Box::new(Engine::open))
}

/// The same as [`serve`], with a say in where the cache comes from.
pub fn serve_with(
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
    factory: EngineFactory,
) -> io::Result<()> {
    let shared = Arc::new(Shared::default());
    let (requests, receiver) = mpsc::channel();
    let reader = {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("blastr-engine-requests".to_owned())
            .spawn(move || read_requests(input, &shared, &requests))
            .map_err(|error| {
                io::Error::other(format!("could not start the request thread: {error}"))
            })?
    };

    let mut server = Server::new(output, factory, Arc::clone(&shared));
    let result = server.run(receiver);
    // The request thread is left where it is: it is blocked on the input
    // stream, which only the window can close, and nothing it reads matters
    // once the work has stopped.
    drop(reader);
    result
}

/// Reads requests until the stream ends, cancelling jobs on the way.
fn read_requests(input: impl Read, shared: &Shared, requests: &Sender<Request>) {
    let mut input = input;
    loop {
        match protocol::read_message::<_, Request>(&mut input) {
            Ok(Some(Request::Cancel { id })) => shared.cancel(id),
            Ok(Some(request)) => {
                if requests.send(request).is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                tracing::warn!("the request stream is broken: {error}");
                return;
            }
        }
    }
}

/// State shared by the thread that reads requests and the one that works.
#[derive(Default)]
struct Shared {
    /// The job that runs now, so that a cancellation reaches it at once.
    current: Mutex<Option<(JobId, CancelToken)>>,
    /// Jobs cancelled before they started.
    cancelled: Mutex<HashSet<JobId>>,
}

impl Shared {
    /// Makes `id` the job that runs now and returns its cancellation token.
    fn begin(&self, id: JobId) -> CancelToken {
        let early = self
            .cancelled
            .lock()
            .map(|mut cancelled| cancelled.remove(&id))
            .unwrap_or(false);
        let token = CancelToken::new();
        if early {
            token.cancel();
        }
        if let Ok(mut current) = self.current.lock() {
            *current = Some((id, token.clone()));
        }
        token
    }

    /// Stops the job that runs now, or remembers that `id` must not start.
    fn cancel(&self, id: JobId) {
        let tripped = match self.current.lock() {
            Ok(current) => match current.as_ref() {
                Some((job, token)) if *job == id => {
                    token.cancel();
                    true
                }
                _ => false,
            },
            Err(_) => false,
        };
        if !tripped && let Ok(mut cancelled) = self.cancelled.lock() {
            cancelled.insert(id);
        }
    }

    /// Notes that no job runs any more.
    fn end(&self) {
        if let Ok(mut current) = self.current.lock() {
            *current = None;
        }
    }
}

/// Why a job stopped before the next request was handled.
enum Interrupted {
    /// A request arrived while the job was watching; it is boxed because a
    /// request is far larger than the two empty variants beside it.
    Request(Box<Request>),
    /// The window closed its end of the stream.
    Closed,
    /// The job ended by itself.
    Stopped,
}

/// The work side of the engine: it owns the engine and the loaded session.
struct Server<W: Write> {
    output: Arc<Output<W>>,
    factory: Option<EngineFactory>,
    engine: Option<Rc<Engine>>,
    engine_failure: Option<Failure>,
    shared: Arc<Shared>,
}

impl<W: Write + Send> Server<W> {
    /// Creates the server; nothing is opened until a request needs it.
    fn new(output: W, factory: EngineFactory, shared: Arc<Shared>) -> Server<W> {
        Server {
            output: Arc::new(Output::new(output)),
            factory: Some(factory),
            engine: None,
            engine_failure: None,
            shared,
        }
    }

    /// Serves requests until the stream ends or the window asks to stop.
    fn run(&mut self, requests: Receiver<Request>) -> io::Result<()> {
        let mut pending = None;
        loop {
            let request = match pending.take().or_else(|| requests.recv().ok()) {
                Some(request) => request,
                None => return Ok(()),
            };
            tracing::debug!("request: {}", name_of(&request));
            match request {
                Request::Hello { .. } => {
                    self.send(&Response::Hello {
                        protocol: protocol::PROTOCOL_VERSION,
                        build: build_id().to_owned(),
                    })?;
                }
                Request::Open { path } => {
                    let result = self.open(&path);
                    self.send(&Response::Opened { result })?;
                }
                Request::CacheStatus => {
                    let result = self.cache(false);
                    self.send(&Response::Cache { result })?;
                }
                Request::CacheClear => {
                    let result = self.cache(true);
                    self.send(&Response::Cache { result })?;
                }
                // The request thread has already tripped the token.
                Request::Cancel { .. } => {}
                Request::Compare { .. } => match self.compare(request, &requests)? {
                    Interrupted::Request(request) => pending = Some(*request),
                    Interrupted::Closed => return Ok(()),
                    Interrupted::Stopped => {}
                },
                Request::WhatIf { .. } => match self.what_if(request)? {
                    Interrupted::Request(request) => pending = Some(*request),
                    Interrupted::Closed => return Ok(()),
                    Interrupted::Stopped => {}
                },
                Request::Shutdown => return Ok(()),
            }
        }
    }

    /// Writes one response to the window.
    fn send(&self, response: &Response) -> io::Result<()> {
        self.output.send(response)
    }

    /// The engine, opened on first use.
    fn engine(&mut self) -> Result<Rc<Engine>, Failure> {
        if let Some(failure) = &self.engine_failure {
            return Err(failure.clone());
        }
        if self.engine.is_none() {
            let opened = match self.factory.take() {
                Some(factory) => factory(),
                None => return Err(Failure::new("the analysis engine could not be opened")),
            };
            match opened {
                Ok(engine) => self.engine = Some(Rc::new(engine)),
                Err(error) => {
                    let failure = Failure::from(&error);
                    self.engine_failure = Some(failure.clone());
                    return Err(failure);
                }
            }
        }
        Ok(Rc::clone(
            self.engine.as_ref().expect("the engine was just opened"),
        ))
    }

    /// A progress reporter for `id`.
    fn progress(&self, id: JobId) -> Reporter<W> {
        Reporter {
            output: Arc::clone(&self.output),
            id,
        }
    }

    /// Reports a failure of a job.
    fn fail(&self, id: JobId, failure: Failure, watching: bool) -> io::Result<()> {
        match id.kind {
            protocol::JobKind::Compare => self.send(&Response::Failed {
                id,
                failure,
                watching,
            }),
            protocol::JobKind::WhatIf => self.send(&Response::WhatIfFailed { id, failure }),
        }
    }

    /// Reads the identity of the repository above `path`.
    fn open(&mut self, path: &Path) -> Result<OpenedRepository, Failure> {
        let root = blastr_git::discover(path).map_err(|_| {
            Failure::new(format!(
                "`{}` is not inside a Git repository.",
                path.display()
            ))
        })?;
        if !root.path().join("Cargo.toml").is_file() {
            return Err(Failure::new(format!(
                "`{}` is not a Cargo project: there is no Cargo.toml in it. \
                 Open the root of a Cargo project or of a workspace.",
                root.path().display()
            )));
        }
        let repo = Repo::open(&root).map_err(git_failure)?;
        let branch = repo.branch().map_err(git_failure)?;
        let dirty = repo.is_dirty().map_err(git_failure)?;
        let head = repo
            .log(0, 1)
            .map_err(git_failure)?
            .first()
            .map(|commit| Commit {
                id: commit.id.clone(),
                short: commit.short_id.clone(),
            });

        Ok(OpenedRepository {
            repository: Repository {
                root: root.path().to_path_buf(),
                name: directory_name(root.path()),
                branch,
                head,
                dirty,
            },
        })
    }

    /// The state of the cache, and optionally its removal.
    fn cache(&mut self, clear: bool) -> Result<CacheStatus, Failure> {
        let engine = self.engine()?;
        let status = if clear {
            engine.cache_clear()
        } else {
            engine.cache_status()
        };
        status.map_err(|error| Failure::from(&error))
    }

    /// Runs one comparison, watching the working tree afterwards when asked.
    fn compare(
        &mut self,
        request: Request,
        requests: &Receiver<Request>,
    ) -> io::Result<Interrupted> {
        let Request::Compare {
            id,
            repo,
            spec,
            options,
            params,
        } = request
        else {
            return Ok(Interrupted::Stopped);
        };
        let cancel = self.shared.begin(id);
        let result = self.compare_job(id, &repo, &spec, &options, &params, &cancel, requests);
        self.shared.end();
        result
    }

    /// The body of [`Server::compare`].
    #[allow(clippy::too_many_arguments)]
    fn compare_job(
        &mut self,
        id: JobId,
        repo_root: &Path,
        spec: &RevSpec,
        options: &AnalysisOptions,
        params: &AnalysisParams,
        cancel: &CancelToken,
        requests: &Receiver<Request>,
    ) -> io::Result<Interrupted> {
        let engine = match self.engine() {
            Ok(engine) => engine,
            Err(failure) => {
                self.fail(id, failure, false)?;
                return Ok(Interrupted::Stopped);
            }
        };
        let progress = self.progress(id);
        let started = Instant::now();

        // A comparison of the working tree is followed only when the window asks
        // for it: with "refresh on save" off, the result stands until the user
        // runs the comparison again.
        if !spec.worktree || !options.watch {
            let output = Arc::clone(&self.output);
            let mut on_changes = |diff: &DiffResult| {
                let _ = output.send(&Response::Changes {
                    id,
                    diff: diff.clone(),
                });
            };
            match engine.analyze_with(
                repo_root,
                spec,
                options,
                params,
                &progress,
                cancel,
                &mut on_changes,
            ) {
                Ok(analysis) => {
                    if cancel.is_cancelled() {
                        return Ok(Interrupted::Stopped);
                    }
                    let graph = engine
                        .graph(
                            repo_root,
                            &analysis.diff.comparison.head,
                            options,
                            &progress,
                            cancel,
                        )
                        .ok();
                    self.send(&Response::Result {
                        id,
                        analysis: Box::new(analysis),
                        graph: graph.map(|graph| Box::new(graph.as_ref().clone())),
                        elapsed_ms: elapsed_ms(started),
                    })?;
                }
                Err(error) => {
                    if !cancel.is_cancelled() {
                        self.fail(id, Failure::from(&error), false)?;
                    }
                }
            }
            return Ok(Interrupted::Stopped);
        }

        let mut session = {
            let output = Arc::clone(&self.output);
            let mut on_changes = |diff: &DiffResult| {
                let _ = output.send(&Response::Changes {
                    id,
                    diff: diff.clone(),
                });
            };
            match engine.session_with(
                SessionRequest {
                    repo_root,
                    spec,
                    options,
                    params,
                },
                &progress,
                cancel,
                &mut on_changes,
            ) {
                Ok(session) => session,
                Err(error) => {
                    if !cancel.is_cancelled() {
                        self.fail(id, Failure::from(&error), false)?;
                    }
                    return Ok(Interrupted::Stopped);
                }
            }
        };
        if cancel.is_cancelled() {
            return Ok(Interrupted::Stopped);
        }
        self.send(&Response::Result {
            id,
            analysis: Box::new(session.analysis().clone()),
            graph: Some(Box::new(head_graph(&session))),
            elapsed_ms: elapsed_ms(started),
        })?;
        self.watch(&mut session, id, cancel, requests)
    }

    /// Follows a loaded working tree until a request arrives, the comparison is
    /// cancelled, or `HEAD` moves.
    fn watch(
        &mut self,
        session: &mut Session,
        id: JobId,
        cancel: &CancelToken,
        requests: &Receiver<Request>,
    ) -> io::Result<Interrupted> {
        let progress = self.progress(id);
        loop {
            match requests.try_recv() {
                Ok(request) => return Ok(Interrupted::Request(Box::new(request))),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return Ok(Interrupted::Closed),
            }
            thread::sleep(WATCH_INTERVAL);
            if cancel.is_cancelled() {
                return Ok(Interrupted::Stopped);
            }

            match session.base_moved() {
                Ok(true) => {
                    self.send(&Response::BaseMoved { id })?;
                    return Ok(Interrupted::Stopped);
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!("could not tell whether the base moved: {error}");
                    return Ok(Interrupted::Stopped);
                }
            }

            let stale = match session.is_stale() {
                Ok(stale) => stale,
                Err(error) => {
                    tracing::warn!("could not tell whether the working tree changed: {error}");
                    return Ok(Interrupted::Stopped);
                }
            };
            if !stale {
                continue;
            }
            self.send(&Response::Refreshing { id })?;
            let started = Instant::now();
            let output = Arc::clone(&self.output);
            let mut on_changes = |diff: &DiffResult| {
                let _ = output.send(&Response::Changes {
                    id,
                    diff: diff.clone(),
                });
            };
            match session.refresh_with(&progress, cancel, &mut on_changes) {
                Ok(refresh) => {
                    if cancel.is_cancelled() {
                        return Ok(Interrupted::Stopped);
                    }
                    self.send(&Response::Result {
                        id,
                        analysis: Box::new(refresh.analysis),
                        graph: Some(Box::new(head_graph(session))),
                        elapsed_ms: elapsed_ms(started),
                    })?;
                }
                Err(error) => {
                    if cancel.is_cancelled() {
                        return Ok(Interrupted::Stopped);
                    }
                    self.fail(id, Failure::from(&error), true)?;
                }
            }
        }
    }

    /// Runs one hypothetical change.
    fn what_if(&mut self, request: Request) -> io::Result<Interrupted> {
        let Request::WhatIf {
            id,
            repo,
            symbol,
            assume,
            options,
            params,
        } = request
        else {
            return Ok(Interrupted::Stopped);
        };
        let cancel = self.shared.begin(id);
        let result = self.what_if_job(id, &repo, &symbol, assume, &options, &params, &cancel);
        self.shared.end();
        result
    }

    /// The body of [`Server::what_if`].
    #[allow(clippy::too_many_arguments)]
    fn what_if_job(
        &mut self,
        id: JobId,
        repo_root: &Path,
        symbol: &SymbolPath,
        assume: Assume,
        options: &AnalysisOptions,
        params: &AnalysisParams,
        cancel: &CancelToken,
    ) -> io::Result<Interrupted> {
        let engine = match self.engine() {
            Ok(engine) => engine,
            Err(failure) => {
                self.fail(id, failure, false)?;
                return Ok(Interrupted::Stopped);
            }
        };
        let progress = self.progress(id);
        let started = Instant::now();
        match engine.what_if(
            repo_root,
            symbol,
            assume,
            options,
            params,
            &progress,
            cancel,
        ) {
            Ok(analysis) => {
                if !cancel.is_cancelled() {
                    self.send(&Response::WhatIfResult {
                        id,
                        analysis: Box::new(analysis),
                        elapsed_ms: elapsed_ms(started),
                    })?;
                }
            }
            Err(error) => {
                if !cancel.is_cancelled() {
                    self.fail(id, Failure::from(&error), false)?;
                }
            }
        }
        Ok(Interrupted::Stopped)
    }
}

/// The graph of the newer side of a loaded session.
fn head_graph(session: &Session) -> RevisionGraph {
    session.head_graph().as_ref().clone()
}

/// How long a job took, in milliseconds.
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The name of a request, for the log.
fn name_of(request: &Request) -> &'static str {
    match request {
        Request::Hello { .. } => "hello",
        Request::Open { .. } => "open",
        Request::Compare { .. } => "compare",
        Request::WhatIf { .. } => "what-if",
        Request::Cancel { .. } => "cancel",
        Request::CacheStatus => "cache status",
        Request::CacheClear => "cache clear",
        Request::Shutdown => "shutdown",
    }
}

/// Turns a Git failure into a message the window can show.
fn git_failure(error: blastr_git::Error) -> Failure {
    Failure {
        message: error.to_string(),
        hint: crate::Error::Git(error).hint(),
    }
}

/// The directory name of `path`, shown as the repository's name.
fn directory_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The output stream of the engine, shared with the progress reporters.
struct Output<W> {
    stream: Mutex<W>,
    last_progress: Mutex<Instant>,
    broken: AtomicBool,
}

impl<W: Write> Output<W> {
    /// Wraps an output stream.
    fn new(stream: W) -> Output<W> {
        Output {
            stream: Mutex::new(stream),
            last_progress: Mutex::new(Instant::now() - PROGRESS_INTERVAL),
            broken: AtomicBool::new(false),
        }
    }

    /// Writes one response; a write that fails is remembered, so that the work
    /// loop stops instead of failing on every report.
    fn send(&self, response: &Response) -> io::Result<()> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(io::Error::other("the window is gone"));
        }
        let written = match self.stream.lock() {
            Ok(mut stream) => protocol::write_message(&mut *stream, response),
            Err(_) => Err(io::Error::other("the output stream is unusable")),
        };
        if written.is_err() {
            self.broken.store(true, Ordering::SeqCst);
        }
        written
    }

    /// Sends a progress report, at most once every [`PROGRESS_INTERVAL`].
    fn report(&self, id: JobId, phase: &str, done: u64, total: Option<u64>) {
        if let Ok(mut last) = self.last_progress.lock() {
            let now = Instant::now();
            if now.duration_since(*last) < PROGRESS_INTERVAL {
                return;
            }
            *last = now;
        }
        let _ = self.send(&Response::Progress {
            id,
            report: Report {
                phase: phase.to_owned(),
                done,
                total,
            },
        });
    }
}

/// A progress sink that forwards to the window as [`Response::Progress`].
struct Reporter<W> {
    output: Arc<Output<W>>,
    id: JobId,
}

impl<W: Write + Send> Progress for Reporter<W> {
    fn report(&self, phase: &str, done: u64, total: Option<u64>) {
        self.output.report(self.id, phase, done, total);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, PipeReader, PipeWriter};

    use blastr_model::{AnalysisOptions, AnalysisParams, CancelToken};

    use super::*;

    /// Pipes of an engine that runs on a cache in memory.
    fn server() -> (
        PipeWriter,
        BufReader<PipeReader>,
        thread::JoinHandle<io::Result<()>>,
    ) {
        let (requests, input) = std::io::pipe().expect("a pipe");
        let (output, responses) = std::io::pipe().expect("a pipe");
        let handle = thread::spawn(move || {
            serve_with(
                requests,
                responses,
                Box::new(|| {
                    let store = blastr_store::Store::open_in_memory()?;
                    Ok(Engine::with_store(store))
                }),
            )
        });
        (input, BufReader::new(output), handle)
    }

    /// Sends one request and reads one response.
    fn ask(
        input: &mut PipeWriter,
        reader: &mut BufReader<PipeReader>,
        request: Request,
    ) -> Response {
        protocol::write_message(input, &request).expect("a written request");
        protocol::read_message(reader)
            .expect("a readable response")
            .expect("a response")
    }

    #[test]
    fn the_engine_names_its_build_and_protocol() {
        let (mut input, mut reader, handle) = server();
        let response = ask(
            &mut input,
            &mut reader,
            Request::Hello {
                protocol: protocol::PROTOCOL_VERSION,
            },
        );
        match response {
            Response::Hello { protocol, build } => {
                assert_eq!(protocol, protocol::PROTOCOL_VERSION);
                assert!(build.starts_with(env!("CARGO_PKG_VERSION")), "{build}");
            }
            other => panic!("expected a hello, got {other:?}"),
        }
        drop(input);
        handle.join().expect("the server ends").expect("no error");
    }

    #[test]
    fn a_hello_with_another_version_is_answered_with_ours() {
        let (mut input, mut reader, handle) = server();
        let response = ask(&mut input, &mut reader, Request::Hello { protocol: 99 });
        assert!(matches!(
            response,
            Response::Hello {
                protocol: protocol::PROTOCOL_VERSION,
                ..
            }
        ));
        drop(input);
        handle.join().expect("the server ends").expect("no error");
    }

    #[test]
    fn opening_a_directory_without_a_repository_says_why() {
        let (mut input, mut reader, handle) = server();
        let directory = tempfile::tempdir().expect("a directory");
        let response = ask(
            &mut input,
            &mut reader,
            Request::Open {
                path: directory.path().to_path_buf(),
            },
        );
        match response {
            Response::Opened { result } => {
                let failure = result.expect_err("no repository");
                assert!(
                    failure.message.contains("is not inside a Git repository"),
                    "{}",
                    failure.message
                );
            }
            other => panic!("expected an opened response, got {other:?}"),
        }
        drop(input);
        handle.join().expect("the server ends").expect("no error");
    }

    #[test]
    fn a_repository_without_a_manifest_is_refused() {
        let (mut input, mut reader, handle) = server();
        let directory = tempfile::tempdir().expect("a directory");
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(directory.path())
            .status()
            .expect("git runs");
        assert!(status.success(), "git init failed");
        let response = ask(
            &mut input,
            &mut reader,
            Request::Open {
                path: directory.path().to_path_buf(),
            },
        );
        match response {
            Response::Opened { result } => {
                let failure = result.expect_err("no manifest");
                assert!(
                    failure.message.contains("is not a Cargo project"),
                    "{}",
                    failure.message
                );
            }
            other => panic!("expected an opened response, got {other:?}"),
        }
        drop(input);
        handle.join().expect("the server ends").expect("no error");
    }

    #[test]
    fn the_cache_can_be_asked_about_and_cleared() {
        let (mut input, mut reader, handle) = server();
        for request in [Request::CacheStatus, Request::CacheClear] {
            match ask(&mut input, &mut reader, request) {
                Response::Cache { result } => {
                    let status = result.expect("a cache status");
                    assert_eq!(status.graphs, 0);
                }
                other => panic!("expected a cache response, got {other:?}"),
            }
        }
        drop(input);
        handle.join().expect("the server ends").expect("no error");
    }

    #[test]
    fn a_job_cancelled_before_it_started_reports_nothing() {
        let (mut input, mut reader, handle) = server();
        let id = JobId {
            kind: protocol::JobKind::Compare,
            generation: 4,
        };
        // The cancellation arrives before the job that it names.
        protocol::write_message(&mut input, &Request::Cancel { id }).expect("a written request");
        let directory = tempfile::tempdir().expect("a directory");
        protocol::write_message(
            &mut input,
            &Request::Compare {
                id,
                repo: directory.path().to_path_buf(),
                spec: blastr_model::RevSpec {
                    worktree: true,
                    ..blastr_model::RevSpec::default()
                },
                options: AnalysisOptions::default(),
                params: AnalysisParams::default(),
            },
        )
        .expect("a written request");
        // The next request is answered, so the comparison answered nothing.
        let response = ask(&mut input, &mut reader, Request::CacheStatus);
        assert!(matches!(response, Response::Cache { .. }), "{response:?}");
        drop(input);
        handle.join().expect("the server ends").expect("no error");
    }

    #[test]
    fn shutdown_ends_the_server() {
        let (mut input, mut reader, handle) = server();
        protocol::write_message(&mut input, &Request::Shutdown).expect("a written request");
        handle.join().expect("the server ends").expect("no error");
        let response: Option<Response> =
            protocol::read_message(&mut reader).expect("a readable end");
        assert!(response.is_none());
    }

    #[test]
    fn a_closed_input_ends_the_server() {
        let (input, _reader, handle) = server();
        drop(input);
        handle.join().expect("the server ends").expect("no error");
    }

    #[test]
    fn progress_reports_are_throttled() {
        let buffer = Buffer::default();
        let output = Output::new(buffer.clone());
        let id = JobId {
            kind: protocol::JobKind::Compare,
            generation: 1,
        };
        for done in 0..100 {
            output.report(id, "extract head", done, Some(100));
        }
        let written = buffer.bytes();
        let mut bytes = written.as_slice();
        let mut reports = 0;
        while let Some(response) =
            protocol::read_message::<_, Response>(&mut bytes).expect("a readable response")
        {
            assert!(matches!(response, Response::Progress { .. }));
            reports += 1;
        }
        assert!(reports >= 1, "the first report is sent");
        assert!(reports < 100, "the rest are throttled, got {reports}");
    }

    #[test]
    fn a_broken_stream_is_remembered() {
        let output = Output::new(Broken);
        let response = Response::Cache {
            result: Err(Failure::new("none")),
        };
        assert!(output.send(&response).is_err());
        assert!(output.broken.load(Ordering::SeqCst));
        assert!(output.send(&response).is_err());
    }

    #[test]
    fn a_progress_reporter_forwards_phases() {
        let buffer = Buffer::default();
        let reporter = Reporter {
            output: Arc::new(Output::new(buffer.clone())),
            id: JobId {
                kind: protocol::JobKind::WhatIf,
                generation: 2,
            },
        };
        reporter.report("impact", 1, Some(3));
        drop(reporter);
        let written = buffer.bytes();
        let mut bytes = written.as_slice();
        match protocol::read_message::<_, Response>(&mut bytes)
            .expect("a readable response")
            .expect("a report")
        {
            Response::Progress { id, report } => {
                assert_eq!(id.generation, 2);
                assert_eq!(report.phase, "impact");
                assert_eq!((report.done, report.total), (1, Some(3)));
            }
            other => panic!("expected a progress report, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_token_reaches_the_job_it_names() {
        let shared = Shared::default();
        let id = JobId {
            kind: protocol::JobKind::Compare,
            generation: 1,
        };
        shared.cancel(id);
        let early: CancelToken = shared.begin(id);
        assert!(early.is_cancelled());
        shared.end();

        let running = JobId {
            kind: protocol::JobKind::Compare,
            generation: 2,
        };
        let token = shared.begin(running);
        assert!(!token.is_cancelled());
        shared.cancel(running);
        assert!(token.is_cancelled());

        let other = JobId {
            kind: protocol::JobKind::WhatIf,
            generation: 2,
        };
        let untouched = shared.begin(other);
        shared.cancel(running);
        assert!(!untouched.is_cancelled(), "another job is not this one");
    }

    /// What the server wrote, so that a test can read it back.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Buffer {
        /// The bytes written so far, as a stream.
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().expect("the buffer").clone()
        }
    }

    impl io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("the buffer").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// An output that cannot be written to.
    struct Broken;

    impl io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("no pipe"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
