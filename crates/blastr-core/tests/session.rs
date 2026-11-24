//! A session keeps a working tree loaded and reports what a save affects.
//!
//! The session is what the window's refresh on save runs on: the project is
//! loaded once, and every later refresh re-extracts the saved files only. This
//! test uses a real Git repository with one small crate, so it costs one
//! project load.

use std::fs;
use std::path::Path;
use std::process::Command;

use blastr_core::{AnalysisRequest, Engine};
use blastr_model::{
    AnalysisOptions, AnalysisParams, CancelToken, ImpactClass, NoProgress, RevSpec,
};

/// The library of the crate under test.
const LIB: &str = "pub fn total() -> i64 {\n    used(2) + 3\n}\n\n\
                   pub fn used(value: i64) -> i64 {\n    value * 2\n}\n\n\
                   #[cfg(test)]\nmod tests {\n    use super::total;\n\n    #[test]\n    fn totals() {\n        assert_eq!(total(), 7);\n    }\n}\n";

/// Writes the crate and commits it.
fn repository(dir: &Path) {
    fs::create_dir_all(dir.join("src")).expect("create the source directory");
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"session_app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write the manifest");
    fs::write(dir.join("src/lib.rs"), LIB).expect("write the library");
    fs::write(dir.join(".gitignore"), "target\n").expect("write the ignore file");
    for args in [
        ["init", "--quiet", "."].as_slice(),
        ["add", "-A"].as_slice(),
        [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "base",
        ]
        .as_slice(),
    ] {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }
}

#[test]
fn a_session_reports_what_a_save_affects() {
    let temp = tempfile::tempdir().expect("create a temporary directory");
    let dir = temp.path();
    repository(dir);

    // The cache lives outside the repository, or its files would be part of
    // the working tree the session watches.
    let outside = tempfile::tempdir().expect("create a second temporary directory");
    let paths = blastr_model::Paths {
        cache_dir: outside.path().join("cache"),
        config_dir: outside.path().join("config"),
    };
    let engine = Engine::with_paths(paths);
    let cancel = CancelToken::new();
    let spec = RevSpec {
        worktree: true,
        ..RevSpec::default()
    };
    let request = AnalysisRequest {
        repo_root: dir,
        spec: &spec,
        options: &AnalysisOptions::default(),
        params: &AnalysisParams::default(),
    };
    let mut session = engine
        .session(&request, &NoProgress, &cancel, &mut |_| {})
        .expect("the session opens");

    // Nothing is edited yet, so nothing changed and nothing is affected.
    assert!(session.analysis().diff.changes.is_empty());
    assert!(session.analysis().items.is_empty());
    assert!(!session.is_stale().expect("the working tree is readable"));

    // A save that changes one body reaches its caller and its test.
    fs::write(
        dir.join("src/lib.rs"),
        LIB.replace("value * 2", "value * 3"),
    )
    .expect("write the library");
    assert!(session.is_stale().expect("the working tree is readable"));
    let refresh = session
        .refresh(&NoProgress, &cancel, &mut |_| {})
        .expect("the session refreshes")
        .clone();
    // Cargo writes a `Cargo.lock` while the project is analysed, so the change
    // count is not a fixed number; the edited source has to be among them.
    assert!(
        refresh
            .diff
            .changed_files
            .iter()
            .any(|file| file.path.as_str() == "src/lib.rs")
    );
    let changes: Vec<String> = refresh
        .diff
        .changes
        .iter()
        .map(|change| change.symbol.path.clone())
        .collect();
    assert_eq!(changes, ["session_app::used"]);
    let affected: Vec<(&str, ImpactClass)> = refresh
        .items
        .iter()
        .map(|item| (item.symbol.path.as_str(), item.class))
        .collect();
    assert_eq!(
        affected,
        [
            ("session_app::total", ImpactClass::Behaviour),
            ("session_app::tests::totals", ImpactClass::Behaviour),
        ]
    );
    assert!(!session.is_stale().expect("the working tree is readable"));

    // Editing something nothing depends on leaves the impact as it was.
    fs::write(dir.join("README.md"), "Notes.\n").expect("write a note");
    let refresh = session
        .refresh(&NoProgress, &cancel, &mut |_| {})
        .expect("the session refreshes")
        .clone();
    assert!(
        refresh
            .diff
            .changed_files
            .iter()
            .any(|file| file.path.as_str() == "README.md")
    );
    assert_eq!(refresh.items.len(), 2);

    // Reverting the edit returns to a tree with no structural change.
    fs::write(dir.join("src/lib.rs"), LIB).expect("write the library");
    let refresh = session
        .refresh(&NoProgress, &cancel, &mut |_| {})
        .expect("the session refreshes")
        .clone();
    assert!(refresh.diff.changes.is_empty());
    assert!(refresh.items.is_empty());
}
