//! Loads small Cargo projects and checks what the extraction produces.

use std::path::Path;

use blastr_model::{
    CancelToken, Certainty, EdgeDetail, EdgeKind, ExecMode, NoProgress, RelPath, RevisionGraph,
    Symbol, SymbolKind, TargetKind, Visibility,
};
use blastr_ra::{Error, LoadOptions, Workspace};

/// What the loaded project is analysed as: everything on, build scripts run.
fn options() -> LoadOptions {
    LoadOptions {
        features: Vec::new(),
        all_features: false,
        target: None,
        exec: ExecMode::Full,
        target_dir: None,
    }
}

/// Writes a Cargo project with one library crate into `dir`.
fn write_project(dir: &Path, package: &str, library: &str) {
    let manifest = format!(
        r#"[package]
name = "{package}"
version = "0.1.0"
edition = "2024"
"#
    );
    std::fs::create_dir_all(dir.join("src")).expect("the source directory is created");
    std::fs::write(dir.join("Cargo.toml"), manifest).expect("the manifest is written");
    std::fs::write(dir.join("src").join("lib.rs"), library).expect("the library crate is written");
}

/// Loads the project in `dir`.
///
/// A toolchain without the `rust-src` component cannot analyse the standard
/// library at all; the test is skipped instead of failing on such a machine.
fn load(dir: &Path) -> Option<Workspace> {
    let cancel = CancelToken::new();
    match Workspace::load(dir, dir, &options(), &NoProgress, &cancel) {
        Ok(workspace) => Some(workspace),
        Err(Error::MissingRustSrc) => {
            eprintln!("skipped: the active toolchain has no rust-src component");
            None
        }
        Err(error) => panic!("the project failed to load: {error}"),
    }
}

fn extract(workspace: &Workspace) -> RevisionGraph {
    let cancel = CancelToken::new();
    match workspace.extract(&NoProgress, &cancel) {
        Ok(graph) => graph,
        Err(error) => panic!("the extraction failed: {error}"),
    }
}

fn find<'a>(graph: &'a RevisionGraph, path: &str) -> &'a Symbol {
    graph
        .symbols
        .iter()
        .find(|symbol| symbol.path.path == path)
        .unwrap_or_else(|| panic!("no symbol `{path}` in the graph"))
}

fn parent_path<'a>(graph: &'a RevisionGraph, symbol: &Symbol) -> Option<&'a str> {
    let parent = symbol.parent?;
    Some(graph.symbols[parent.0 as usize].path.path.as_str())
}

const LIBRARY: &str = r#"//! A crate with one of everything.

pub mod shapes {
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    impl Point {
        pub fn new(x: i32, y: i32) -> Point {
            Point { x, y }
        }
    }

    impl std::fmt::Display for Point {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "({}, {})", self.x, self.y)
        }
    }
}

pub enum State {
    Idle,
    Busy { progress: u8 },
}

pub trait Greet {
    fn name(&self) -> String;

    fn greet(&self) -> String {
        format!("hello {}", self.name())
    }
}

#[derive(Debug, Clone)]
pub struct Widget {
    pub id: u32,
}

impl Widget {
    pub fn bump(&mut self) {
        self.id += 1;
    }
}

impl Greet for Widget {
    fn name(&self) -> String {
        format!("widget {}", self.id)
    }
}

pub const LIMIT: u32 = 10;

pub fn limit_text() -> String {
    format!("limit {LIMIT}")
}

macro_rules! twice {
    ($value:expr) => {
        $value * 2
    };
}

pub fn doubled(value: u32) -> u32 {
    twice!(value)
}

fn internal() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn doubles() {
        assert_eq!(super::doubled(2), 4);
    }
}
"#;

#[test]
fn extracts_items_paths_flags_hashes_and_edges() {
    let dir = tempfile::tempdir().expect("a temporary directory is created");
    write_project(dir.path(), "my-crate", LIBRARY);
    let Some(workspace) = load(dir.path()) else {
        return;
    };
    let graph = extract(&workspace);

    // The crate list.
    assert_eq!(graph.crates.len(), 1);
    let crate_info = &graph.crates[0];
    assert_eq!(crate_info.name, "my_crate");
    assert_eq!(crate_info.package, "my-crate");
    assert_eq!(crate_info.target_kind, TargetKind::Lib);
    assert_eq!(crate_info.target_name, "my_crate");
    assert_eq!(crate_info.root_file, RelPath::new("src/lib.rs"));
    assert!(!graph.restricted);

    // Every item is where the contract says it is.
    let cases: &[(&str, SymbolKind)] = &[
        ("my_crate", SymbolKind::Module),
        ("my_crate::shapes", SymbolKind::Module),
        ("my_crate::shapes::Point", SymbolKind::Struct),
        ("my_crate::shapes::Point::x", SymbolKind::Field),
        ("my_crate::shapes::Point::y", SymbolKind::Field),
        ("my_crate::shapes::<impl Point>", SymbolKind::Impl),
        ("my_crate::shapes::Point::new", SymbolKind::Function),
        (
            "my_crate::shapes::<impl std::fmt::Display for Point>",
            SymbolKind::Impl,
        ),
        (
            "my_crate::shapes::<impl std::fmt::Display for Point>::fmt",
            SymbolKind::Method,
        ),
        ("my_crate::State", SymbolKind::Enum),
        ("my_crate::State::Idle", SymbolKind::Variant),
        ("my_crate::State::Busy", SymbolKind::Variant),
        ("my_crate::State::Busy::progress", SymbolKind::Field),
        ("my_crate::Greet", SymbolKind::Trait),
        ("my_crate::Greet::name", SymbolKind::Method),
        ("my_crate::Greet::greet", SymbolKind::Method),
        ("my_crate::Widget", SymbolKind::Struct),
        ("my_crate::Widget::id", SymbolKind::Field),
        ("my_crate::<impl Debug for Widget>", SymbolKind::Impl),
        ("my_crate::<impl Clone for Widget>", SymbolKind::Impl),
        ("my_crate::LIMIT", SymbolKind::Const),
        ("my_crate::twice", SymbolKind::Macro),
        ("my_crate::doubled", SymbolKind::Function),
        ("my_crate::internal", SymbolKind::Function),
        ("my_crate::tests", SymbolKind::Module),
        ("my_crate::tests::doubles", SymbolKind::Function),
    ];
    for &(path, kind) in cases {
        let symbol = find(&graph, path);
        assert_eq!(symbol.path.kind, kind, "wrong kind for `{path}`");
        assert_eq!(
            symbol.path.disambiguator, 0,
            "wrong disambiguator for `{path}`"
        );
        assert_eq!(symbol.krate.0, 0, "wrong crate for `{path}`");
    }

    // Every location is in the one file of the crate.
    for symbol in &graph.symbols {
        assert_eq!(
            symbol.location.file,
            RelPath::new("src/lib.rs"),
            "wrong file for `{}`",
            symbol.path.path
        );
        assert!(
            symbol.location.span.start <= symbol.location.span.end,
            "backwards range for `{}`",
            symbol.path.path
        );
        if symbol.parent.is_some() {
            assert!(
                symbol.location.span.start < symbol.location.span.end,
                "empty range for `{}`",
                symbol.path.path
            );
        }
        assert!(symbol.location.span.start_line >= 1);
        assert!(symbol.location.span.end_line >= symbol.location.span.start_line);
    }

    // Parents.
    assert_eq!(find(&graph, "my_crate").parent, None);
    assert_eq!(
        parent_path(&graph, find(&graph, "my_crate::shapes")),
        Some("my_crate")
    );
    assert_eq!(
        parent_path(&graph, find(&graph, "my_crate::shapes::Point::x")),
        Some("my_crate::shapes::Point")
    );
    assert_eq!(
        parent_path(&graph, find(&graph, "my_crate::State::Busy::progress")),
        Some("my_crate::State::Busy")
    );
    assert_eq!(
        parent_path(&graph, find(&graph, "my_crate::shapes::Point::new")),
        Some("my_crate::shapes::<impl Point>")
    );
    assert_eq!(
        parent_path(
            &graph,
            find(
                &graph,
                "my_crate::shapes::<impl std::fmt::Display for Point>::fmt"
            )
        ),
        Some("my_crate::shapes::<impl std::fmt::Display for Point>")
    );
    assert_eq!(
        parent_path(&graph, find(&graph, "my_crate::tests::doubles")),
        Some("my_crate::tests")
    );

    // Flags.
    let test_function = find(&graph, "my_crate::tests::doubles");
    assert!(test_function.flags.is_test);
    assert!(!test_function.flags.is_pub_api);
    let trait_method = find(&graph, "my_crate::Greet::greet");
    assert!(trait_method.flags.in_trait);
    assert!(trait_method.flags.has_body);
    assert!(!trait_method.flags.in_trait_impl);
    let required_method = find(&graph, "my_crate::Greet::name");
    assert!(required_method.flags.in_trait);
    assert!(!required_method.flags.has_body);
    let provided_method = find(
        &graph,
        "my_crate::shapes::<impl std::fmt::Display for Point>::fmt",
    );
    assert!(provided_method.flags.in_trait_impl);
    assert!(provided_method.flags.has_body);
    assert!(!provided_method.flags.in_trait);
    assert!(find(&graph, "my_crate::doubled").flags.is_pub_api);
    assert!(find(&graph, "my_crate::shapes::Point").flags.is_pub_api);
    assert!(!find(&graph, "my_crate::internal").flags.is_pub_api);
    assert!(!find(&graph, "my_crate::internal").flags.is_test);
    assert!(find(&graph, "my_crate::doubled").flags.has_body);
    assert!(find(&graph, "my_crate::LIMIT").flags.has_body);
    assert!(!find(&graph, "my_crate::doubled").flags.from_macro);
    assert!(
        find(&graph, "my_crate::<impl Debug for Widget>")
            .flags
            .from_macro
    );
    assert!(
        find(&graph, "my_crate::<impl Clone for Widget>")
            .flags
            .from_macro
    );

    // Impl info.
    let display_impl = find(
        &graph,
        "my_crate::shapes::<impl std::fmt::Display for Point>",
    );
    let info = display_impl
        .impl_info
        .as_ref()
        .expect("impls have impl info");
    assert_eq!(info.trait_path.as_deref(), Some("core::fmt::Display"));
    assert_eq!(info.self_path.as_deref(), Some("my_crate::shapes::Point"));
    let inherent = find(&graph, "my_crate::shapes::<impl Point>");
    let info = inherent.impl_info.as_ref().expect("impls have impl info");
    assert_eq!(info.trait_path, None);
    assert_eq!(info.self_path.as_deref(), Some("my_crate::shapes::Point"));
    let derive = find(&graph, "my_crate::<impl Debug for Widget>");
    let info = derive.impl_info.as_ref().expect("impls have impl info");
    assert!(
        info.trait_path
            .as_deref()
            .is_some_and(|path| path.ends_with("::Debug")),
        "unexpected derive trait path: {:?}",
        info.trait_path
    );
    assert_eq!(info.self_path.as_deref(), Some("my_crate::Widget"));
    let clone = find(&graph, "my_crate::<impl Clone for Widget>");
    let info = clone.impl_info.as_ref().expect("impls have impl info");
    assert!(
        info.trait_path
            .as_deref()
            .is_some_and(|path| path.ends_with("::Clone")),
        "unexpected derive trait path: {:?}",
        info.trait_path
    );

    // Visibility.
    assert_eq!(
        find(&graph, "my_crate::shapes::Point").visibility,
        Visibility::Public
    );

    // Hashes, as the table in the contract says.
    let doubled = find(&graph, "my_crate::doubled");
    assert_ne!(doubled.signature_hash, 0);
    assert_ne!(doubled.body_hash, 0);
    assert!(!doubled.body_sketch.is_empty());
    let point = find(&graph, "my_crate::shapes::Point");
    assert_ne!(point.signature_hash, 0);
    assert_eq!(point.body_hash, 0);
    assert!(point.body_sketch.is_empty());
    assert_ne!(find(&graph, "my_crate::shapes::Point::x").signature_hash, 0);
    assert_eq!(find(&graph, "my_crate::shapes::Point::x").body_hash, 0);
    assert_ne!(find(&graph, "my_crate::State").signature_hash, 0);
    assert_ne!(find(&graph, "my_crate::State::Busy").signature_hash, 0);
    assert_ne!(find(&graph, "my_crate::Greet").signature_hash, 0);
    assert_eq!(find(&graph, "my_crate::Greet").body_hash, 0);
    assert_ne!(find(&graph, "my_crate::LIMIT").signature_hash, 0);
    assert_ne!(find(&graph, "my_crate::LIMIT").body_hash, 0);
    // The name of a macro is the only thing but its rules, and a signature
    // never holds the item's own name.
    assert_eq!(find(&graph, "my_crate::twice").signature_hash, 0);
    assert_ne!(find(&graph, "my_crate::twice").body_hash, 0);
    assert_ne!(
        find(&graph, "my_crate::shapes::<impl Point>").signature_hash,
        0
    );
    let root = find(&graph, "my_crate");
    assert_eq!(root.signature_hash, 0);
    assert_eq!(root.body_hash, 0);
    assert_eq!(root.attr_hash, 0);

    // The struct contains each of its fields.
    let point = find(&graph, "my_crate::shapes::Point");
    let point_id = symbol_id(&graph, point);
    for path in ["my_crate::shapes::Point::x", "my_crate::shapes::Point::y"] {
        let field = find(&graph, path);
        let field_id = symbol_id(&graph, field);
        let edge = graph
            .edges
            .iter()
            .find(|edge| {
                edge.kind == EdgeKind::Contains && edge.from == point_id && edge.to == field_id
            })
            .unwrap_or_else(|| panic!("no `Contains` edge to `{path}`"));
        assert_eq!(edge.certainty, Certainty::Resolved);
        assert_eq!(edge.detail, EdgeDetail::None);
        assert_eq!(edge.site, field.location);
    }

    // Every item except a crate root is contained by something.
    for symbol in &graph.symbols {
        let contained = graph
            .edges
            .iter()
            .any(|edge| edge.kind == EdgeKind::Contains && edge.to == symbol_id(&graph, symbol));
        assert_eq!(
            contained,
            symbol.parent.is_some(),
            "wrong `Contains` edges for `{}`",
            symbol.path.path
        );
    }

    // Calls, including one written inside a macro call.
    assert_edge(
        &graph,
        "my_crate::tests::doubles",
        "my_crate::doubled",
        EdgeKind::Calls,
        Certainty::Resolved,
    );
    assert_edge(
        &graph,
        "my_crate::Greet::greet",
        "my_crate::Greet::name",
        EdgeKind::Calls,
        Certainty::Resolved,
    );

    // A call through a trait method declaration reaches every implementation.
    assert_edge(
        &graph,
        "my_crate::Greet::greet",
        "my_crate::<impl Greet for Widget>::name",
        EdgeKind::MayDispatchTo,
        Certainty::Candidate,
    );
    assert_edge(
        &graph,
        "my_crate::<impl Greet for Widget>",
        "my_crate::Greet",
        EdgeKind::Implements,
        Certainty::Resolved,
    );
    assert_edge(
        &graph,
        "my_crate::<impl Greet for Widget>::name",
        "my_crate::Greet::name",
        EdgeKind::Implements,
        Certainty::Resolved,
    );

    // Field accesses: a read, a write and the fields of a literal.
    assert_edge(
        &graph,
        "my_crate::shapes::<impl std::fmt::Display for Point>::fmt",
        "my_crate::shapes::Point::x",
        EdgeKind::ReadsField,
        Certainty::Resolved,
    );
    assert_edge(
        &graph,
        "my_crate::Widget::bump",
        "my_crate::Widget::id",
        EdgeKind::WritesField,
        Certainty::Resolved,
    );
    assert_edge(
        &graph,
        "my_crate::shapes::Point::new",
        "my_crate::shapes::Point",
        EdgeKind::Constructs,
        Certainty::Resolved,
    );

    // A constant captured by a format string is a value reference as well.
    assert_edge(
        &graph,
        "my_crate::limit_text",
        "my_crate::LIMIT",
        EdgeKind::UsesValue,
        Certainty::Resolved,
    );

    // A field named in a literal is neither read nor written by it.
    let new = symbol_id(&graph, find(&graph, "my_crate::shapes::Point::new"));
    assert!(
        !graph
            .edges
            .iter()
            .any(|edge| edge.from == new && edge.kind == EdgeKind::ReadsField),
        "a struct literal must not read its fields"
    );
}

/// Asserts that the graph has a resolved edge of `kind` between two paths.
fn assert_edge(graph: &RevisionGraph, from: &str, to: &str, kind: EdgeKind, certainty: Certainty) {
    let from_id = symbol_id(graph, find(graph, from));
    let to_id = symbol_id(graph, find(graph, to));
    let edge = graph
        .edges
        .iter()
        .find(|edge| edge.from == from_id && edge.to == to_id && edge.kind == kind)
        .unwrap_or_else(|| panic!("no `{kind}` edge from `{from}` to `{to}`"));
    assert_eq!(edge.certainty, certainty);
    assert_eq!(edge.detail, EdgeDetail::None);
    assert_eq!(edge.site.file, RelPath::new("src/lib.rs"));
    assert!(edge.site.span.start < edge.site.span.end);
}

fn symbol_id(graph: &RevisionGraph, symbol: &Symbol) -> blastr_model::SymbolId {
    let index = graph
        .symbols
        .iter()
        .position(|candidate| std::ptr::eq(candidate, symbol))
        .expect("the symbol is in the graph");
    blastr_model::SymbolId(index as u32)
}

const NAMES: &str = r#"pub fn first(value: u32) -> u32 {
    value * 2
}

pub fn second(value: u32) -> u32 {
    value * 2
}
"#;

#[test]
fn a_rename_keeps_the_hashes() {
    let dir = tempfile::tempdir().expect("a temporary directory is created");
    write_project(dir.path(), "names", NAMES);
    let Some(workspace) = load(dir.path()) else {
        return;
    };
    let graph = extract(&workspace);

    let first = find(&graph, "names::first");
    let second = find(&graph, "names::second");
    // The name of an item is in its path, not in its hashes.
    assert_eq!(first.signature_hash, second.signature_hash);
    assert_eq!(first.body_hash, second.body_hash);
    assert_eq!(first.body_sketch, second.body_sketch);
}

const FORMATTING: &str = r#"pub mod first {
    pub fn compute(x: u32) -> u32 {
        x + 1
    }
}

pub mod reformatted {
    /// Documentation is a comment and is not hashed.
    pub fn compute(  x : u32 ) -> u32 {

        // A comment inside the body.
        x+1
    }
}

pub mod changed {
    pub fn compute(x: u32) -> u32 {
        x + 2
    }
}
"#;

#[test]
fn hashes_ignore_formatting_and_follow_the_body() {
    let dir = tempfile::tempdir().expect("a temporary directory is created");
    write_project(dir.path(), "formatting", FORMATTING);
    let Some(workspace) = load(dir.path()) else {
        return;
    };
    let graph = extract(&workspace);

    let first = find(&graph, "formatting::first::compute");
    let reformatted = find(&graph, "formatting::reformatted::compute");
    let changed = find(&graph, "formatting::changed::compute");

    // Whitespace and comments do not take part in any hash.
    assert_eq!(first.signature_hash, reformatted.signature_hash);
    assert_eq!(first.body_hash, reformatted.body_hash);
    assert_eq!(first.attr_hash, reformatted.attr_hash);
    assert_eq!(first.body_sketch, reformatted.body_sketch);

    // A changed body changes only the body hash.
    assert_eq!(first.signature_hash, changed.signature_hash);
    assert_eq!(first.attr_hash, changed.attr_hash);
    assert_ne!(first.body_hash, changed.body_hash);
    assert_ne!(first.body_sketch, changed.body_sketch);
}

const CFG_DISABLED: &str = r#"pub struct Thing;

pub trait Visitor {
    type Error;

    fn handle(&self) -> Result<(), Self::Error>;
}

/// Generates two copies of one item, one of them disabled.
macro_rules! make {
    ($method:ident) => {
        #[cfg(any())]
        fn $method<V>(self, visitor: V) -> Result<(), V::Error>
        where
            V: Visitor,
        {
            visitor.handle()
        }

        #[cfg(not(any()))]
        fn $method<V>(self, visitor: V) -> Result<(), V::Error>
        where
            V: Visitor,
        {
            visitor.handle()
        }
    };
}

impl Thing {
    make!(run);
}

#[cfg(any())]
mod disabled {
    pub fn gone(missing: Missing) -> Thing {
        missing.call()
    }
}

pub fn broken() {
    missing_function();
}

pub fn broken_in_macro() {
    assert_eq!(also_missing(), 1);
}
"#;

#[test]
fn only_references_that_do_not_resolve_at_all_are_reported() {
    let dir = tempfile::tempdir().expect("a temporary directory is created");
    write_project(dir.path(), "disabled", CFG_DISABLED);
    let Some(workspace) = load(dir.path()) else {
        return;
    };
    let graph = extract(&workspace);

    let mut reported: Vec<&str> = graph
        .unresolved
        .iter()
        .map(|item| item.text.as_str())
        .collect();
    reported.sort_unstable();
    // Disabled code is not resolved by rust-analyzer at all, so nothing in it is
    // reported; what the compiler could not resolve either still is.
    assert_eq!(
        reported,
        ["also_missing", "missing_function"],
        "unexpected unresolved references"
    );
    // The enabled copy of the generated item still produces its edges.
    assert_edge(
        &graph,
        "disabled::Thing::run",
        "disabled::Visitor::handle",
        EdgeKind::Calls,
        Certainty::Resolved,
    );
}
