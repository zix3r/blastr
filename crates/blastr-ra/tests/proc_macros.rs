//! A load that lost a procedural macro says so instead of failing, and a
//! procedural macro a workspace crate defines is a definition like any other.
//!
//! rust-analyzer expands a macro whose procedural macro failed to load to
//! nothing, and reports no error of its own. A graph made from such a project
//! is quietly shorter than the graph of the same project loaded properly: every
//! item the macro generates is missing. What such a load lost is reported, not
//! refused, and only the macros that the extracted code can call count.
//!
//! The tests load projects whose procedural macro cannot be built, and a project
//! whose macros do build; a machine without `rust-src` cannot load a project at
//! all, so they are skipped there.

use std::fs;
use std::path::Path;

use blastr_model::{CancelToken, Certainty, EdgeKind, ExecMode, NoProgress, RevisionGraph};
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
fn a_procedural_macro_is_a_definition_that_reaches_what_invokes_it() {
    // A workspace with a procedural-macro crate and the crate that uses it. The
    // macros work on tokens only, so the crate builds offline in a second.
    let temp = tempfile::tempdir().expect("create a temporary directory");
    let root = temp.path();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"macros\"]\nresolver = \"2\"\n",
    );
    write(
        &root.join("macros/Cargo.toml"),
        "[package]\nname = \"macros\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\nproc-macro = true\n",
    );
    write(
        &root.join("macros/src/lib.rs"),
        r#"use proc_macro::{TokenStream, TokenTree};

/// Derives `app::Greet` for a struct.
#[proc_macro_derive(Greet)]
pub fn derive_greet(input: TokenStream) -> TokenStream {
    let mut after_struct = false;
    let mut name = String::new();
    for tree in input {
        let TokenTree::Ident(ident) = tree else { continue };
        let text = ident.to_string();
        if text == "struct" {
            after_struct = true;
        } else if after_struct {
            name = text;
            break;
        }
    }
    format!("impl Greet for {name} {{ fn greet(&self) -> u8 {{ 1 }} }}")
        .parse()
        .expect("the generated impl parses")
}

/// Keeps the annotated item and adds a constant next to it.
#[proc_macro_attribute]
pub fn tagged(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut output = item;
    output.extend(
        "pub const TAGGED_MARK: u8 = 3;"
            .parse::<TokenStream>()
            .expect("the generated constant parses"),
    );
    output
}

/// Generates an expression.
#[proc_macro]
pub fn make_item(_input: TokenStream) -> TokenStream {
    "1 + 3"
        .parse()
        .expect("the generated expression parses")
}
"#,
    );
    write(
        &root.join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\nmacros = { path = \"../macros\" }\n",
    );
    write(
        &root.join("app/src/lib.rs"),
        r#"use macros::{Greet, make_item, tagged};

pub trait Greet {
    fn greet(&self) -> u8;
}

// A derive of another crate adds no edge: only a macro of a workspace member
// has a definition in the graph.
#[derive(Debug, Clone, Greet)]
pub struct Point {
    pub x: u8,
}

#[tagged]
pub fn handler() -> u8 {
    2
}

pub fn made() -> u8 {
    make_item!()
}

pub fn greet_point(point: &Point) -> u8 {
    point.greet()
}
"#,
    );

    let Some(workspace) = load(root) else {
        return;
    };
    let graph = extract(&workspace);

    // The proc-macro functions are the definitions of the macros, so they are
    // the symbols an invocation reaches, and a macro and its function are one
    // symbol rather than two.
    let mut expands: Vec<(&str, &str)> = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Expands)
        .map(|edge| {
            assert_eq!(edge.certainty, Certainty::Resolved);
            (
                graph.symbols[edge.from.0 as usize].path.path.as_str(),
                graph.symbols[edge.to.0 as usize].path.path.as_str(),
            )
        })
        .collect();
    expands.sort_unstable();
    assert_eq!(
        expands,
        [
            // The item the derive is written on, and the impl and the method
            // the derive generated for it.
            ("app::<impl Greet for Point>", "macros::derive_greet"),
            ("app::<impl Greet for Point>::greet", "macros::derive_greet"),
            ("app::Point", "macros::derive_greet"),
            // The item the attribute macro is written on, and the constant it
            // generated next to it.
            ("app::TAGGED_MARK", "macros::tagged"),
            ("app::handler", "macros::tagged"),
            // The function whose body invokes a function-like macro.
            ("app::made", "macros::make_item"),
        ],
        "unexpected `Expands` edges"
    );

    // The derives of the standard library are not definitions of the workspace,
    // so they add nothing.
    assert!(
        !expands
            .iter()
            .any(|(_, to)| to.starts_with("std") || *to == "app::Greet"),
        "a derive of another crate has no definition to reach"
    );
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
