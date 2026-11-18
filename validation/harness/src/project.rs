//! The project under test: one load, many edited variants, and a writable copy
//! for the compiler.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use blastr_core::{Engine, FileEdit, ProjectBase, Variant};
use blastr_model::{
    AnalysisOptions, AnalysisParams, CancelToken, NoProgress, RelPath, RevisionGraph,
};
use blastr_store::{Paths, Store};

use crate::Result;
use crate::cargo;

/// A project the checks run against.
///
/// The directory itself is never written to. rust-analyzer loads it once and
/// every case is an in-memory variant of that load; the compiler runs in a
/// copy, which is written and restored per case.
pub struct Project {
    /// The directory the project was read from.
    dir: PathBuf,
    /// The project, loaded once.
    base: ProjectBase,
    /// A writable copy of the project for `cargo`.
    copy: PathBuf,
    /// The target directory all Cargo runs of this project share.
    target_dir: PathBuf,
    /// The revision of the sample, as git names it.
    revision: String,
    /// How long loading and extracting the project took, in milliseconds.
    load_ms: u64,
    /// Holds the temporary directories the copy and the cache live in.
    _scratch: tempfile::TempDir,
}

impl Project {
    /// Loads the project in `dir` and prepares a copy for the compiler.
    pub fn open(dir: &Path) -> Result<Project> {
        let dir = dir
            .canonicalize()
            .map_err(|error| format!("`{}` cannot be read: {error}", dir.display()))?;
        let scratch = tempfile::tempdir()?;
        let cache = scratch.path().join("cache");
        let target_dir = scratch.path().join("target");
        let copy = scratch.path().join("copy");
        std::fs::create_dir_all(&cache)?;
        std::fs::create_dir_all(&target_dir)?;
        cargo::copy_project(&dir, &copy)?;

        let paths = Paths {
            cache_dir: cache.clone(),
            config_dir: cache.join("config"),
        };
        let store = Store::open_in_memory()?;
        let engine = Engine::with_store_and_paths(store, paths);
        let options = AnalysisOptions::default();
        let cancel = CancelToken::new();
        let started = std::time::Instant::now();
        let base = engine.project_base(&dir, &options, &NoProgress, &cancel)?;
        let load_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(Project {
            revision: revision_of(&dir),
            dir,
            base,
            copy,
            target_dir,
            load_ms,
            _scratch: scratch,
        })
    }

    /// The directory the project was read from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The writable copy the compiler runs in.
    pub fn copy(&self) -> &Path {
        &self.copy
    }

    /// The target directory the Cargo runs share.
    pub fn target_dir(&self) -> &Path {
        &self.target_dir
    }

    /// The revision of the sample project.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// How long loading and extracting the project took, in milliseconds.
    pub fn load_ms(&self) -> u64 {
        self.load_ms
    }

    /// The graph of the project as it is on disk.
    pub fn graph(&self) -> &Arc<RevisionGraph> {
        self.base.graph()
    }

    /// Analyzes one edited variant, with the time it took.
    pub fn analyze(
        &mut self,
        edits: &[FileEdit],
        params: &AnalysisParams,
    ) -> Result<(Variant, u64)> {
        let cancel = CancelToken::new();
        let started = Instant::now();
        let variant = self.base.variant(edits, params, &NoProgress, &cancel)?;
        let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok((variant, millis))
    }

    /// Writes `text` over `file` in the copy, returning the text that was there.
    pub fn write_copy(&self, file: &RelPath, text: &str) -> Result<String> {
        let path = file.to_path(&self.copy);
        let original = std::fs::read_to_string(&path)
            .map_err(|error| format!("`{}` cannot be read: {error}", path.display()))?;
        cargo::write_file(&path, text)?;
        Ok(original)
    }

    /// Puts `text` back into the copy.
    pub fn restore_copy(&self, file: &RelPath, text: &str) -> Result<()> {
        cargo::write_file(&file.to_path(&self.copy), text)
    }
}

/// Time limit for a single Cargo run.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(600);

/// Time limit for one test run of one mutant.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// The abbreviated id of the commit `dir` is at, with `-dirty` when the sample
/// itself has uncommitted changes; `no-git` outside a repository.
pub fn revision_of(dir: &Path) -> String {
    let Ok(root) = blastr_git::discover(dir) else {
        return "no-git".to_owned();
    };
    let Ok(repo) = blastr_git::Repo::open(&root) else {
        return "no-git".to_owned();
    };
    let head = repo.head_commit().ok().flatten();
    let dirty = repo.is_dirty().unwrap_or(false);
    match head {
        Some(head) if dirty => format!("{head}-dirty"),
        Some(head) => head,
        None => "no-commits".to_owned(),
    }
}
