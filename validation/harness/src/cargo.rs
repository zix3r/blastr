//! Running Cargo and reading what it prints.
//!
//! `cargo check --message-format=json` writes one JSON object per line: build
//! artifacts, compiler messages and a final summary. `cargo test` writes the
//! output of one libtest binary after another. Both are parsed here, so that
//! the checks themselves only deal with diagnostics and test names.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::Result;

/// What one process run produced.
pub struct Run {
    /// True when the process exited successfully within the time limit.
    pub success: bool,
    /// Standard output, as UTF-8 (lossy: a compiler may print anything).
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// True when the time limit ran out and the process was killed.
    pub timed_out: bool,
    /// Wall-clock time the run took.
    pub millis: u64,
}

/// One error diagnostic of the compiler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// The error code, e.g. `E0308`; empty when the diagnostic has none.
    pub code: String,
    /// The text of the message.
    pub message: String,
    /// The file of the primary span, absolute when Cargo printed it relatively.
    pub file: Option<PathBuf>,
    /// Byte offset of the first character of the primary span.
    pub start: u32,
    /// Byte offset one past the last character of the primary span.
    pub end: u32,
    /// Line of the primary span, starting at 1.
    pub line: u32,
}

/// One target Cargo built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// The package name, without version or source.
    pub package: String,
    /// The target kind, as Cargo names it (`lib`, `bin`, `test`, ...).
    pub kind: String,
    /// The target name.
    pub target: String,
}

/// What `cargo check` said.
#[derive(Debug, Clone, Default)]
pub struct CheckOutput {
    /// Error diagnostics with a source span.
    pub errors: Vec<Diagnostic>,
    /// Diagnostics Cargo reported without a span, such as "could not compile".
    pub spanless: Vec<String>,
    /// The targets that were built.
    pub artifacts: Vec<Artifact>,
}

/// What `cargo test` said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestOutput {
    /// Names of the tests that failed, sorted and without duplicates.
    pub failed: Vec<String>,
    /// Names of the tests that failed inside a doc-test, which version 1 of
    /// Blastr does not analyse.
    pub failed_doctests: Vec<String>,
    /// How many tests the run reported, over every binary.
    pub total: u32,
}

/// The environment every run gets: no network, no colour, and a target
/// directory that outlives the run.
fn command(program: &str, dir: &Path, target_dir: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("CARGO_TERM_COLOR", "never")
        .env("CARGO_NET_OFFLINE", "true")
        .stdin(Stdio::null());
    command
}

/// Runs a process with a time limit, keeping its output in temporary files so
/// that a killed process cannot deadlock on a full pipe.
pub fn run(command: &mut Command, timeout: Duration) -> Result<Run> {
    let scratch = tempfile::tempdir()?;
    let out_path = scratch.path().join("stdout.txt");
    let err_path = scratch.path().join("stderr.txt");
    command
        .stdout(Stdio::from(std::fs::File::create(&out_path)?))
        .stderr(Stdio::from(std::fs::File::create(&err_path)?));

    let started = Instant::now();
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child: Child = command
        .spawn()
        .map_err(|error| format!("could not start `{program}`: {error}"))?;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let stdout = read_text_file(&out_path);
    let stderr = read_text_file(&err_path);

    Ok(Run {
        success: status.is_some_and(|status| status.success()),
        stdout,
        stderr,
        timed_out,
        millis,
    })
}

/// The text of `path`, or an empty string when it cannot be read as UTF-8.
fn read_text_file(path: &Path) -> String {
    let mut text = String::new();
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_ok() {
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
    text
}

/// Runs `cargo check --all-targets --offline` in `dir` and parses its messages.
pub fn check(dir: &Path, target_dir: &Path, timeout: Duration) -> Result<(CheckOutput, Run)> {
    let mut command = command("cargo", dir, target_dir);
    command.args([
        "check",
        "--all-targets",
        "--offline",
        "--message-format=json",
    ]);
    let run = run(&mut command, timeout)?;
    Ok((parse_check(&run.stdout, dir), run))
}

/// Parses the JSON lines of `cargo check`.
pub fn parse_check(stdout: &str, dir: &Path) -> CheckOutput {
    let mut output = CheckOutput::default();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("reason").and_then(|reason| reason.as_str()) {
            Some("compiler-message") => {
                if let Some(message) = value.get("message") {
                    read_message(message, dir, &mut output);
                }
            }
            Some("compiler-artifact") => {
                let package = value
                    .get("package_id")
                    .and_then(|id| id.as_str())
                    .map(package_name)
                    .unwrap_or_default();
                let target = value.get("target");
                let kind = target
                    .and_then(|target| target.get("kind"))
                    .and_then(|kind| kind.as_array())
                    .and_then(|kinds| kinds.first())
                    .and_then(|kind| kind.as_str())
                    .unwrap_or_default();
                let name = target
                    .and_then(|target| target.get("name"))
                    .and_then(|name| name.as_str())
                    .unwrap_or_default();
                output.artifacts.push(Artifact {
                    package,
                    kind: kind.to_owned(),
                    target: name.to_owned(),
                });
            }
            _ => {}
        }
    }
    output
}

/// Reads one `compiler-message` payload into `output`.
fn read_message(message: &serde_json::Value, dir: &Path, output: &mut CheckOutput) {
    if message.get("level").and_then(|level| level.as_str()) != Some("error") {
        return;
    }
    let text = message
        .get("message")
        .and_then(|text| text.as_str())
        .unwrap_or_default()
        .to_owned();
    let code = message
        .get("code")
        .and_then(|code| code.get("code"))
        .and_then(|code| code.as_str())
        .unwrap_or_default()
        .to_owned();
    let spans: Vec<&serde_json::Value> = message
        .get("spans")
        .and_then(|spans| spans.as_array())
        .map(|spans| spans.iter().collect())
        .unwrap_or_default();
    let primary = spans
        .iter()
        .find(|span| span.get("is_primary").and_then(|flag| flag.as_bool()) == Some(true))
        .or_else(|| spans.first());
    let Some(primary) = primary else {
        output.spanless.push(text);
        return;
    };
    let file = primary
        .get("file_name")
        .and_then(|name| name.as_str())
        .map(|name| {
            let path = Path::new(name);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                dir.join(path)
            }
        });
    let number = |key: &str| {
        primary
            .get(key)
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_default()
    };
    output.errors.push(Diagnostic {
        code,
        message: text,
        file,
        start: number("byte_start"),
        end: number("byte_end"),
        line: number("line_start"),
    });
}

/// The package name of a Cargo package id such as
/// `path+file:///p#tinycore@0.1.0` or `registry+https://…#serde@1.0.0`.
pub fn package_name(id: &str) -> String {
    let tail = id.rsplit('#').next().unwrap_or(id);
    let name = tail.split('@').next().unwrap_or(tail);
    name.to_owned()
}

/// Runs `cargo test --workspace --no-fail-fast` in `dir` and parses its output.
pub fn test(dir: &Path, target_dir: &Path, timeout: Duration) -> Result<(TestOutput, Run)> {
    let mut command = command("cargo", dir, target_dir);
    command.args(["test", "--workspace", "--no-fail-fast", "--offline"]);
    let run = run(&mut command, timeout)?;
    // libtest writes its progress to standard output; Cargo writes the section
    // headers there as well, and everything is read together.
    let mut text = run.stdout.clone();
    text.push('\n');
    text.push_str(&run.stderr);
    Ok((parse_libtest(&text), run))
}

/// Parses libtest output: the failing test names, and how many tests ran.
pub fn parse_libtest(text: &str) -> TestOutput {
    let mut output = TestOutput::default();
    let mut failed = Vec::new();
    let mut doctest_failed = Vec::new();
    let mut in_doctests = false;
    for line in text.lines() {
        let trimmed = line.trim_end();
        let head = trimmed.trim_start();
        if head.starts_with("Doc-tests ") {
            in_doctests = true;
            continue;
        }
        if head.starts_with("Running ") {
            in_doctests = false;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("test ")
            && let Some(name) = rest.strip_suffix(" ... FAILED")
        {
            if in_doctests {
                doctest_failed.push(name.to_owned());
            } else {
                failed.push(name.to_owned());
            }
            continue;
        }
        if head.starts_with("running ")
            && let Some(count) = head
                .strip_prefix("running ")
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|count| count.parse::<u32>().ok())
        {
            output.total = output.total.saturating_add(count);
        }
    }
    failed.sort();
    failed.dedup();
    doctest_failed.sort();
    doctest_failed.dedup();
    output.failed = failed;
    output.failed_doctests = doctest_failed;
    output
}

/// Copies a project directory into `into`, without build outputs and version
/// control data: the checks write into the copy, never into the project.
pub fn copy_project(from: &Path, into: &Path) -> Result<()> {
    const SKIP: &[&str] = &["target", ".git", ".blastr"];
    std::fs::create_dir_all(into)?;
    let mut stack = vec![(from.to_path_buf(), into.to_path_buf())];
    while let Some((source, destination)) = stack.pop() {
        for entry in std::fs::read_dir(&source)? {
            let entry = entry?;
            let name = entry.file_name();
            if SKIP.iter().any(|skip| name.to_str() == Some(skip)) {
                continue;
            }
            let path = entry.path();
            let target = destination.join(&name);
            let kind = entry.file_type()?;
            if kind.is_dir() {
                std::fs::create_dir_all(&target)?;
                stack.push((path, target));
            } else if kind.is_file() {
                std::fs::copy(&path, &target)?;
            }
        }
    }
    Ok(())
}

/// Writing a file, creating the directories above it.
pub fn write_file(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

/// The error codes a Rust compiler reports while resolving names. When a case
/// produces nothing but these, type checking may not have run: rustc replaces a
/// name it could not resolve with an error type and suppresses the errors the
/// code around it would produce, so the error list a `use` of a removed or
/// privatised item produces says nothing about the sites that use it.
pub const RESOLUTION_CODES: &[&str] = &[
    "E0405", // cannot find trait
    "E0412", // cannot find type
    "E0422", // cannot find struct, variant or union
    "E0425", // cannot find value
    "E0432", // unresolved import
    "E0433", // failed to resolve: use of undeclared crate or module
    "E0531", // cannot find tuple struct or tuple variant
    "E0532", // expected struct pattern
    "E0603", // item is private, and cannot be resolved
];

/// Whether a case that produced `codes` may have been stopped at name
/// resolution, so that its error list is not the whole truth.
pub fn stopped_at_resolution(codes: &[String]) -> bool {
    !codes.is_empty()
        && codes
            .iter()
            .all(|code| RESOLUTION_CODES.contains(&code.as_str()))
}

/// The packages Cargo said it was checking, from the progress lines it writes
/// to standard error: `   Checking tinycore v0.1.0 (…)`.
///
/// Cargo emits no `compiler-artifact` message for a unit that failed, and a
/// failing build is exactly when the question "was this crate checked?" has to
/// be answered, so the progress lines are read as well.
pub fn progress_packages(stderr: &str) -> BTreeSet<String> {
    let mut packages = BTreeSet::new();
    for line in stderr.lines() {
        let head = line.trim_start();
        for prefix in ["Checking ", "Compiling ", "Fresh ", "Documenting "] {
            let Some(rest) = head.strip_prefix(prefix) else {
                continue;
            };
            // `name v1.0.0 (path)`; a progress bar line starts with `[`.
            if let Some((name, _)) = rest.split_once(" v") {
                let name = name.trim();
                if !name.is_empty() {
                    packages.insert(name.to_owned());
                }
            }
        }
    }
    packages
}

/// Counts of error codes, for the report.
pub fn error_codes(errors: &[Diagnostic]) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for error in errors {
        *counts.entry(code_text(error)).or_insert(0) += 1;
    }
    counts
}

/// The code of a diagnostic, or `none` when it has none.
pub fn code_text(error: &Diagnostic) -> String {
    if error.code.is_empty() {
        "none".to_owned()
    } else {
        error.code.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_come_out_of_cargo_ids() {
        assert_eq!(
            package_name("path+file:///home/x/p#tinycore@0.1.0"),
            "tinycore"
        );
        assert_eq!(
            package_name("registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0"),
            "serde"
        );
        assert_eq!(package_name("tinycore"), "tinycore");
    }

    #[test]
    fn compiler_messages_are_read_with_their_primary_span() {
        let stdout = concat!(
            r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"mismatched types","spans":[{"is_primary":true,"file_name":"src/lib.rs","byte_start":10,"byte_end":14,"line_start":2}]}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"warning","code":{"code":"unused"},"message":"unused","spans":[]}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"error","code":null,"message":"could not compile `app` (lib)","spans":[]}}"#,
            "\n",
            r#"{"reason":"compiler-artifact","package_id":"path+file:///p#app@0.1.0","target":{"kind":["lib"],"name":"app"}}"#,
            "\n"
        );
        let dir = Path::new("/project");
        let parsed = parse_check(stdout, dir);
        assert_eq!(parsed.errors.len(), 1);
        let error = &parsed.errors[0];
        assert_eq!(error.code, "E0308");
        assert_eq!(error.start, 10);
        assert_eq!(error.end, 14);
        assert_eq!(error.line, 2);
        assert_eq!(
            error.file.as_deref(),
            Some(dir.join("src/lib.rs").as_path())
        );
        assert_eq!(parsed.spanless.len(), 1);
        assert_eq!(parsed.artifacts.len(), 1);
        assert_eq!(parsed.artifacts[0].package, "app");
        assert_eq!(parsed.artifacts[0].kind, "lib");
        assert_eq!(parsed.artifacts[0].target, "app");
    }

    #[test]
    fn absolute_paths_are_kept() {
        let stdout = r#"{"reason":"compiler-message","message":{"level":"error","message":"x","spans":[{"is_primary":true,"file_name":"/abs/src/lib.rs","byte_start":0,"byte_end":1,"line_start":1}]}}"#;
        let parsed = parse_check(stdout, Path::new("/other"));
        assert_eq!(
            parsed.errors[0].file.as_deref(),
            Some(Path::new("/abs/src/lib.rs"))
        );
    }

    #[test]
    fn libtest_output_gives_test_names_and_counts() {
        let text = "\
     Running unittests src/lib.rs (target/debug/deps/app-123)\n\
\n\
running 3 tests\n\
test tests::one ... ok\n\
test tests::two ... FAILED\n\
test tests::three ... ok\n\
\n\
test result: FAILED. 2 passed; 1 failed; 0 ignored\n\
\n\
   Doc-tests app\n\
\n\
running 1 test\n\
test src/lib.rs - example (line 3) ... FAILED\n\
\n\
test result: FAILED. 0 passed; 1 failed;\n";
        let parsed = parse_libtest(text);
        assert_eq!(parsed.failed, vec!["tests::two".to_owned()]);
        assert_eq!(
            parsed.failed_doctests,
            vec!["src/lib.rs - example (line 3)".to_owned()]
        );
        assert_eq!(parsed.total, 4);
    }

    #[test]
    fn resolution_codes_are_recognized() {
        assert!(stopped_at_resolution(&["E0425".to_owned()]));
        assert!(!stopped_at_resolution(&[
            "E0425".to_owned(),
            "E0308".to_owned()
        ]));
        assert!(!stopped_at_resolution(&[]));
    }

    #[test]
    fn error_codes_are_counted() {
        let errors = vec![
            Diagnostic {
                code: "E0308".to_owned(),
                message: String::new(),
                file: None,
                start: 0,
                end: 0,
                line: 1,
            },
            Diagnostic {
                code: "E0308".to_owned(),
                message: String::new(),
                file: None,
                start: 0,
                end: 0,
                line: 1,
            },
            Diagnostic {
                code: String::new(),
                message: String::new(),
                file: None,
                start: 0,
                end: 0,
                line: 1,
            },
        ];
        let counts = error_codes(&errors);
        assert_eq!(counts.get("E0308"), Some(&2));
        assert_eq!(counts.get("none"), Some(&1));
    }
}
