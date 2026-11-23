//! A graph made without a procedural macro is reported, never cached.
//!
//! rust-analyzer expands a macro whose dylib failed to load to nothing. The
//! analysis goes on and says what it is missing, and because such a graph is
//! incomplete it is not stored: the next run tries the project again instead of
//! answering with what this machine could not build.

use std::fs;
use std::path::Path;
use std::process::Command;

use blastr_core::Engine;
use blastr_model::{AnalysisOptions, AnalysisParams, CancelToken, Limit, NoProgress, RevSpec};

/// Writes one file, creating the directories it needs.
fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create the directory");
    }
    fs::write(path, text).expect("write the file");
}

/// Runs one Git command in `dir`.
fn git(dir: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(arguments)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {arguments:?} failed");
}

/// A repository whose crate calls a procedural macro that cannot be built.
fn repository(dir: &Path) {
    write(
        &dir.join("Cargo.toml"),
        "[package]\nname = \"macro_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nbroken_macro = { path = \"broken_macro\" }\n",
    );
    write(
        &dir.join("src/lib.rs"),
        "broken_macro::make_item!();\n\npub fn value() -> u32 {\n    1\n}\n",
    );
    write(
        &dir.join("broken_macro/Cargo.toml"),
        "[package]\nname = \"broken_macro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\nproc-macro = true\n",
    );
    write(
        &dir.join("broken_macro/src/lib.rs"),
        "pub fn make_item() { this is not Rust }\n",
    );
    write(&dir.join(".gitignore"), "target\n");
    git(dir, &["init", "--quiet", "."]);
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "base",
        ],
    );
    // A working-tree change, so that the comparison has something to classify.
    write(
        &dir.join("src/lib.rs"),
        "broken_macro::make_item!();\n\npub fn value() -> u32 {\n    2\n}\n",
    );
}

/// The crates named by the macros limit of `analysis`.
fn missing_macros(analysis: &blastr_model::Analysis) -> Option<Vec<String>> {
    analysis.diff.limits.iter().find_map(|limit| match limit {
        Limit::MacrosNotExpanded { crates } => Some(crates.clone()),
        _ => None,
    })
}

#[test]
fn an_incomplete_graph_is_reported_and_not_cached() {
    let temp = tempfile::tempdir().expect("create a temporary directory");
    let dir = temp.path().join("repo");
    repository(&dir);

    // Snapshot and target directories live outside the repository, or their
    // files would be part of the working tree that is analysed.
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

    let analysis = match engine.analyze(
        &dir,
        &spec,
        &AnalysisOptions::default(),
        &AnalysisParams::default(),
        &NoProgress,
        &cancel,
    ) {
        Ok(analysis) => analysis,
        // A toolchain without `rust-src` cannot load a project at all.
        Err(error) if format!("{error}").contains("standard library") => {
            eprintln!("skipped: the active toolchain has no rust-src component");
            return;
        }
        Err(error) => panic!("the analysis failed: {error}"),
    };

    assert_eq!(
        missing_macros(&analysis),
        Some(vec!["broken_macro".to_owned()]),
        "the analysis has to say which macro it could not expand"
    );
    // The run went on: the crate is still analysed.
    assert!(
        analysis
            .diff
            .changes
            .iter()
            .any(|change| change.symbol.path.contains("macro_app::value")),
        "the changed function is still classified"
    );

    // Nothing was stored, so the next run tries the project again.
    assert_eq!(engine.cache_status().unwrap().graphs, 0);
    assert_eq!(engine.cache_status().unwrap().results, 0);

    let again = engine
        .analyze(
            &dir,
            &spec,
            &AnalysisOptions::default(),
            &AnalysisParams::default(),
            &NoProgress,
            &cancel,
        )
        .expect("the second analysis runs");
    assert_eq!(
        missing_macros(&again),
        Some(vec!["broken_macro".to_owned()]),
        "the second run says the same thing"
    );
    assert_eq!(engine.cache_status().unwrap().graphs, 0);
    assert_eq!(engine.cache_status().unwrap().results, 0);
}
