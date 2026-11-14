use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn blastr() -> Command {
    assert_cmd::cargo::cargo_bin_cmd!("blastr")
}

fn assert_reaches_stub(args: &[&str]) {
    blastr()
        .args(args)
        .assert()
        .code(1)
        .stderr(contains("not implemented yet").and(contains("Usage:").not()));
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
fn stub_commands_are_not_implemented() {
    let cases: &[(&[&str], &str)] = &[
        (&["analyze"], "analyze"),
        (&["diff"], "diff"),
        (&["tests"], "tests"),
        (&["impact", "crate::foo"], "impact"),
        (&["explain", "crate::foo"], "explain"),
        (&["index"], "index"),
        (&["cache", "status"], "cache status"),
        (&["cache", "clear"], "cache clear"),
    ];
    for (args, name) in cases {
        blastr()
            .args(*args)
            .assert()
            .code(1)
            .stdout(predicates::str::is_empty())
            .stderr(contains(format!("`{name}` is not implemented yet")));
    }
}

#[test]
fn revision_arguments_are_accepted() {
    for args in [
        &["analyze", "HEAD~1"][..],
        &["analyze", "main..HEAD"][..],
        &["diff", "--worktree"][..],
        &["diff", "--staged"][..],
        &["tests", "HEAD", "--parent", "2"][..],
        &["explain", "crate::foo", "HEAD~1"][..],
        &["index", "HEAD"][..],
    ] {
        assert_reaches_stub(args);
    }
}

#[test]
fn revision_arguments_conflict() {
    for args in [
        &["diff", "--worktree", "--staged"][..],
        &["diff", "HEAD", "--worktree"][..],
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
    assert_reaches_stub(&["--format", "json", "diff"]);
    assert_reaches_stub(&[
        "diff",
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
        assert_reaches_stub(&["impact", "crate::foo", "--assume", kind]);
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
