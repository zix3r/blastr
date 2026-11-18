//! A load that lost a procedural macro says so instead of failing.
//!
//! rust-analyzer expands a macro whose procedural macro failed to load to
//! nothing, and reports no error of its own. A graph made from such a project
//! is quietly shorter than the graph of the same project loaded properly: every
//! item the macro generates is missing. What such a load lost is reported, not
//! refused, and only the macros that the extracted code can call count.
//!
//! The tests load projects whose procedural macro cannot be built; a machine
//! without `rust-src` cannot load a project at all, so they are skipped there.

use std::fs;
use std::path::Path;

use blastr_model::{CancelToken, ExecMode, NoProgress, RevisionGraph};
use blastr_ra::{Error, LoadOptions, Workspace};

/// Writes one file, creating the directories it needs.
fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create the directory");
    }
    fs::write(path, text).expect("write the file");
}

/// What is loaded: everything on, build scripts and macros run.
fn options(root: &Path) -> LoadOptions {
    LoadOptions {
        features: Vec::new(),
        all_features: false,
        target: None,
        exec: ExecMode::Full,
        target_dir: Some(root.join("target")),
    }
}

/// Loads the project in `root`, skipping the test on a toolchain without
/// `rust-src`.
fn load(root: &Path) -> Option<Workspace> {
    let cancel = CancelToken::new();
    match Workspace::load(root, root, &options(root), &NoProgress, &cancel) {
        Ok(workspace) => Some(workspace),
        Err(Error::MissingRustSrc) => {
            eprintln!("skipped: the active toolchain has no rust-src component");
            None
        }
        Err(error) => panic!("the project failed to load: {error}"),
    }
}

/// Extracts `workspace`.
fn extract(workspace: &Workspace) -> RevisionGraph {
    let cancel = CancelToken::new();
    match workspace.extract(&NoProgress, &cancel) {
        Ok(graph) => graph,
        Err(error) => panic!("the extraction failed: {error}"),
    }
}

/// The paths of the symbols the graph has.
fn paths(graph: &RevisionGraph) -> Vec<&str> {
    graph
        .symbols
        .iter()
        .map(|symbol| symbol.path.path.as_str())
        .collect()
}

#[test]
fn a_broken_procedural_macro_of_a_dependency_is_a_limit() {
    let temp = tempfile::tempdir().expect("create a temporary directory");
    let root = temp.path();
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nbroken_macro = { path = \"broken_macro\" }\n",
    );
    // The macro would generate an item here, which a load that lost it cannot
    // produce.
    write(
        &root.join("src/lib.rs"),
        "broken_macro::make_item!();\n\npub fn value() -> u32 {\n    1\n}\n",
    );
    write(
        &root.join("broken_macro/Cargo.toml"),
        "[package]\nname = \"broken_macro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\nproc-macro = true\n",
    );
    // A procedural macro that cannot be compiled, so Cargo never produces the
    // dylib rust-analyzer would load it from.
    write(
        &root.join("broken_macro/src/lib.rs"),
        "pub fn make_item() { this is not Rust }\n",
    );

    let Some(workspace) = load(root) else {
        return;
    };
    assert_eq!(
        workspace.missing_proc_macros(),
        ["broken_macro"],
        "the load has to name the macro it could not expand"
    );

    // The project is still analysed, and it does not pretend to have the item
    // the macro would have generated.
    let graph = extract(&workspace);
    let paths = paths(&graph);
    assert!(paths.contains(&"app::value"), "the crate is in the graph");
    assert!(
        !paths.iter().any(|path| path.contains("generated")),
        "the macro expanded to nothing, so nothing it generates is in the graph"
    );
}

#[test]
fn a_procedural_macro_no_local_crate_uses_is_not_a_limit() {
    // A workspace whose second member is a broken procedural macro that the
    // first does not depend on. Its macros can only expand inside a dependency
    // crate, which Blastr does not extract, so its failure leaves the graph of
    // the workspace complete.
    let temp = tempfile::tempdir().expect("create a temporary directory");
    let root = temp.path();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"broken_macro\"]\nresolver = \"2\"\n",
    );
    write(
        &root.join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        &root.join("app/src/lib.rs"),
        "pub fn value() -> u32 {\n    1\n}\n",
    );
    write(
        &root.join("broken_macro/Cargo.toml"),
        "[package]\nname = \"broken_macro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\nproc-macro = true\n",
    );
    write(
        &root.join("broken_macro/src/lib.rs"),
        "pub fn make_item() { this is not Rust }\n",
    );

    let Some(workspace) = load(root) else {
        return;
    };
    assert_eq!(
        workspace.missing_proc_macros(),
        [] as [String; 0],
        "a macro no crate of the workspace calls cannot make its graph incomplete"
    );
    let graph = extract(&workspace);
    assert!(paths(&graph).contains(&"app::value"));
}
