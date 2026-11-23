//! The engine process, driven the way the window drives it.
//!
//! The engine is started as the window starts it and fed the requests the
//! window sends. What it answers must be exactly what [`Engine::analyze`]
//! computes for the same input: the window and a direct call cannot disagree.

use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use blastr_core::Engine;
use blastr_model::protocol::{self, Failure, JobId, JobKind, Request, Response};
use blastr_model::{Analysis, AnalysisOptions, AnalysisParams, CancelToken, NoProgress, RevSpec};

/// The library of the crate before the change.
const BASE: &str = "pub fn total() -> i64 {\n    used(2) + 3\n}\n\n\
                    pub fn used(value: i64) -> i64 {\n    value * 2\n}\n\n\
                    pub fn untouched() -> i64 {\n    7\n}\n\n\
                    #[cfg(test)]\nmod tests {\n    use super::total;\n\n    #[test]\n    fn totals() {\n        assert_eq!(total(), 7);\n    }\n}\n";

/// The library of the crate after it.
const HEAD: &str = "pub fn total() -> i64 {\n    used(2) + 4\n}\n\n\
                    pub fn used(value: i64) -> i64 {\n    value * 3\n}\n\n\
                    pub fn untouched() -> i64 {\n    7\n}\n\n\
                    #[cfg(test)]\nmod tests {\n    use super::total;\n\n    #[test]\n    fn totals() {\n        assert_eq!(total(), 10);\n    }\n}\n";

/// An engine process with its standard streams.
struct Driver {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Driver {
    /// Starts the engine, with its log thrown away.
    fn start() -> Driver {
        let mut child = Command::new(env!("CARGO_BIN_EXE_blastr-engine"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the engine starts");
        let input = BufWriter::new(child.stdin.take().expect("the engine's input"));
        let output = BufReader::new(child.stdout.take().expect("the engine's output"));
        Driver {
            child,
            input,
            output,
        }
    }

    /// Writes one request.
    fn send(&mut self, request: &Request) {
        protocol::write_message(&mut self.input, request).expect("a written request");
    }

    /// The next answer, or `None` when the engine closed its stream.
    fn next(&mut self) -> Option<Response> {
        protocol::read_message(&mut self.output).expect("a readable answer")
    }

    /// Says hello and checks that the engine answers, returning its build.
    fn handshake(&mut self) -> String {
        self.send(&Request::Hello {
            protocol: protocol::PROTOCOL_VERSION,
        });
        match self.next().expect("a hello") {
            Response::Hello { protocol, build } => {
                assert_eq!(protocol, protocol::PROTOCOL_VERSION);
                build
            }
            other => panic!("expected a hello, got {other:?}"),
        }
    }

    /// Ends the engine the way a crash would.
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Writes a small crate and commits it twice, so that `HEAD` differs from
/// `HEAD~1` in two functions.
fn repository(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).expect("create the source directory");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"engine_app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write the manifest");
    std::fs::write(dir.join(".gitignore"), "target\n").expect("write the ignore file");
    std::fs::write(dir.join("src/lib.rs"), BASE).expect("write the library");
    commit(dir, "base");
    std::fs::write(dir.join("src/lib.rs"), HEAD).expect("write the changed library");
    commit(dir, "change the totals");
}

/// Commits everything in `dir`.
fn commit(dir: &Path, message: &str) {
    let steps: [&[&str]; 3] = [
        &["init", "--quiet", "."],
        &["add", "-A"],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            message,
        ],
    ];
    for args in steps {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }
}

/// Opens `dir` and checks what the engine says about it.
fn opened(mut driver: Driver, dir: &Path) -> Driver {
    driver.send(&Request::Open {
        path: dir.to_path_buf(),
    });
    match driver.next().expect("an opened repository") {
        Response::Opened { result } => {
            let opened = result.expect("the fixture is a repository");
            assert_eq!(
                opened.repository.name,
                dir.file_name().expect("a directory name").to_string_lossy()
            );
            assert!(opened.repository.head.is_some(), "a commit to show");
        }
        other => panic!("expected an opened repository, got {other:?}"),
    }
    driver
}

/// The answers of one comparison, up to its result.
fn compare(driver: &mut Driver, dir: &Path, spec: RevSpec) -> Analysis {
    let id = JobId {
        kind: JobKind::Compare,
        generation: 1,
    };
    driver.send(&Request::Compare {
        id,
        repo: dir.to_path_buf(),
        spec,
        options: AnalysisOptions::default(),
        params: AnalysisParams::default(),
    });
    let mut changes = None;
    loop {
        match driver.next().expect("an answer") {
            Response::Progress { .. } => {}
            Response::Changes { diff, .. } => changes = Some(diff),
            Response::Result { analysis, .. } => {
                let analysis = *analysis;
                if let Some(changes) = changes {
                    assert_eq!(changes, analysis.diff, "the changes come first, unchanged");
                }
                return analysis;
            }
            Response::Failed { failure, .. } => panic!("the engine failed: {}", failure.describe()),
            other => panic!("unexpected answer {other:?}"),
        }
    }
}

#[test]
fn the_engine_process_computes_what_a_direct_call_computes() {
    let temp = tempfile::tempdir().expect("a temporary directory");
    repository(temp.path());
    let spec = RevSpec {
        rev: Some("HEAD".to_owned()),
        ..RevSpec::default()
    };

    let mut driver = Driver::start();
    let build = driver.handshake();
    assert!(build.starts_with(blastr_core::VERSION), "{build}");

    let mut driver = opened(driver, temp.path());
    let from_process = compare(&mut driver, temp.path(), spec.clone());

    let cache = tempfile::tempdir().expect("a cache directory");
    let direct = Engine::with_paths(blastr_model::Paths::under(cache.path()))
        .analyze(
            temp.path(),
            &spec,
            &AnalysisOptions::default(),
            &AnalysisParams::default(),
            &NoProgress,
            &CancelToken::new(),
        )
        .expect("a direct analysis");

    assert!(
        !direct.items.is_empty(),
        "the fixture has impact to compare"
    );
    assert_eq!(
        from_process.items, direct.items,
        "the same items, in the same order"
    );
    assert_eq!(from_process.tests, direct.tests, "the same tests");
    assert_eq!(
        from_process.diff.changes, direct.diff.changes,
        "the same changes"
    );
    assert_eq!(from_process.diff.changed_files, direct.diff.changed_files);
    assert_eq!(from_process.diff.comparison, direct.diff.comparison);
    assert_eq!(from_process.diff.limits, direct.diff.limits);
    assert_eq!(from_process.limits, direct.limits);
    assert_eq!(from_process.params, direct.params);
    // The analysis key records a fingerprint of the executable that computed
    // it, and the engine and this test are two such executables, so the keys
    // are the only thing that may differ.
}

#[test]
fn an_engine_killed_mid_run_ends_its_stream_and_another_one_answers() {
    let temp = tempfile::tempdir().expect("a temporary directory");
    repository(temp.path());

    let mut driver = Driver::start();
    driver.handshake();
    // A comparison of the working tree loads the project; it is still loading
    // when the process ends, so no result can reach the window.
    let id = JobId {
        kind: JobKind::Compare,
        generation: 1,
    };
    driver.send(&Request::Compare {
        id,
        repo: temp.path().to_path_buf(),
        spec: RevSpec {
            worktree: true,
            ..RevSpec::default()
        },
        options: AnalysisOptions::default(),
        params: AnalysisParams::default(),
    });
    std::thread::sleep(Duration::from_millis(150));
    driver.kill();

    while let Some(answer) = driver.next() {
        assert!(
            !matches!(answer, Response::Result { .. } | Response::Failed { .. }),
            "a killed engine cannot finish its job: {answer:?}"
        );
    }

    // The window starts a new engine on its next request, and it answers.
    let mut restarted = Driver::start();
    restarted.handshake();
    restarted.send(&Request::CacheStatus);
    match restarted.next().expect("a cache status") {
        Response::Cache { result } => {
            let status = result.expect("a cache status");
            let _ = status.graphs;
        }
        other => panic!("expected a cache status, got {other:?}"),
    }
}

#[test]
fn an_engine_asked_to_shut_down_leaves_its_stream() {
    let mut driver = Driver::start();
    driver.handshake();
    driver.send(&Request::Shutdown);

    assert!(driver.next().is_none(), "the stream ends after a shutdown");
    let status = driver.child.try_wait().expect("the process is readable");
    assert!(
        status.is_some() || driver.child.wait().is_ok(),
        "the engine process ends"
    );
}

/// What a failure says, for a panic message.
trait Describe {
    /// The message and its hint on one line.
    fn describe(&self) -> String;
}

impl Describe for Failure {
    fn describe(&self) -> String {
        match &self.hint {
            Some(hint) => format!("{} ({hint})", self.message),
            None => self.message.clone(),
        }
    }
}
