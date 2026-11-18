//! End-to-end tests: the harness binary, run on the small project in
//! `testdata/tiny`.
//!
//! The project is copied into a temporary directory first. A Cargo project that
//! sits inside the directory of another Cargo workspace — which `testdata` does,
//! the harness being a workspace of its own — loses a crate when Blastr loads
//! it, so the copy is made outside that tree; see the README's known limits.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The binary this test belongs to.
const BINARY: &str = env!("CARGO_BIN_EXE_blastr-validate");

/// The small two-crate project, copied out of the harness's workspace tree.
fn project(dir: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/tiny");
    let target = dir.join("tiny");
    copy(&source, &target);
    target
}

/// Copies a directory tree, skipping build output.
fn copy(from: &Path, into: &Path) {
    std::fs::create_dir_all(into).expect("create the target directory");
    for entry in std::fs::read_dir(from).expect("read the source directory") {
        let entry = entry.expect("read a directory entry");
        let name = entry.file_name();
        if name.to_str() == Some("target") {
            continue;
        }
        let path = entry.path();
        let destination = into.join(&name);
        if entry.file_type().expect("a file type").is_dir() {
            copy(&path, &destination);
        } else {
            std::fs::copy(&path, &destination).expect("copy a file");
        }
    }
}

/// Runs the harness with the given arguments and returns its stdout.
fn run(args: &[&str]) -> String {
    let output = Command::new(BINARY)
        .args(args)
        .output()
        .expect("the harness starts");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "blastr-validate {args:?} failed:\n{stdout}\n{stderr}"
    );
    stdout
}

/// Reads a JSON report written by the harness.
fn read_report(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).expect("the report is written");
    serde_json::from_str(&text).expect("the report is JSON")
}

#[test]
fn the_usage_text_is_printed_without_arguments() {
    let stdout = run(&[]);
    assert!(stdout.contains("blastr-validate"));
    assert!(stdout.contains("compiler"));
}

#[test]
fn the_compiler_check_scores_cases_and_records_them() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let project = project(dir.path());
    let out = dir.path().join("compiler.json");
    let stdout = run(&[
        "compiler",
        project.to_str().expect("a UTF-8 path"),
        "--limit",
        "6",
        "--out",
        out.to_str().expect("a UTF-8 path"),
    ]);
    assert!(stdout.contains("cargo check"), "{stdout}");
    let report = read_report(&out);
    assert_eq!(report["project"], "tiny");
    assert_eq!(report["check"], "compiler");
    assert_eq!(report["totals"]["cases"], 6);
    assert!(
        report["crates"].as_u64().unwrap_or_default() >= 2,
        "both crates are in the graph: {report}"
    );
    // The load is recorded, as is the version the result came from.
    assert!(
        !report["blastr_version"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    );
    assert!(
        !report["rust_analyzer"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    );
    // Every case records the edit, so a miss can be reproduced from the report.
    let cases = report["cases"].as_array().expect("the cases are a list");
    assert_eq!(cases.len(), 6);
    for case in cases {
        assert!(!case["id"].as_str().unwrap_or_default().is_empty());
        assert!(!case["before"].as_str().unwrap_or_default().is_empty());
        // `after` is empty for the edits that remove the item outright.
        let kind = case["kind"].as_str().unwrap_or_default();
        let after = case["after"].as_str().unwrap_or_default();
        assert!(
            !after.is_empty() || kind.starts_with("remove-"),
            "a case that does not remove something shows its result: {case}"
        );
    }
}

#[test]
fn one_case_can_be_rerun_by_id() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let project = project(dir.path());
    let out = dir.path().join("one.json");
    run(&[
        "compiler",
        project.to_str().expect("a UTF-8 path"),
        "--limit",
        "1",
        "--out",
        out.to_str().expect("a UTF-8 path"),
    ]);
    let report = read_report(&out);
    let id = report["cases"][0]["id"]
        .as_str()
        .expect("the case has an id")
        .to_owned();
    let stdout = run(&[
        "compiler",
        project.to_str().expect("a UTF-8 path"),
        "--case",
        &id,
    ]);
    assert!(stdout.contains(&id), "{stdout}");
    assert!(stdout.contains("reachable"), "{stdout}");
}

#[test]
fn the_test_check_runs_the_suite_and_the_mutants() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let project = project(dir.path());
    let out = dir.path().join("tests.json");
    let stdout = run(&[
        "tests",
        project.to_str().expect("a UTF-8 path"),
        "--limit",
        "3",
        "--out",
        out.to_str().expect("a UTF-8 path"),
    ]);
    assert!(stdout.contains("baseline:"), "{stdout}");
    let report = read_report(&out);
    assert_eq!(report["check"], "tests");
    assert_eq!(report["totals"]["cases"], 3);
    assert!(
        report["baseline_test_ms"].as_u64().unwrap_or_default() > 0,
        "the baseline runs are timed: {report}"
    );
}
