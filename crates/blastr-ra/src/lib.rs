//! Blastr's adapter to rust-analyzer.
//!
//! This is the only crate that depends on the `ra_ap_*` crates; every other
//! crate talks to it through plain Rust types, because no rust-analyzer type
//! ever appears in this crate's public API.
//!
//! [`Workspace::load`] loads a Cargo project into a rust-analyzer database and
//! [`Workspace::extract`] turns the loaded project into a
//! [`blastr_model::RevisionGraph`]. The extraction rules (which items become
//! symbols, how their paths and hashes are built) are fixed by the stage 2
//! contract; the modules below split the work up:
//!
//! - `load` discovers and loads the Cargo project,
//! - `apply` brings edits of the source files into the loaded project,
//! - `extract` walks the loaded crates and assembles the graph,
//! - `pipeline` runs the phases of an extraction on several threads,
//! - `stats` times the phases of an extraction when a caller asks for it,
//! - `symbols` turns one rust-analyzer definition into one symbol,
//! - `paths` builds the canonical paths,
//! - `hashing` hashes syntax tokens,
//! - `locate` maps file ids and text ranges to locations,
//! - `edges` is the hook for the dependency edges.

#![forbid(unsafe_code)]

mod apply;
mod edges;
mod extract;
mod hashing;
mod load;
mod locate;
mod paths;
mod pipeline;
mod stats;
mod symbols;

use std::path::PathBuf;

use blastr_model::{CancelToken, ExecMode, RelPath};
use ra_ap_ide::AnalysisHost;
use ra_ap_load_cargo::SourceRootConfig;
use ra_ap_syntax::{Edition, SourceFile};
use ra_ap_vfs::Vfs;
// Not used directly: the dependency exists to hold the version back, see the
// workspace manifest.
use unicode_properties as _;

use crate::load::MemberCrate;

pub use crate::stats::{ExtractReport, FileTiming, PhaseTiming};

/// The version of the rust-analyzer libraries this build uses.
pub const RUST_ANALYZER_VERSION: &str = "0.0.358";

/// How a project is loaded and which configuration it is analysed under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadOptions {
    /// Cargo features to enable.
    pub features: Vec<String>,
    /// Enable every feature instead of [`LoadOptions::features`].
    pub all_features: bool,
    /// Target triple to analyse for; `None` means the host.
    pub target: Option<String>,
    /// Whether build scripts and procedural macros may run.
    pub exec: ExecMode,
    /// Value for `CARGO_TARGET_DIR` while loading; `None` leaves Cargo's default.
    pub target_dir: Option<PathBuf>,
}

/// How an extraction runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtractOptions {
    /// The number of worker threads that walk the files; `None` uses the number
    /// of cores the machine has.
    pub threads: Option<usize>,
    /// Whether to record an [`ExtractReport`] of the run.
    ///
    /// Recording costs a clock reading per query, so it is meant for timing a
    /// project rather than for production runs.
    pub measure: bool,
}

/// Everything that can go wrong while loading or extracting a project.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The directory does not contain a `Cargo.toml`.
    #[error("no `Cargo.toml` found in `{}`", .dir.display())]
    NoManifest {
        /// The directory that was searched.
        dir: PathBuf,
    },
    /// Cargo itself failed, or the project could not be loaded.
    #[error("cargo failed to load the project: {0}")]
    Cargo(String),
    /// The Rust toolchain could not be queried or does not work.
    #[error("the Rust toolchain could not be used: {0}")]
    Toolchain(String),
    /// The active toolchain has no `rust-src` component.
    #[error("the standard library sources are not installed")]
    MissingRustSrc,
    /// The caller cancelled through their [`CancelToken`].
    #[error("cancelled")]
    Cancelled,
    /// Blastr's own state is inconsistent, or a feature is not available yet.
    #[error("analysis failed: {0}")]
    Internal(String),
}

/// One edit applied on top of a loaded project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    /// The file to change, relative to the repository root.
    pub path: RelPath,
    /// New content; `None` removes the file.
    pub content: Option<String>,
}

/// A Cargo project loaded into rust-analyzer.
///
/// The rust-analyzer database and the virtual file system both live in here, so
/// later stages can apply edits and re-extract without loading Cargo again.
pub struct Workspace {
    /// The loaded database, with all analysis state.
    host: AnalysisHost,
    /// The files rust-analyzer knows about.
    vfs: Vfs,
    /// The directory the reported paths are relative to.
    root: PathBuf,
    /// The file-set configuration that decides which source root a file belongs
    /// to, so that an added file lands in the same root a fresh load would use.
    source_roots: SourceRootConfig,
    /// `rustc --version` of the toolchain the project was loaded with.
    toolchain: String,
    /// The workspace-member crates, sorted by `(package, target kind, target name)`.
    crates: Vec<MemberCrate>,
    /// True when build scripts and procedural macros were not run.
    restricted: bool,
    /// The procedural-macro crates a crate of this workspace depends on
    /// directly whose macros did not load, sorted and without duplicates.
    missing_proc_macros: Vec<String>,
}

impl Workspace {
    /// The toolchain version the project was loaded with, e.g. `rustc 1.100.0 (...)`.
    pub fn toolchain(&self) -> &str {
        &self.toolchain
    }

    /// The crates whose procedural macros the analysed code calls but did not
    /// get, sorted and without duplicates.
    ///
    /// rust-analyzer expands a macro whose dylib failed to load to nothing and
    /// reports no error, so the items such a macro would generate are missing
    /// from the graph. Empty means nothing the extracted code can call was
    /// lost. Never set when procedural macros were not run at all, because
    /// [`blastr_model::Limit::Restricted`] already says so then.
    pub fn missing_proc_macros(&self) -> &[String] {
        &self.missing_proc_macros
    }
}

/// Returns true when `text` parses as a Rust source file without syntax errors.
pub fn parse_ok(text: &str) -> bool {
    SourceFile::parse(text, Edition::CURRENT)
        .errors()
        .is_empty()
}

/// Reports cancellation of long-running work.
fn check_cancel(cancel: &CancelToken) -> Result<(), Error> {
    cancel.check().map_err(|_| Error::Cancelled)
}

#[cfg(test)]
mod tests {
    use super::{Error, LoadOptions, Workspace, parse_ok};
    use blastr_model::RevisionGraph;

    fn assert_send<T: Send>() {}

    #[test]
    fn public_types_are_send() {
        assert_send::<Workspace>();
        assert_send::<Error>();
        assert_send::<LoadOptions>();
        assert_send::<RevisionGraph>();
    }

    #[test]
    fn accepts_valid_snippet() {
        assert!(parse_ok("fn main() { let x = 1; }"));
    }

    #[test]
    fn accepts_empty_text() {
        assert!(parse_ok(""));
    }

    #[test]
    fn rejects_unclosed_parameter_list() {
        assert!(!parse_ok("fn main( {"));
    }

    #[test]
    fn rejects_struct_without_name() {
        assert!(!parse_ok("struct ;"));
    }
}
