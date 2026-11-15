//! Integration tests of the command line.
//!
//! Every test here stops before an analysis loads a Cargo project: they check
//! the interface, the argument checks and the errors a run can meet before it
//! looks at code, so the suite stays fast.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn blastr() -> Command {
    assert_cmd::cargo::cargo_bin_cmd!("blastr")
}

/// Runs the command in a directory that is not a repository, where every
/// analysis stops; this proves the arguments were accepted.
fn assert_runs_outside_a_repository(args: &[&str]) {
    let temp = tempfile::tempdir().expect("create a temporary directory");
    blastr()
        .current_dir(temp.path())
        .args(args)
        .assert()
        .code(1)
        .stderr(contains("is not inside a Git repository").and(contains("hint:")))
        .stderr(contains("not implemented").not());
}

/// Creates a temporary directory holding a new Git repository.
fn git_repository() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("create a temporary directory");
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(temp.path())
        .status()
        .expect("run git init");
    assert!(status.success(), "git init failed");
    temp
}

#[test]
fn version() {
    blastr()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("blastr 0.1.0"));
}

#[test]
fn help_lists_every_command() {
    let assert = blastr().arg("--help").assert().success();
    let stdout = assert.get_output().stdout.clone();
    let stdout = String::from_utf8(stdout).expect("help output is UTF-8");
    for command in [
        "analyze",
        "diff",
        "tests",
        "impact",
        "explain",
        "index",
        "cache",
        "gui",
        "completions",
    ] {
        assert!(stdout.contains(command), "help is missing {command}");
    }
    assert!(
        stdout.contains("Run without a command to open the GUI."),
        "help is missing the GUI hint"
    );
}

#[test]
fn help_lists_global_options() {
    let assert = blastr().arg("--help").assert().success();
    let stdout = assert.get_output().stdout.clone();
    let stdout = String::from_utf8(stdout).expect("help output is UTF-8");
    for option in [
        "--manifest-path",
        "--features",
        "--all-features",
        "--target",
        "--format",
        "--depth",
        "--min-confidence",
        "--all",
        "--fail-on",
        "--no-exec",
        "--verbose",
    ] {
        assert!(stdout.contains(option), "help is missing {option}");
    }
}

#[test]
fn every_analysis_command_has_help() {
    for command in ["analyze", "tests", "impact", "explain"] {
        blastr()
            .args([command, "--help"])
            .assert()
            .success()
            .stdout(contains("Usage:"))
            .stdout(contains("--format"));
    }
}

#[test]
fn the_analysis_commands_fail_outside_a_repository() {
    for args in [
        &["analyze"][..],
        &["tests"][..],
        &["impact", "crate::foo"][..],
        &["explain", "crate::foo"][..],
    ] {
        let temp = tempfile::tempdir().expect("create a temporary directory");
        blastr()
            .current_dir(temp.path())
            .args(args)
            .assert()
            .code(1)
            .stdout(predicates::str::is_empty())
            .stderr(contains("is not inside a Git repository"))
            .stderr(contains("hint:"))
            .stderr(contains("not implemented").not());
    }
}

#[test]
fn revision_arguments_are_accepted() {
    for args in [
        &["analyze", "HEAD~1"][..],
        &["analyze", "main..HEAD"][..],
        &["analyze", "--worktree"][..],
        &["analyze", "--staged"][..],
        &["tests", "HEAD", "--parent", "2"][..],
        &["explain", "crate::foo", "HEAD~1"][..],
        &["analyze", "HEAD"][..],
    ] {
        assert_runs_outside_a_repository(args);
    }
}

#[test]
fn revision_arguments_conflict() {
    for args in [
        &["analyze", "--worktree", "--staged"][..],
        &["analyze", "HEAD", "--worktree"][..],
    ] {
        blastr()
            .args(args)
            .assert()
            .code(1)
            .stderr(contains("cannot be used with"));
    }
}

#[test]
fn global_options_work_before_and_after_the_command() {
    assert_runs_outside_a_repository(&["--format", "json", "analyze"]);
    assert_runs_outside_a_repository(&[
        "analyze",
        "--format",
        "json",
        "--depth",
        "3",
        "--min-confidence",
        "likely",
        "--features",
        "a,b",
        "--all-features",
        "--no-exec",
        "--all",
        "--fail-on",
        "behaviour",
        "-vv",
    ]);
}

#[test]
fn diff_outside_a_repository_fails_with_a_hint() {
    let temp = tempfile::tempdir().expect("create a temporary directory");
    blastr()
        .current_dir(temp.path())
        .arg("diff")
        .assert()
        .code(1)
        .stdout(predicates::str::is_empty())
        .stderr(contains("is not inside a Git repository"))
        .stderr(contains("hint:"));
}

#[test]
fn diff_rejects_the_markdown_format() {
    let temp = tempfile::tempdir().expect("create a temporary directory");
    blastr()
        .current_dir(temp.path())
        .args(["diff", "--format", "markdown"])
        .assert()
        .code(1)
        .stdout(predicates::str::is_empty())
        .stderr(contains("markdown").and(contains("not available")))
        .stderr(contains("hint:"));
}

#[test]
fn analyze_and_impact_reject_the_cargo_format() {
    for command in ["analyze", "impact"] {
        blastr()
            .args([command, "crate::foo", "--format", "cargo"])
            .assert()
            .code(1)
            .stdout(predicates::str::is_empty())
            .stderr(contains("only available for `blastr tests`"))
            .stderr(contains("hint:"));
    }
}

#[test]
fn explain_rejects_every_format_but_human() {
    for format in ["json", "markdown", "cargo"] {
        blastr()
            .args(["explain", "crate::foo", "--format", format])
            .assert()
            .code(1)
            .stdout(predicates::str::is_empty())
            .stderr(contains(format!("the {format} format is not available")))
            .stderr(contains("hint:"));
    }
}

#[test]
fn a_depth_outside_the_range_is_rejected() {
    for depth in ["0", "65"] {
        blastr()
            .args(["analyze", "--depth", depth])
            .assert()
            .code(1)
            .stdout(predicates::str::is_empty())
            .stderr(contains("--depth must be between 1 and 64"))
            .stderr(contains("hint:"));
    }
}

#[test]
fn an_unknown_configuration_key_is_named_with_a_suggestion() {
    let repo = git_repository();
    std::fs::write(repo.path().join("blastr.toml"), "deoth = 3\n")
        .expect("write the configuration file");

    blastr()
        .current_dir(repo.path())
        .arg("analyze")
        .assert()
        .code(1)
        .stdout(predicates::str::is_empty())
        .stderr(contains("unknown key"))
        .stderr(contains("deoth"))
        .stderr(contains("did you mean `depth`?"));
}

#[test]
fn cache_status_reports_the_cache() {
    blastr()
        .args(["cache", "status"])
        .assert()
        .success()
        .stdout(contains("CACHE"))
        .stdout(contains("graphs"))
        .stdout(contains("snapshots"))
        .stdout(contains("results"))
        .stdout(contains("database"));
}

#[test]
fn bad_values_are_usage_errors() {
    for args in [
        &["diff", "--format", "xml"][..],
        &["diff", "--min-confidence", "maybe"][..],
        &["impact", "crate::foo", "--assume", "nonsense"][..],
        &["frobnicate"][..],
        &["impact"][..],
    ] {
        blastr()
            .args(args)
            .assert()
            .code(1)
            .stderr(contains("not implemented yet").not())
            .stderr(predicates::str::is_empty().not());
    }
}

#[test]
fn assume_kinds_are_accepted() {
    for kind in [
        "body",
        "signature",
        "removed",
        "visibility",
        "field-removed",
        "field-retyped",
        "field-added",
        "variant-added",
        "variant-removed",
    ] {
        assert_runs_outside_a_repository(&["impact", "crate::foo", "--assume", kind]);
    }
}

#[test]
fn completions_print_a_script() {
    for shell in ["bash", "fish", "powershell", "zsh"] {
        blastr()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicates::str::is_empty().not())
            .stdout(contains("blastr"));
    }
}

#[cfg(not(feature = "gui"))]
#[test]
fn no_command_prints_help_without_gui() {
    blastr().assert().success().stdout(contains("Usage:"));
}

#[cfg(not(feature = "gui"))]
#[test]
fn gui_command_fails_without_gui() {
    blastr()
        .arg("gui")
        .assert()
        .code(1)
        .stderr(contains("this build of blastr has no GUI"));
}
