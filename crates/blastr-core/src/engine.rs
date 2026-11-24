//! The entry point of the analysis pipeline.
//!
//! [`Engine`] turns the two sides of a comparison into [`RevisionGraph`]
//! values — from the cache when possible, by loading the project with
//! rust-analyzer otherwise — and hands them to [`pipeline`], which classifies
//! the changes and traces their impact.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use blastr_git::Repo;
use blastr_model::{
    Analysis, AnalysisKey, AnalysisOptions, AnalysisParams, Assume, CancelToken, ChangedFile,
    Comparison, DiffResult, Paths, Progress, RelPath, RevSpec, RevisionGraph, Source, SymbolPath,
};

use blastr_model::protocol::CacheStatus;

use crate::cache::{Cache, Kind};
use crate::codec;
use crate::error::{Error, Result};
use crate::pipeline::{
    Loaded, MANIFEST_FILE, Run, Sides, collect_limits, pipeline, trace, union_of,
};

/// The toolchain recorded in an analysis key when `rustc --version` cannot be run.
const UNKNOWN_TOOLCHAIN: &str = "unknown";

/// The file name that fixes the dependency graph of the project.
const LOCK_FILE: &str = "Cargo.lock";

/// Text hashed into every result key ahead of the two analysis keys, so that a
/// result key can never repeat a digest computed for another purpose.
const RESULT_KEY_DOMAIN: &[u8] = b"analysis-1";

/// What to analyse: a comparison in a repository, under one configuration.
#[derive(Debug, Clone, Copy)]
pub struct AnalysisRequest<'a> {
    /// Any directory inside the repository.
    pub repo_root: &'a Path,
    /// The comparison.
    pub spec: &'a RevSpec,
    /// Configuration to analyse the project under.
    pub options: &'a AnalysisOptions,
    /// The parameters of the impact analysis.
    pub params: &'a AnalysisParams,
}

/// One graph as the engine fetched it.
pub(crate) struct Fetched {
    /// The graph.
    pub(crate) graph: RevisionGraph,
    /// The procedural-macro crates it may be missing, sorted and without
    /// duplicates; empty for a graph from the cache, which only ever holds
    /// complete ones.
    pub(crate) missing_macros: Vec<String>,
}

/// Entry point of the analysis pipeline. Owns the cache.
#[derive(Debug)]
pub struct Engine {
    cache: Cache,
    paths: Paths,
}

impl Engine {
    /// Opens the engine on the per-user cache directory ([`blastr_model::paths`]).
    pub fn open() -> Result<Engine> {
        let paths = blastr_model::paths().ok_or(Error::NoUserDirs)?;
        Ok(Engine::with_paths(paths))
    }

    /// Creates an engine whose cache lives in the given directories.
    pub fn with_paths(paths: Paths) -> Engine {
        Engine {
            cache: Cache::new(&paths.cache_dir),
            paths,
        }
    }

    /// Full impact analysis of a comparison, handing over the classified
    /// changes as soon as they are known and before the impact is traced.
    ///
    /// A comparison whose two graphs were analysed before with the same
    /// parameters is answered from the result cache without a snapshot or a
    /// project load, and `on_changes` is not called. Otherwise progress
    /// reports the phases of loading the graphs, then `classify` and `impact`.
    pub fn analyze(
        &self,
        request: &AnalysisRequest<'_>,
        progress: &dyn Progress,
        cancel: &CancelToken,
        on_changes: &mut dyn FnMut(&DiffResult),
    ) -> Result<Analysis> {
        cancel.check()?;
        let repo = open_repo(request.repo_root)?;
        let comparison = repo.resolve(request.spec)?;
        let toolchain = toolchain_version(repo.root());
        let run = Run {
            repo: &repo,
            options: request.options,
            toolchain: &toolchain,
            progress,
            cancel,
        };

        // Both keys are known without loading anything, so a finished analysis
        // is found before any snapshot or project load starts.
        let base_key = self.key(&run, &comparison.base)?;
        let head_key = self.key(&run, &comparison.head)?;
        let result_key = match (&base_key, &head_key) {
            (Some(base), Some(head)) => Some(result_key(base, head, request.params)?),
            // A side without a content id cannot be named, so its analysis
            // cannot be shared through the cache.
            _ => None,
        };
        if let Some(key) = &result_key
            && let Some(bytes) = self.cache.get(Kind::Result, key)?
            // A damaged entry is not a failure of this run: the analysis is
            // computed again and stored over it.
            && let Ok(analysis) = codec::decode::<Analysis>(&bytes)
        {
            return Ok(analysis);
        }

        let changed_files = repo.changed_files(&comparison)?;
        let sides = self.sides(&run, &comparison, &changed_files, base_key, head_key)?;
        let analysis = pipeline(
            &run,
            &comparison,
            changed_files,
            &sides,
            request.params,
            on_changes,
        )?;
        // An incomplete analysis may not be the answer of a later run, so it is
        // not stored: the next run tries the project again.
        if let Some(key) = &result_key
            && sides.missing_macros.is_empty()
        {
            self.cache
                .put(Kind::Result, key, &codec::encode(&analysis)?)?;
        }
        Ok(analysis)
    }

    /// What-if: the impact of assuming `assume` happened to `symbol` in the
    /// working tree.
    ///
    /// The change is invented, so it is reported as a comparison of the
    /// working tree with itself: there are no changed files, and the assumed
    /// change is the only one. The graph is cached like any other, the
    /// analysis is not.
    #[allow(clippy::too_many_arguments)]
    pub fn what_if(
        &self,
        repo_root: &Path,
        symbol: &SymbolPath,
        assume: Assume,
        options: &AnalysisOptions,
        params: &AnalysisParams,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Analysis> {
        cancel.check()?;
        let repo = open_repo(repo_root)?;
        let toolchain = toolchain_version(repo.root());
        let run = Run {
            repo: &repo,
            options,
            toolchain: &toolchain,
            progress,
            cancel,
        };
        let key = self.key(&run, &Source::Worktree)?;
        let fetched = self.fetch(&run, &Source::Worktree, &key, "head")?;
        let change = crate::impact::whatif::assumed_change(&fetched.graph, symbol, assume)?;
        // The working tree is compared with itself, so none of its lines
        // changed.
        let limits = collect_limits(
            &run,
            &fetched.graph,
            &BTreeMap::new(),
            &fetched.missing_macros,
        )?;
        let diff = DiffResult {
            comparison: Comparison {
                base: Source::Worktree,
                head: Source::Worktree,
            },
            base_key: key.clone(),
            head_key: key,
            changed_files: Vec::new(),
            changes: vec![change],
            limits,
        };
        trace(diff, &fetched.graph, &fetched.graph, params, &run)
    }

    /// The graph of one side of a comparison, from the cache when the same
    /// content was analysed before.
    ///
    /// This is what a front end searches for symbols in; after an analysis of
    /// the same side it only costs reading the cached graph.
    pub fn graph(
        &self,
        repo_root: &Path,
        source: &Source,
        options: &AnalysisOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Arc<RevisionGraph>> {
        cancel.check()?;
        let repo = open_repo(repo_root)?;
        let toolchain = toolchain_version(repo.root());
        let run = Run {
            repo: &repo,
            options,
            toolchain: &toolchain,
            progress,
            cancel,
        };
        let key = self.key(&run, source)?;
        Ok(Arc::new(self.fetch(&run, source, &key, "head")?.graph))
    }

    /// Summarises what the cache holds and where it lives.
    pub fn cache_status(&self) -> Result<CacheStatus> {
        let (graphs, graph_bytes) = self.cache.stats(Kind::Graph)?;
        let (results, result_bytes) = self.cache.stats(Kind::Result)?;
        let snapshots_dir = self.paths.snapshots_dir();
        let (snapshots, snapshot_bytes) = directory_stats(&snapshots_dir)?;
        Ok(CacheStatus {
            graphs,
            graph_bytes,
            results,
            result_bytes,
            snapshots,
            snapshot_bytes,
            cache_dir: self.paths.cache_dir.clone(),
            snapshots_dir,
        })
    }

    /// Removes everything the cache holds and reports what was there.
    pub fn cache_clear(&self) -> Result<CacheStatus> {
        let status = self.cache_status()?;
        self.cache.clear()?;
        remove_dir_if_present(&self.paths.snapshots_dir())?;
        remove_dir_if_present(&self.paths.target_dir())?;
        Ok(status)
    }

    /// The two graphs of a comparison.
    ///
    /// A side found in the cache is read from it. When neither side is, and
    /// both are the same Cargo project, the project is loaded once from the
    /// base side and the head side is applied on top of it; otherwise each
    /// missing side is loaded on its own.
    pub(crate) fn sides(
        &self,
        run: &Run<'_>,
        comparison: &Comparison,
        changed_files: &[ChangedFile],
        base_key: Option<AnalysisKey>,
        head_key: Option<AnalysisKey>,
    ) -> Result<Sides> {
        let base = self.cached(&base_key)?;
        let head = self.cached(&head_key)?;
        if base.is_none() && head.is_none() && self.same_project(run, comparison)? {
            // One project load, two extractions; the project is dropped before
            // the graphs are stored so that it does not stay in memory.
            let (base, head, missing_macros) = {
                let mut loaded = self.load(run, &comparison.base, "base")?;
                let base = loaded.extract(run, "base")?;
                let head = loaded.overlay(run, &comparison.head, changed_files)?;
                (base, head, loaded.missing_macros)
            };
            // One project made both graphs, so what its macros could not
            // expand is missing from both.
            if missing_macros.is_empty() {
                self.store(&base_key, &base)?;
                self.store(&head_key, &head)?;
            }
            return Ok(Sides {
                base: Arc::new(base),
                base_key,
                head: Arc::new(head),
                head_key,
                missing_macros,
            });
        }
        let base = match base {
            Some(graph) => Fetched {
                graph,
                missing_macros: Vec::new(),
            },
            None => self.fetch(run, &comparison.base, &base_key, "base")?,
        };
        let head = match head {
            Some(graph) => Fetched {
                graph,
                missing_macros: Vec::new(),
            },
            None => self.fetch(run, &comparison.head, &head_key, "head")?,
        };
        Ok(Sides {
            missing_macros: union_of(&base.missing_macros, &head.missing_macros),
            base: Arc::new(base.graph),
            base_key,
            head: Arc::new(head.graph),
            head_key,
        })
    }

    /// The graph stored under `key`; `None` without a key, or when nothing
    /// usable is stored.
    fn cached(&self, key: &Option<AnalysisKey>) -> Result<Option<RevisionGraph>> {
        let Some(key) = key else {
            return Ok(None);
        };
        Ok(self
            .cache
            .get(Kind::Graph, &key.hash())?
            .and_then(|bytes| codec::decode::<RevisionGraph>(&bytes).ok()))
    }

    /// The graph of one side: from the cache under `key`, or loaded and
    /// extracted.
    ///
    /// A graph made from a project whose macros did not all load is not
    /// stored: an incomplete graph may not be the answer of a later run, which
    /// has a chance of building the macros.
    pub(crate) fn fetch(
        &self,
        run: &Run<'_>,
        source: &Source,
        key: &Option<AnalysisKey>,
        phase: &str,
    ) -> Result<Fetched> {
        if let Some(graph) = self.cached(key)? {
            return Ok(Fetched {
                graph,
                missing_macros: Vec::new(),
            });
        }
        // The project lives in this block so that it is dropped before the
        // next side is loaded: only one project is in memory at a time.
        let (graph, missing_macros) = {
            let loaded = self.load(run, source, phase)?;
            (loaded.extract(run, phase)?, loaded.missing_macros)
        };
        if missing_macros.is_empty() {
            self.store(key, &graph)?;
        }
        Ok(Fetched {
            graph,
            missing_macros,
        })
    }

    /// Loads the project `source` holds into rust-analyzer.
    pub(crate) fn load(&self, run: &Run<'_>, source: &Source, phase: &str) -> Result<Loaded> {
        run.cancel.check()?;
        run.progress.report("snapshot", 0, None);
        let root = run.repo.materialize(source, &self.paths.snapshots_dir())?;
        let manifest_dir = match &run.options.manifest_dir {
            Some(dir) => dir.to_path(&root),
            None => root.clone(),
        };
        run.cancel.check()?;
        run.progress.report(&format!("load {phase}"), 0, None);
        let options = blastr_ra::LoadOptions {
            features: run.options.features.clone(),
            all_features: run.options.all_features,
            target: run.options.target.clone(),
            exec: run.options.exec,
            target_dir: Some(self.paths.target_dir()),
        };
        let workspace =
            blastr_ra::Workspace::load(&root, &manifest_dir, &options, run.progress, run.cancel)?;
        run.cancel.check()?;
        Ok(Loaded::new(workspace))
    }

    /// True when both sides are the same Cargo project, so that the project
    /// loaded from the base side describes both: `Cargo.toml` and `Cargo.lock`
    /// in the manifest directory are byte-for-byte equal, or absent from both.
    fn same_project(&self, run: &Run<'_>, comparison: &Comparison) -> Result<bool> {
        for name in [MANIFEST_FILE, LOCK_FILE] {
            let path = project_file(run.options, name);
            if run.repo.read_file(&comparison.base, &path)?
                != run.repo.read_file(&comparison.head, &path)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Writes one graph into the cache under `key`; nothing happens without a key.
    fn store(&self, key: &Option<AnalysisKey>, graph: &RevisionGraph) -> Result<()> {
        if let Some(key) = key {
            self.cache
                .put(Kind::Graph, &key.hash(), &codec::encode(graph)?)?;
        }
        Ok(())
    }

    /// The analysis key of one side, computed without loading the project;
    /// `None` when the source has no content id.
    pub(crate) fn key(&self, run: &Run<'_>, source: &Source) -> Result<Option<AnalysisKey>> {
        let Some(content_id) = run.repo.content_id(source)? else {
            return Ok(None);
        };
        let options = run.options;
        let mut features = options.features.clone();
        features.sort();
        features.dedup();
        let cargo_lock = match run
            .repo
            .read_file(source, &project_file(options, LOCK_FILE))?
        {
            Some(bytes) => blake3::hash(&bytes).to_hex().to_string(),
            None => String::new(),
        };
        Ok(Some(AnalysisKey {
            repository: run.repo.repository_id(),
            source: content_id,
            features,
            all_features: options.all_features,
            target: options.target.clone(),
            cfg_test: true,
            cargo_lock,
            toolchain: run.toolchain.to_owned(),
            rust_analyzer: blastr_ra::RUST_ANALYZER_VERSION.to_owned(),
            blastr: build_id().to_owned(),
            exec: options.exec,
            manifest_dir: options.manifest_dir.clone(),
        }))
    }
}

/// Opens the repository that holds `dir`.
pub(crate) fn open_repo(dir: &Path) -> Result<Repo> {
    let root = blastr_git::discover(dir)?;
    Ok(Repo::open(&root)?)
}

/// The version of this build followed by a fingerprint of the executable.
///
/// Cached graphs and results are keyed by it. The version alone would let a
/// rebuilt executable of the same version read what older logic computed.
/// The window also reads it from the engine's handshake, so that two programs
/// from different builds are refused instead of misunderstood.
pub fn build_id() -> &'static str {
    static BUILD_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BUILD_ID.get_or_init(|| {
        let version = env!("CARGO_PKG_VERSION");
        let fingerprint = std::env::current_exe()
            .and_then(std::fs::metadata)
            .ok()
            .map(|metadata| {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |since| since.as_nanos());
                let mut hasher = blake3::Hasher::new();
                hasher.update(&metadata.len().to_le_bytes());
                hasher.update(&modified.to_le_bytes());
                hasher.finalize().to_hex()[..8].to_owned()
            });
        match fingerprint {
            Some(fingerprint) => format!("{version}+{fingerprint}"),
            None => version.to_owned(),
        }
    })
}

/// The cache key of an analysis: BLAKE3, as lowercase hexadecimal, of a fixed
/// prefix, the two hex analysis keys and the parameters, in that order.
///
/// Equal keys mean the same two graphs were analysed under the same
/// parameters, so the stored result describes exactly the same thing.
fn result_key(base: &AnalysisKey, head: &AnalysisKey, params: &AnalysisParams) -> Result<String> {
    let parameters = serde_json::to_vec(params).map_err(|error| {
        Error::Codec(format!(
            "could not serialise the analysis parameters: {error}"
        ))
    })?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(RESULT_KEY_DOMAIN);
    hasher.update(base.hash().as_bytes());
    hasher.update(head.hash().as_bytes());
    hasher.update(&parameters);
    Ok(hasher.finalize().to_hex().to_string())
}

/// The output of `rustc --version` run in `dir`, or `unknown` when it cannot
/// be run.
///
/// Reading the version costs one process per engine call, which is why it is
/// done once and passed down rather than per side.
pub(crate) fn toolchain_version(dir: &Path) -> String {
    Command::new("rustc")
        .arg("--version")
        .current_dir(dir)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| UNKNOWN_TOOLCHAIN.to_owned())
}

/// `<manifest dir>/<name>`, relative to the root the paths are relative to.
fn project_file(options: &AnalysisOptions, name: &str) -> RelPath {
    match &options.manifest_dir {
        Some(dir) if !dir.as_str().is_empty() => RelPath::new(format!("{}/{name}", dir.as_str())),
        _ => RelPath::new(name),
    }
}

/// Number and total size of the subdirectories of `dir`.
///
/// A directory that does not exist counts as empty.
fn directory_stats(dir: &Path) -> Result<(u64, u64)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => return Err(Error::Io(error)),
    };
    let mut directories = 0;
    let mut bytes = 0;
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            directories += 1;
            bytes += directory_size(&entry.path())?;
        }
    }
    Ok((directories, bytes))
}

/// Total size of the files below `dir`, walked recursively.
fn directory_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(Error::Io(error)),
        };
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata()?.len();
            }
        }
    }
    Ok(total)
}

/// Removes a directory tree; a directory that does not exist is not an error.
///
/// A cache holds what other tools left there: `CARGO_TARGET_DIR` is written by
/// Cargo, and a checkout can mark files read-only. Windows refuses to delete a
/// read-only file, and `remove_dir_all` then fails with `Access is denied` for
/// the whole tree, so the attribute is cleared entry by entry before the
/// removal is tried again. When an entry still cannot be removed, the error
/// names it, because "Access is denied" alone does not say what held the file.
fn remove_dir_if_present(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => match remove_dir_forcibly(dir) {
            Ok(()) => Ok(()),
            Err((_path, error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err((path, error)) => Err(Error::RemoveDir {
                path,
                message: error.to_string(),
            }),
        },
    }
}

/// Removes a directory tree, clearing read-only attributes on the way.
///
/// The tree is walked first and deleted afterwards, deepest directory last, so
/// that a directory is only removed once it is empty. `Err` holds the path that
/// failed together with the error the system reported for it.
fn remove_dir_forcibly(dir: &Path) -> std::result::Result<(), (PathBuf, std::io::Error)> {
    let mut directories = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory).map_err(|error| (directory.clone(), error))?;
        directories.push(directory.clone());
        for entry in entries {
            let entry = entry.map_err(|error| (directory.clone(), error))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| (path.clone(), error))?;
            if file_type.is_dir() {
                pending.push(path);
            } else {
                remove_file_forcibly(&path)?;
            }
        }
    }
    for directory in directories.iter().rev() {
        std::fs::remove_dir(directory).map_err(|error| (directory.clone(), error))?;
    }
    Ok(())
}

/// Removes one file, clearing a read-only attribute that would stop it.
fn remove_file_forcibly(path: &Path) -> std::result::Result<(), (PathBuf, std::io::Error)> {
    let failed = |error| (path.to_path_buf(), error);
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(_) => {
            clear_read_only(path).map_err(failed)?;
            std::fs::remove_file(path).map_err(failed)
        }
    }
}

/// Clears the read-only attribute of `path`, if it has one.
///
/// This is the one place `set_readonly(false)` is meant: the lint that warns
/// about it guards against clearing the *write* permission a Unix mode needs,
/// which is not what the call is for here. Windows has no other standard way to
/// clear the file attribute that stops a deletion.
#[allow(clippy::permissions_set_readonly_false)]
fn clear_read_only(path: &Path) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    if permissions.readonly() {
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use blastr_model::{ExecMode, ImpactLimits, Limit, TestSelection};

    use super::*;

    fn paths_under(dir: &Path) -> Paths {
        Paths {
            cache_dir: dir.to_path_buf(),
            config_dir: dir.join("config"),
        }
    }

    #[test]
    fn fresh_cache_reports_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::with_paths(paths_under(dir.path()));

        let status = engine.cache_status().unwrap();
        assert_eq!(status.graphs, 0);
        assert_eq!(status.graph_bytes, 0);
        assert_eq!(status.results, 0);
        assert_eq!(status.result_bytes, 0);
        assert_eq!(status.snapshots, 0);
        assert_eq!(status.snapshot_bytes, 0);
        assert_eq!(status.cache_dir, dir.path());
        assert_eq!(status.snapshots_dir, dir.path().join("snapshots"));
    }

    #[test]
    fn clear_reports_and_removes_graphs_and_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        let cache = Cache::new(&paths.cache_dir);
        cache.put(Kind::Graph, "key", b"graph").unwrap();
        cache.put(Kind::Result, "result", b"analysis").unwrap();

        let snapshot = paths.snapshots_dir().join("4f2a91c");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("lib.rs"), b"fn main() {}").unwrap();
        let build = paths.target_dir().join("debug");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("out"), b"x").unwrap();

        let engine = Engine::with_paths(paths.clone());
        let status = engine.cache_clear().unwrap();

        assert_eq!(status.graphs, 1);
        assert_eq!(status.graph_bytes, 5);
        assert_eq!(status.results, 1);
        assert_eq!(status.result_bytes, 8);
        assert_eq!(status.snapshots, 1);
        assert_eq!(status.snapshot_bytes, 12);
        assert!(!paths.snapshots_dir().exists());
        assert!(!paths.target_dir().exists());
        assert_eq!(engine.cache_status().unwrap().graphs, 0);
        assert_eq!(engine.cache_status().unwrap().results, 0);
        assert_eq!(engine.cache_status().unwrap().snapshots, 0);
    }

    #[test]
    fn clear_removes_a_tree_of_read_only_files() {
        // Cargo and a Git checkout both leave read-only entries behind, and
        // Windows refuses to delete those until the attribute is cleared.
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_under(dir.path());
        let snapshot = paths.snapshots_dir().join("4f2a91c").join(".git");
        std::fs::create_dir_all(&snapshot).unwrap();
        let pack = snapshot.join("pack.idx");
        std::fs::write(&pack, b"pack").unwrap();
        let mut permissions = std::fs::metadata(&pack).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&pack, permissions).unwrap();

        let engine = Engine::with_paths(paths.clone());
        engine.cache_clear().unwrap();

        assert!(!paths.snapshots_dir().exists());
    }

    #[test]
    fn a_failed_removal_names_the_file() {
        let error = Error::RemoveDir {
            path: PathBuf::from("C:/cache/target/debug/locked.rlib"),
            message: "Access is denied. (os error 5)".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "cannot remove `C:/cache/target/debug/locked.rlib`: Access is denied. (os error 5)"
        );
        assert!(error.hint().is_some());
    }

    /// An analysis key with the fields fixed so that only `source` varies.
    fn sample_key(source: &str) -> AnalysisKey {
        AnalysisKey {
            repository: "repo".to_owned(),
            source: source.to_owned(),
            features: vec!["default".to_owned()],
            all_features: false,
            target: None,
            cfg_test: true,
            cargo_lock: "lock".to_owned(),
            toolchain: "rustc 1.100.0".to_owned(),
            rust_analyzer: "0.0.358".to_owned(),
            blastr: "0.1.0".to_owned(),
            exec: ExecMode::Full,
            manifest_dir: None,
        }
    }

    /// An analysis with only the fields the codec could get wrong filled in.
    fn sample_analysis() -> Analysis {
        Analysis {
            diff: DiffResult {
                comparison: Comparison {
                    base: Source::Worktree,
                    head: Source::Worktree,
                },
                base_key: Some(sample_key("base")),
                head_key: Some(sample_key("head")),
                changed_files: Vec::new(),
                changes: Vec::new(),
                limits: vec![Limit::Restricted],
            },
            items: Vec::new(),
            tests: TestSelection::default(),
            limits: ImpactLimits {
                unresolved_total: 3,
                candidate_steps: 1,
                ..ImpactLimits::default()
            },
            params: AnalysisParams::default(),
        }
    }

    #[test]
    fn the_result_key_is_stable_and_follows_its_inputs() {
        let base = sample_key("base");
        let head = sample_key("head");
        let params = AnalysisParams::default();

        let key = result_key(&base, &head, &params).unwrap();
        assert_eq!(key, result_key(&base, &head, &params).unwrap());
        assert_eq!(key.len(), 64);
        assert!(
            key.chars()
                .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
        );
        // The two sides are not interchangeable.
        assert_ne!(key, result_key(&head, &base, &params).unwrap());
    }

    #[test]
    fn the_result_key_changes_with_the_parameters() {
        let base = sample_key("base");
        let head = sample_key("head");
        let params = AnalysisParams::default();
        let key = result_key(&base, &head, &params).unwrap();

        let mut deeper = params.clone();
        deeper.depth += 1;
        assert_ne!(key, result_key(&base, &head, &deeper).unwrap());

        let mut stricter = params.clone();
        stricter.min_confidence = blastr_model::Confidence::Definite;
        assert_ne!(key, result_key(&base, &head, &stricter).unwrap());

        let mut ignored = params.clone();
        ignored.ignore.push("vendor/**".to_owned());
        assert_ne!(key, result_key(&base, &head, &ignored).unwrap());
    }

    #[test]
    fn an_analysis_round_trips_through_the_codec() {
        let analysis = sample_analysis();
        let bytes = codec::encode(&analysis).unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(codec::decode::<Analysis>(&bytes).ok(), Some(analysis));
    }

    #[test]
    fn a_damaged_analysis_blob_is_a_miss() {
        assert!(codec::decode::<Analysis>(b"not compressed at all").is_err());

        let mut truncated = codec::encode(&sample_analysis()).unwrap();
        truncated.truncate(truncated.len() / 2);
        assert!(codec::decode::<Analysis>(&truncated).is_err());
    }

    #[test]
    fn cache_status_counts_stored_analyses() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path());
        cache.put(Kind::Result, "first", b"short").unwrap();
        cache
            .put(Kind::Result, "second", b"a longer payload")
            .unwrap();
        let engine = Engine::with_paths(paths_under(dir.path()));

        let status = engine.cache_status().unwrap();
        assert_eq!(status.results, 2);
        assert_eq!(status.result_bytes, 5 + 16);
        assert_eq!(status.graphs, 0);
        assert_eq!(status.graph_bytes, 0);
    }
}
