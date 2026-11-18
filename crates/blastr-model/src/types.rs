//! The data types every crate shares: symbols, edges, graphs, changes and
//! the identity of an analysis.
//!
//! All types serialise with `serde`. Graphs are stored with `postcard`, which
//! is not self-describing, so no field here may use `skip_serializing_if`,
//! `flatten`, `untagged` or a default that changes the encoded shape.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Certainty, ChangeKind, EdgeKind, ExecMode, SymbolKind};

/// Index of a symbol in [`RevisionGraph::symbols`]. Only meaningful inside one graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SymbolId(pub u32);

/// Index of a crate in [`RevisionGraph::crates`]. Only meaningful inside one graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CrateId(pub u32);

/// A file path relative to the repository root, always with `/` separators.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct RelPath(pub String);

/// A range of source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    /// Byte offset of the first character.
    pub start: u32,
    /// Byte offset one past the last character.
    pub end: u32,
    /// Line of the first character, starting at 1.
    pub start_line: u32,
    /// Line of the last character, starting at 1.
    pub end_line: u32,
}

/// A range of source text in a file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Location {
    /// The file, relative to the repository root.
    pub file: RelPath,
    /// The range inside the file.
    pub span: Span,
}

/// The identity of a symbol across revisions.
///
/// `path` is the canonical path, starting with the crate name:
/// `billing::discount::apply_discount`. The exact format is fixed by the
/// extraction rules; two revisions of the same item produce the same value.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct SymbolPath {
    /// Canonical path, beginning with the crate name.
    pub path: String,
    /// What the path names.
    pub kind: SymbolKind,
    /// Tells apart items that would otherwise have the same path and kind,
    /// counted in source order starting at 0.
    pub disambiguator: u32,
}

/// How widely an item can be named, from narrowest to widest.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Visibility {
    /// Visible in its own module only.
    Private,
    /// `pub(super)` or `pub(in path)`.
    Restricted,
    /// `pub(crate)`.
    Crate,
    /// `pub`.
    Public,
}

/// Facts about a symbol that later stages branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct SymbolFlags {
    /// A function carrying a test attribute (last path segment `test`).
    pub is_test: bool,
    /// The `main` function of a binary target.
    pub is_main: bool,
    /// Public and reachable from the root of a library crate.
    pub is_pub_api: bool,
    /// A function with a body, or a const or static with an initialiser.
    pub has_body: bool,
    /// Declared inside a `trait` block.
    pub in_trait: bool,
    /// Declared inside an `impl Trait for Type` block.
    pub in_trait_impl: bool,
    /// Produced by a macro or a derive rather than written out.
    pub from_macro: bool,
}

/// What an `impl` block implements. Present on symbols of kind `Impl` only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImplInfo {
    /// Canonical path of the implemented trait, `None` for an inherent impl.
    /// The trait may be defined outside the workspace (`core::fmt::Display`).
    pub trait_path: Option<String>,
    /// Canonical path of the self type when it is a struct, enum or union.
    pub self_path: Option<String>,
}

/// One item of the analysed project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    /// Identity across revisions; also carries the kind.
    pub path: SymbolPath,
    /// The crate the symbol belongs to.
    pub krate: CrateId,
    /// Where the whole item is in the source.
    pub location: Location,
    /// Declared visibility.
    pub visibility: Visibility,
    /// Hash of the item's interface. See the extraction rules for what each
    /// kind includes. 0 when the kind has none.
    pub signature_hash: u64,
    /// Hash of the item's body or initialiser. 0 when there is none.
    pub body_hash: u64,
    /// Hash of the item's attributes other than `derive` and `doc`. 0 when there are none.
    pub attr_hash: u64,
    /// Sketch of the body used to estimate similarity between two bodies.
    /// Empty when there is no body.
    pub body_sketch: Vec<u32>,
    /// The enclosing symbol: module, type, trait or impl.
    pub parent: Option<SymbolId>,
    /// Facts later stages branch on.
    pub flags: SymbolFlags,
    /// Set on `Impl` symbols only.
    pub impl_info: Option<ImplInfo>,
}

/// The kind of Cargo target a crate was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TargetKind {
    /// A library.
    Lib,
    /// A binary.
    Bin,
    /// An integration test in `tests/`.
    Test,
    /// An example.
    Example,
    /// A benchmark.
    Bench,
    /// A procedural-macro library.
    ProcMacro,
    /// A build script.
    BuildScript,
    /// Anything else.
    Other,
}

/// One crate of the analysed project. Only workspace members are recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrateInfo {
    /// The crate name as it appears in paths (underscores, not dashes).
    pub name: String,
    /// The Cargo package the target belongs to, as `cargo -p` accepts it:
    /// the package name, or `name@version` when a dependency of the project
    /// has the same name.
    pub package: String,
    /// The kind of target.
    pub target_kind: TargetKind,
    /// The target name, as `cargo test --test <name>` expects it.
    pub target_name: String,
    /// The crate root file.
    pub root_file: RelPath,
}

/// Extra information some edges carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeDetail {
    /// Nothing extra.
    None,
    /// A `Matches` edge from a `match` that lists the variants and has no catch-all arm.
    MatchExhaustive,
    /// A `Matches` edge from a `match` that has a catch-all arm.
    MatchWildcard,
    /// A `Constructs` or `Matches` edge to a struct whose literal or pattern
    /// ends with `..`, so it does not name every field.
    WithRest,
}

/// A dependency: `from` depends on `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// The symbol that contains the reference.
    pub from: SymbolId,
    /// The symbol referred to.
    pub to: SymbolId,
    /// What kind of reference it is.
    pub kind: EdgeKind,
    /// Whether the target is the one definition or one of several candidates.
    pub certainty: Certainty,
    /// Where the reference is written.
    pub site: Location,
    /// Extra information for some kinds.
    pub detail: EdgeDetail,
}

/// A reference that could not be resolved to a definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedRef {
    /// Where the reference is written.
    pub location: Location,
    /// The text of the reference.
    pub text: String,
}

/// Everything extracted from one revision of a project under one configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RevisionGraph {
    /// Workspace-member crates.
    pub crates: Vec<CrateInfo>,
    /// Symbols; [`SymbolId`] indexes this.
    pub symbols: Vec<Symbol>,
    /// Dependency edges.
    pub edges: Vec<Edge>,
    /// References that did not resolve.
    pub unresolved: Vec<UnresolvedRef>,
    /// Source ranges that are inactive under the analysed configuration.
    pub inactive: Vec<Location>,
    /// True when build scripts and procedural macros were not run.
    pub restricted: bool,
}

/// What the user asked to be analysed, independent of the revision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnalysisOptions {
    /// Cargo features to enable.
    pub features: Vec<String>,
    /// Enable every feature.
    pub all_features: bool,
    /// Target triple; `None` means the host.
    pub target: Option<String>,
    /// Whether build scripts and procedural macros run.
    pub exec: ExecMode,
    /// Directory containing the `Cargo.toml` to load, relative to the
    /// repository root; `None` means the root itself.
    pub manifest_dir: Option<RelPath>,
    /// Whether a comparison of the working tree keeps following it: the
    /// engine then recomputes when a saved file changes it.
    pub watch: bool,
}

/// Identifies exactly what a result describes. Two results with equal keys
/// describe the same thing and may be shared.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisKey {
    /// Identifies the repository.
    pub repository: String,
    /// Identifies the content analysed: a tree hash, or a content-derived
    /// identifier for the index or the working tree.
    pub source: String,
    /// Enabled features, sorted.
    pub features: Vec<String>,
    /// Whether every feature was enabled.
    pub all_features: bool,
    /// Target triple; `None` means the host.
    pub target: Option<String>,
    /// Whether `cfg(test)` was on.
    pub cfg_test: bool,
    /// Hash of `Cargo.lock`, empty when there is none.
    pub cargo_lock: String,
    /// The Rust toolchain version.
    pub toolchain: String,
    /// The rust-analyzer library version.
    pub rust_analyzer: String,
    /// The Blastr build: its version and a fingerprint of the executable,
    /// so that a rebuilt Blastr never reads what an older build computed.
    pub blastr: String,
    /// Whether build scripts and procedural macros ran.
    pub exec: ExecMode,
    /// The manifest directory analysed.
    pub manifest_dir: Option<RelPath>,
}

/// One side of a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// A committed tree.
    Tree {
        /// The tree's object id, in hexadecimal.
        id: String,
        /// How to show it to the user: a short commit id or the revision text.
        label: String,
    },
    /// The Git index (staged content).
    Index,
    /// The working tree.
    Worktree,
    /// A plain directory outside Git. Used by tests.
    Dir(PathBuf),
}

/// The two sides being compared.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Comparison {
    /// The older side.
    pub base: Source,
    /// The newer side.
    pub head: Source,
}

/// What the user typed to choose a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RevSpec {
    /// A revision (`HEAD~1`) or a range (`main..HEAD`).
    pub rev: Option<String>,
    /// Compare `HEAD` with the working tree.
    pub worktree: bool,
    /// Compare `HEAD` with the index.
    pub staged: bool,
    /// Which parent of a merge commit to compare against, starting at 1.
    pub parent: Option<u32>,
}

/// How a file differs between the two sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FileStatus {
    /// Present on the head side only.
    Added,
    /// Present on both sides with different content.
    Modified,
    /// Present on the base side only.
    Deleted,
    /// Moved; `old_path` is set.
    Renamed,
}

/// A file that differs between the two sides.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ChangedFile {
    /// The path on the head side, or on the base side for a deleted file.
    pub path: RelPath,
    /// The path on the base side when the file was renamed.
    pub old_path: Option<RelPath>,
    /// How the file differs.
    pub status: FileStatus,
}

/// One commit, for listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Full object id, in hexadecimal.
    pub id: String,
    /// Abbreviated object id.
    pub short_id: String,
    /// First line of the message.
    pub subject: String,
    /// Author name.
    pub author: String,
    /// Author time, in seconds since the Unix epoch.
    pub time: i64,
    /// Number of parents.
    pub parents: u32,
}

/// Whether something appeared, disappeared or was altered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Delta {
    /// Present on the head side only.
    Added,
    /// Present on the base side only.
    Removed,
    /// Present on both sides and different.
    Modified,
}

/// The specifics of a change, depending on its kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeDetail {
    /// The kind says everything.
    None,
    /// For `Field`, `Variant`, `TraitItem` and `Impl`: what happened to the member.
    Member(Delta),
    /// For `TraitItem`: only the body of a provided method changed.
    DefaultBody,
    /// For `Renamed`: the previous identity.
    Renamed {
        /// The symbol's path on the base side.
        from: SymbolPath,
    },
    /// For `Visibility`: the two values.
    Visibility {
        /// Visibility on the base side.
        from: Visibility,
        /// Visibility on the head side.
        to: Visibility,
    },
    /// For `Manifest`: a one-line description.
    Manifest(String),
    /// For `ConstValue`: the declared type changed, not only the value.
    Retyped,
}

/// One structural difference between the two sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Change {
    /// What kind of change it is.
    pub kind: ChangeKind,
    /// The symbol changed: its head-side identity, or its base-side identity
    /// when it no longer exists.
    pub symbol: SymbolPath,
    /// Where the symbol is on the base side.
    pub old: Option<Location>,
    /// Where the symbol is on the head side.
    pub new: Option<Location>,
    /// The specifics.
    pub detail: ChangeDetail,
}

/// Something that limits how complete a result is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Limit {
    /// A changed range lies in code that is inactive under this configuration.
    ChangedButInactive(Location),
    /// Build scripts and procedural macros were not run.
    Restricted,
    /// The repository has submodules, which are not analysed.
    Submodules,
    /// The repository uses Git LFS, whose content is not analysed.
    Lfs,
    /// References that could not be resolved.
    Unresolved {
        /// How many.
        count: u32,
    },
    /// The procedural macros of crates the analysed code uses could not be
    /// loaded, so everything they expand to is missing from the graph.
    MacrosNotExpanded {
        /// The crates whose macros failed, sorted and without duplicates.
        crates: Vec<String>,
    },
}

/// The result of comparing two sides structurally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiffResult {
    /// What was compared.
    pub comparison: Comparison,
    /// Identity of the base analysis; `None` for a plain directory.
    pub base_key: Option<AnalysisKey>,
    /// Identity of the head analysis; `None` for a plain directory.
    pub head_key: Option<AnalysisKey>,
    /// Files that differ.
    pub changed_files: Vec<ChangedFile>,
    /// Structural changes, in a stable order.
    pub changes: Vec<Change>,
    /// What limits the result.
    pub limits: Vec<Limit>,
}
