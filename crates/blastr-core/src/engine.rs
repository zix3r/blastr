//! The entry point of the analysis pipeline.
//!
//! [`Engine`] turns the two sides of a comparison into [`RevisionGraph`]
//! values (from the cache when possible, by loading the project with
//! rust-analyzer otherwise) and hands them to the structural comparison in
//! [`crate::diff`].

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use blastr_git::Repo;
use blastr_model::{
    AnalysisKey, AnalysisOptions, CancelToken, Change, ChangedFile, Comparison, DiffResult,
    ExecMode, FileStatus, Limit, Progress, RelPath, RevSpec, RevisionGraph, Source,
};

use crate::codec::{decode_graph, encode_graph};
use crate::error::{Error, Result};
use crate::hunks::{changed_lines, overlaps};

/// The toolchain recorded in an analysis key when `rustc --version` cannot be run.
const UNKNOWN_TOOLCHAIN: &str = "unknown";

/// The file name whose changes are reported as manifest changes.
const MANIFEST_FILE: &str = "Cargo.toml";

/// The file name that fixes the dependency graph of the project.
const LOCK_FILE: &str = "Cargo.lock";

/// Everything one call to [`Engine::graph_for`] needs beyond the source.
struct GraphRequest<'a> {
    /// Configuration to analyse the project under.
    options: &'a AnalysisOptions,
    /// Which side this is, used in the progress phase names: `base` or `head`.
    phase: &'a str,
    /// Toolchain version to record in the analysis key; `None` when no key is built.
    toolchain: Option<&'a str>,
    /// Where progress is reported.
    progress: &'a dyn Progress,
    /// Cancellation flag.
    cancel: &'a CancelToken,
}

/// Both graphs of one comparison, and the cache keys they belong to.
struct Sides {
    /// The graph of the base side.
    base: Arc<RevisionGraph>,
    /// The cache key of the base graph; `None` when the source has no content id.
    base_key: Option<AnalysisKey>,
    /// The graph of the head side.
    head: Arc<RevisionGraph>,
    /// The cache key of the head graph; `None` when the source has no content id.
    head_key: Option<AnalysisKey>,
}

/// Everything one attempt at building both graphs from a single project load
/// needs.
struct OverlayRequest<'a> {
    /// The repository the sources belong to; `None` for plain directories.
    repo: Option<&'a Repo>,
    /// The two sides being compared.
    comparison: &'a Comparison,
    /// The files that differ between them.
    changed_files: &'a [ChangedFile],
    /// Configuration to analyse the project under.
    options: &'a AnalysisOptions,
    /// Toolchain version to record in the analysis keys.
    toolchain: Option<&'a str>,
    /// Where progress is reported.
    progress: &'a dyn Progress,
    /// Cancellation flag.
    cancel: &'a CancelToken,
}

/// Entry point of the analysis pipeline. Owns the cache.
#[derive(Debug)]
pub struct Engine {
    store: blastr_store::Store,
    paths: blastr_store::Paths,
}

impl Engine {
    /// Opens the engine with the per-user cache database
    /// (`blastr_store::paths()?.database()`).
    pub fn open() -> Result<Engine> {
        let paths = blastr_store::paths()?;
        let store = blastr_store::Store::open(&paths.database())?;
        Ok(Engine::with_store_and_paths(store, paths))
    }

    /// Creates an engine on an already opened store (used by tests).
    ///
    /// The cache directories are taken from the system temporary directory,
    /// so nothing outside it is touched.
    pub fn with_store(store: blastr_store::Store) -> Engine {
        Engine::with_store_and_paths(store, temporary_paths())
    }

    /// Creates an engine on an already opened store and explicit directories.
    pub fn with_store_and_paths(store: blastr_store::Store, paths: blastr_store::Paths) -> Engine {
        Engine { store, paths }
    }

    /// The cache this engine uses.
    pub fn store(&self) -> &blastr_store::Store {
        &self.store
    }

    /// Compares two sides of the repository at `repo_root`.
    pub fn diff(
        &self,
        repo_root: &Path,
        spec: &RevSpec,
        options: &AnalysisOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<DiffResult> {
        cancel.check()?;
        let root = blastr_git::discover(repo_root)?;
        let repo = Repo::open(&root)?;
        let comparison = repo.resolve(spec)?;
        let changed_files = repo.changed_files(&comparison)?;
        let toolchain = toolchain_version(repo.root());

        let sides = match self.overlay_graphs(OverlayRequest {
            repo: Some(&repo),
            comparison: &comparison,
            changed_files: &changed_files,
            options,
            toolchain: Some(toolchain.as_str()),
            progress,
            cancel,
        })? {
            Some(sides) => sides,
            None => {
                // The two sides are different projects, or one of them is
                // cached, so each side is loaded on its own.
                let (base, base_key, _) = self.graph_for(
                    Some(&repo),
                    &comparison.base,
                    GraphRequest {
                        options,
                        phase: "base",
                        toolchain: Some(toolchain.as_str()),
                        progress,
                        cancel,
                    },
                )?;
                cancel.check()?;
                let (head, head_key, _) = self.graph_for(
                    Some(&repo),
                    &comparison.head,
                    GraphRequest {
                        options,
                        phase: "head",
                        toolchain: Some(toolchain.as_str()),
                        progress,
                        cancel,
                    },
                )?;
                cancel.check()?;
                Sides {
                    base,
                    base_key,
                    head,
                    head_key,
                }
            }
        };

        progress.report("classify", 0, None);
        let mut changes = crate::diff::classify(
            &sides.base,
            &sides.head,
            &changed_files,
            &crate::diff::MatchParams::default(),
        );
        changes.extend(manifest_changes_of_repo(
            &repo,
            &comparison,
            &changed_files,
        )?);
        let changed_lines = self.changed_lines_of(Some(&repo), &comparison, &changed_files)?;
        let limits = collect_limits(
            Some(&repo),
            &sides.head,
            &changed_files,
            &changed_lines,
            options,
        )?;

        Ok(DiffResult {
            comparison,
            base_key: sides.base_key,
            head_key: sides.head_key,
            changed_files,
            changes,
            limits,
        })
    }

    /// Compares two plain directories. Nothing is cached.
    pub fn diff_dirs(
        &self,
        base: &Path,
        head: &Path,
        options: &AnalysisOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<DiffOutput> {
        cancel.check()?;
        let changed_files = blastr_git::changed_files_between_dirs(base, head)?;
        let comparison = Comparison {
            base: Source::Dir(base.to_path_buf()),
            head: Source::Dir(head.to_path_buf()),
        };

        let (base_graph, _, _) = self.graph_for(
            None,
            &comparison.base,
            GraphRequest {
                options,
                phase: "base",
                toolchain: None,
                progress,
                cancel,
            },
        )?;
        cancel.check()?;
        let (head_graph, _, _) = self.graph_for(
            None,
            &comparison.head,
            GraphRequest {
                options,
                phase: "head",
                toolchain: None,
                progress,
                cancel,
            },
        )?;
        cancel.check()?;

        progress.report("classify", 0, None);
        let mut changes = crate::diff::classify(
            &base_graph,
            &head_graph,
            &changed_files,
            &crate::diff::MatchParams::default(),
        );
        changes.extend(manifest_changes_of_dirs(base, head, &changed_files)?);
        let changed_lines = self.changed_lines_of(None, &comparison, &changed_files)?;
        let limits = collect_limits(None, &head_graph, &changed_files, &changed_lines, options)?;

        Ok(DiffOutput {
            result: DiffResult {
                comparison,
                base_key: None,
                head_key: None,
                changed_files,
                changes,
                limits,
            },
            base: base_graph,
            head: head_graph,
        })
    }

    /// Builds (or fetches from the cache) the graph of one revision;
    /// `rev` `None` means `HEAD`.
    pub fn index(
        &self,
        repo_root: &Path,
        rev: Option<&str>,
        options: &AnalysisOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<IndexReport> {
        cancel.check()?;
        let root = blastr_git::discover(repo_root)?;
        let repo = Repo::open(&root)?;
        let spec = RevSpec {
            rev: Some(rev.unwrap_or("HEAD").to_owned()),
            ..RevSpec::default()
        };
        // A root commit has nothing to compare against, and there is no other
        // way to reach its graph, so the error is passed on as it is.
        let comparison = repo.resolve(&spec)?;
        let toolchain = toolchain_version(repo.root());

        let (graph, key, from_cache) = self.graph_for(
            Some(&repo),
            &comparison.head,
            GraphRequest {
                options,
                phase: "head",
                toolchain: Some(toolchain.as_str()),
                progress,
                cancel,
            },
        )?;
        let Some(key) = key else {
            // Every source `Repo::resolve` produces has a content id, so the
            // graph of a revision always has a key.
            return Err(Error::Ra(blastr_ra::Error::Internal(
                "the indexed revision has no content identifier".to_owned(),
            )));
        };

        Ok(IndexReport {
            key,
            from_cache,
            symbols: graph.symbols.len(),
            edges: graph.edges.len(),
            unresolved: graph.unresolved.len(),
        })
    }

    /// Summarises what the cache holds and where it lives.
    pub fn cache_status(&self) -> Result<CacheStatus> {
        let graphs = self.store.list_graphs()?.len() as u64;
        let graph_bytes = self.store.total_size()?;
        let snapshots_dir = self.paths.snapshots_dir();
        let (snapshots, snapshot_bytes) = directory_stats(&snapshots_dir)?;
        Ok(CacheStatus {
            graphs,
            graph_bytes,
            snapshots,
            snapshot_bytes,
            database: self.paths.database(),
            snapshots_dir,
        })
    }

    /// Removes everything the cache holds and reports what was there.
    pub fn cache_clear(&self) -> Result<CacheStatus> {
        let status = self.cache_status()?;
        self.store.clear()?;
        remove_dir_if_present(&self.paths.snapshots_dir())?;
        remove_dir_if_present(&self.paths.target_dir())?;
        Ok(status)
    }

    /// Loads the graph of one side of a comparison, from the cache when the
    /// same content was analysed before.
    ///
    /// Returns the graph, the analysis key it was stored under (`None` when
    /// the source has no content id, which is the case for a plain directory
    /// and for a working tree without a repository) and whether it came from
    /// the cache.
    ///
    /// `toolchain` is the toolchain version to record in the key; it is only
    /// read when a key is built.
    fn graph_for(
        &self,
        repo: Option<&Repo>,
        source: &Source,
        request: GraphRequest<'_>,
    ) -> Result<(Arc<RevisionGraph>, Option<AnalysisKey>, bool)> {
        let GraphRequest {
            options,
            phase,
            toolchain,
            progress,
            cancel,
        } = request;
        cancel.check()?;
        let key = self.key_for(repo, source, options, toolchain)?;

        if let Some(key) = &key
            && let Some(bytes) = self.store.get_graph(&key.hash())?
            && let Ok(graph) = decode_graph(&bytes)
        {
            return Ok((Arc::new(graph), Some(key.clone()), true));
        }

        progress.report("snapshot", 0, None);
        cancel.check()?;
        let root_dir = self.materialize(repo, source)?;
        let manifest_dir = manifest_dir_in(options, &root_dir);
        cancel.check()?;

        let load_options = self.load_options(options);
        progress.report(&format!("load {phase}"), 0, None);
        // The workspace lives in this block so that it is dropped before the
        // next side is loaded: only one project is in memory at a time.
        let graph = {
            let workspace = blastr_ra::Workspace::load(
                &root_dir,
                &manifest_dir,
                &load_options,
                progress,
                cancel,
            )?;
            cancel.check()?;
            progress.report(&format!("extract {phase}"), 0, None);
            workspace.extract(progress, cancel)?
        };

        self.store_graph(&key, &graph)?;
        Ok((Arc::new(graph), key, false))
    }

    /// The two graphs of one comparison, with the head reached by applying the
    /// changed files to the project loaded from the base side.
    ///
    /// rust-analyzer can only be reused when both sides are the same project:
    /// the same `Cargo.toml` and `Cargo.lock` mean the same crates, the same
    /// targets and the same dependency graph, so only the text of the files
    /// differs. Returns `Ok(None)` when that is not the case, or when one of the
    /// graphs is already cached (fetching it is cheaper than any load); the
    /// caller then loads both sides one after the other.
    ///
    /// Progress phases are `snapshot`, `load base`, `extract base` and
    /// `extract head`; there is no `load head`.
    fn overlay_graphs(&self, request: OverlayRequest<'_>) -> Result<Option<Sides>> {
        let OverlayRequest {
            repo,
            comparison,
            changed_files,
            options,
            toolchain,
            progress,
            cancel,
        } = request;
        cancel.check()?;
        if !self.same_project_structure(repo, comparison, options)? {
            return Ok(None);
        }

        // The keys are known without loading, so a cached side is recognised
        // before the load starts.
        let base_key = self.key_for(repo, &comparison.base, options, toolchain)?;
        let head_key = self.key_for(repo, &comparison.head, options, toolchain)?;
        let cached = self.store.list_graphs()?;
        let in_cache = |key: &Option<AnalysisKey>| match key {
            Some(key) => {
                let hash = key.hash();
                cached.iter().any(|entry| entry.key_hash == hash)
            }
            None => false,
        };
        if in_cache(&base_key) || in_cache(&head_key) {
            return Ok(None);
        }

        progress.report("snapshot", 0, None);
        let root_dir = self.materialize(repo, &comparison.base)?;
        let manifest_dir = manifest_dir_in(options, &root_dir);
        let load_options = self.load_options(options);
        cancel.check()?;

        // One project load, two extractions; the workspace is dropped before
        // the graphs are stored so that it does not stay in memory.
        let (base, head) = {
            progress.report("load base", 0, None);
            let mut workspace = blastr_ra::Workspace::load(
                &root_dir,
                &manifest_dir,
                &load_options,
                progress,
                cancel,
            )?;
            cancel.check()?;
            progress.report("extract base", 0, None);
            let base = workspace.extract(progress, cancel)?;
            cancel.check()?;

            let edits = self.file_edits(repo, &comparison.head, changed_files)?;
            workspace.apply(&edits)?;
            cancel.check()?;
            progress.report("extract head", 0, None);
            let head = workspace.extract(progress, cancel)?;
            cancel.check()?;
            (base, head)
        };

        self.store_graph(&base_key, &base)?;
        self.store_graph(&head_key, &head)?;
        Ok(Some(Sides {
            base: Arc::new(base),
            base_key,
            head: Arc::new(head),
            head_key,
        }))
    }

    /// True when both sides are the same Cargo project, so that the project
    /// loaded from the base side describes both.
    ///
    /// `Cargo.toml` and `Cargo.lock` in the manifest directory must be
    /// byte-for-byte equal; a `Cargo.lock` that neither side has counts as equal.
    fn same_project_structure(
        &self,
        repo: Option<&Repo>,
        comparison: &Comparison,
        options: &AnalysisOptions,
    ) -> Result<bool> {
        for name in [MANIFEST_FILE, LOCK_FILE] {
            let path = project_file(options, name);
            let base = self.read_source_file(repo, &comparison.base, &path)?;
            let head = self.read_source_file(repo, &comparison.head, &path)?;
            if base != head {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The edits that turn the base side into the head side.
    fn file_edits(
        &self,
        repo: Option<&Repo>,
        head: &Source,
        changed_files: &[ChangedFile],
    ) -> Result<Vec<blastr_ra::FileEdit>> {
        let mut edits = Vec::new();
        for file in changed_files {
            // A rename is the removal of the old path and the addition of the new one.
            if let Some(old_path) = &file.old_path {
                edits.push(blastr_ra::FileEdit {
                    path: old_path.clone(),
                    content: None,
                });
            }
            if file.status == FileStatus::Deleted {
                edits.push(blastr_ra::FileEdit {
                    path: file.path.clone(),
                    content: None,
                });
                continue;
            }
            let Some(bytes) = self.read_source_file(repo, head, &file.path)? else {
                continue;
            };
            // Text that is not UTF-8 cannot be a Rust source file and
            // rust-analyzer would drop it as well, so the base text is left alone.
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            edits.push(blastr_ra::FileEdit {
                path: file.path.clone(),
                content: Some(text),
            });
        }
        Ok(edits)
    }

    /// The graph of both sides of a directory comparison, the head reached by
    /// applying the changed files to the base side instead of loading it again.
    ///
    /// This is the code path [`Engine::diff`] takes when both sides are the same
    /// project, exposed so that the tests can compare it with the two full loads
    /// of [`Engine::diff_dirs`]. `Ok(None)` when the directories do not share
    /// their `Cargo.toml` and `Cargo.lock`.
    pub fn overlay_dirs(
        &self,
        base: &Path,
        head: &Path,
        options: &AnalysisOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Option<(Arc<RevisionGraph>, Arc<RevisionGraph>)>> {
        cancel.check()?;
        let changed_files = blastr_git::changed_files_between_dirs(base, head)?;
        let comparison = Comparison {
            base: Source::Dir(base.to_path_buf()),
            head: Source::Dir(head.to_path_buf()),
        };
        let sides = self.overlay_graphs(OverlayRequest {
            repo: None,
            comparison: &comparison,
            changed_files: &changed_files,
            options,
            toolchain: None,
            progress,
            cancel,
        })?;
        Ok(sides.map(|sides| (sides.base, sides.head)))
    }

    /// The analysis key of one side, or `None` when the source has no content id.
    fn key_for(
        &self,
        repo: Option<&Repo>,
        source: &Source,
        options: &AnalysisOptions,
        toolchain: Option<&str>,
    ) -> Result<Option<AnalysisKey>> {
        let Some(repo) = repo else {
            return Ok(None);
        };
        let Some(content_id) = repo.content_id(source)? else {
            return Ok(None);
        };
        Ok(Some(self.analysis_key(
            repo,
            source,
            &content_id,
            options,
            toolchain,
        )?))
    }

    /// A directory holding the content of `source`, exactly as it is analysed.
    fn materialize(&self, repo: Option<&Repo>, source: &Source) -> Result<PathBuf> {
        match repo {
            Some(repo) => Ok(repo.materialize(source, &self.paths.snapshots_dir())?),
            None => match source {
                Source::Dir(path) => Ok(path.clone()),
                // Without a repository only a plain directory can be read; the
                // engine never asks for anything else in that case.
                other => Err(Error::Ra(blastr_ra::Error::Internal(format!(
                    "cannot materialise {other:?} without a repository"
                )))),
            },
        }
    }

    /// How rust-analyzer is told to load a project.
    fn load_options(&self, options: &AnalysisOptions) -> blastr_ra::LoadOptions {
        blastr_ra::LoadOptions {
            features: options.features.clone(),
            all_features: options.all_features,
            target: options.target.clone(),
            exec: options.exec,
            target_dir: Some(self.paths.target_dir()),
        }
    }

    /// Writes one graph into the cache under `key`; nothing happens without a key.
    fn store_graph(&self, key: &Option<AnalysisKey>, graph: &RevisionGraph) -> Result<()> {
        let Some(key) = key else {
            return Ok(());
        };
        let bytes = encode_graph(graph)?;
        let key_json = serde_json::to_string(key).map_err(|error| {
            Error::Codec(format!("could not serialise the analysis key: {error}"))
        })?;
        self.store.put_graph(&key.hash(), &key_json, &bytes)?;
        Ok(())
    }

    /// The bytes of `path` on `source`, or `None` when the file does not exist there.
    fn read_source_file(
        &self,
        repo: Option<&Repo>,
        source: &Source,
        path: &RelPath,
    ) -> Result<Option<Vec<u8>>> {
        match repo {
            Some(repo) => Ok(repo.read_file(source, path)?),
            None => {
                let Source::Dir(dir) = source else {
                    return Err(Error::Ra(blastr_ra::Error::Internal(format!(
                        "cannot read {source:?} without a repository"
                    ))));
                };
                match std::fs::read(path.to_path(dir)) {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(Error::Io(error)),
                }
            }
        }
    }

    /// The head-side lines that differ from the base side, per changed file.
    ///
    /// Files that did not change in content are absent, and so are files that
    /// are not UTF-8 text, which the pipeline cannot compare.
    fn changed_lines_of(
        &self,
        repo: Option<&Repo>,
        comparison: &Comparison,
        changed_files: &[ChangedFile],
    ) -> Result<BTreeMap<RelPath, Vec<RangeInclusive<u32>>>> {
        let mut lines = BTreeMap::new();
        for file in changed_files {
            if !matches!(file.status, FileStatus::Added | FileStatus::Modified) {
                continue;
            }
            let Some(head) = self.read_source_file(repo, &comparison.head, &file.path)? else {
                continue;
            };
            let Some(head) = String::from_utf8(head).ok() else {
                continue;
            };
            let base = if file.status == FileStatus::Added {
                None
            } else {
                let old = file.old_path.as_ref().unwrap_or(&file.path);
                self.read_source_file(repo, &comparison.base, old)?
            };
            let base = base.and_then(|bytes| String::from_utf8(bytes).ok());
            let ranges = changed_lines(base.as_deref(), &head);
            if !ranges.is_empty() {
                lines.insert(file.path.clone(), ranges);
            }
        }
        Ok(lines)
    }

    /// The identity of one analysis, computed without loading the project.
    fn analysis_key(
        &self,
        repo: &Repo,
        source: &Source,
        content_id: &str,
        options: &AnalysisOptions,
        toolchain: Option<&str>,
    ) -> Result<AnalysisKey> {
        let mut features = options.features.clone();
        features.sort();
        features.dedup();

        let lock_file = project_file(options, LOCK_FILE);
        let cargo_lock = match repo.read_file(source, &lock_file)? {
            Some(bytes) => blake3::hash(&bytes).to_hex().to_string(),
            None => String::new(),
        };

        Ok(AnalysisKey {
            repository: repo.repository_id(),
            source: content_id.to_owned(),
            features,
            all_features: options.all_features,
            target: options.target.clone(),
            cfg_test: true,
            cargo_lock,
            toolchain: toolchain.unwrap_or(UNKNOWN_TOOLCHAIN).to_owned(),
            rust_analyzer: blastr_ra::RUST_ANALYZER_VERSION.to_owned(),
            blastr: env!("CARGO_PKG_VERSION").to_owned(),
            exec: options.exec,
            manifest_dir: options.manifest_dir.clone(),
        })
    }
}

/// Both graphs of a directory comparison, for tests.
#[derive(Debug, Clone)]
pub struct DiffOutput {
    /// The comparison of the two directories.
    pub result: DiffResult,
    /// The graph of the base directory.
    pub base: Arc<RevisionGraph>,
    /// The graph of the head directory.
    pub head: Arc<RevisionGraph>,
}

/// What building the graph of one revision produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexReport {
    /// The identity of the analysis.
    pub key: AnalysisKey,
    /// Whether the graph came from the cache.
    pub from_cache: bool,
    /// Number of symbols in the graph.
    pub symbols: usize,
    /// Number of edges in the graph.
    pub edges: usize,
    /// Number of unresolved references in the graph.
    pub unresolved: usize,
}

/// What the cache holds and where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheStatus {
    /// Number of cached graphs.
    pub graphs: u64,
    /// Total size of the cached graphs, in bytes.
    pub graph_bytes: u64,
    /// Number of materialised snapshots.
    pub snapshots: u64,
    /// Total size of the snapshots on disk, in bytes.
    pub snapshot_bytes: u64,
    /// Path of the cache database.
    pub database: PathBuf,
    /// Directory holding materialised snapshots.
    pub snapshots_dir: PathBuf,
}

/// The cache directories used by [`Engine::with_store`].
fn temporary_paths() -> blastr_store::Paths {
    let cache_dir = std::env::temp_dir().join("blastr");
    let config_dir = cache_dir.join("config");
    blastr_store::Paths {
        cache_dir,
        config_dir,
    }
}

/// The output of `rustc --version` run in `dir`, or `unknown` when it cannot
/// be run.
///
/// Reading the version costs one process per engine call, which is why it is
/// done once and passed down rather than per side.
fn toolchain_version(dir: &Path) -> String {
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

/// True when `path` is named `Cargo.toml`, at any depth.
fn is_manifest(path: &RelPath) -> bool {
    path.as_str().rsplit('/').next() == Some(MANIFEST_FILE)
}

/// `<manifest dir>/<name>`, relative to the root the paths are relative to.
fn project_file(options: &AnalysisOptions, name: &str) -> RelPath {
    match &options.manifest_dir {
        Some(dir) if !dir.as_str().is_empty() => RelPath::new(format!("{}/{name}", dir.as_str())),
        _ => RelPath::new(name),
    }
}

/// The directory holding the `Cargo.toml` to load, inside the snapshot at `root`.
fn manifest_dir_in(options: &AnalysisOptions, root: &Path) -> PathBuf {
    match &options.manifest_dir {
        Some(dir) => dir.to_path(root),
        None => root.to_path_buf(),
    }
}

/// Invalid UTF-8 counts as an absent file.
fn read_utf8(bytes: Option<Vec<u8>>) -> Option<String> {
    bytes.and_then(|bytes| String::from_utf8(bytes).ok())
}

/// Feature and dependency changes of the `Cargo.toml` files that changed in a
/// repository comparison.
fn manifest_changes_of_repo(
    repo: &Repo,
    comparison: &Comparison,
    changed_files: &[ChangedFile],
) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    for file in changed_files {
        if !is_manifest(&file.path) {
            continue;
        }
        let old_path = file.old_path.as_ref().unwrap_or(&file.path);
        let old = read_utf8(repo.read_file(&comparison.base, old_path)?);
        let new = read_utf8(repo.read_file(&comparison.head, &file.path)?);
        changes.extend(crate::diff::manifest_changes(
            &file.path,
            old.as_deref(),
            new.as_deref(),
        ));
    }
    Ok(changes)
}

/// Feature and dependency changes of the `Cargo.toml` files that differ
/// between two plain directories.
fn manifest_changes_of_dirs(
    base: &Path,
    head: &Path,
    changed_files: &[ChangedFile],
) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    for file in changed_files {
        if !is_manifest(&file.path) {
            continue;
        }
        let old_path = file.old_path.as_ref().unwrap_or(&file.path);
        let old = read_text_file(base, old_path)?;
        let new = read_text_file(head, &file.path)?;
        changes.extend(crate::diff::manifest_changes(
            &file.path,
            old.as_deref(),
            new.as_deref(),
        ));
    }
    Ok(changes)
}

/// Reads a file below `root` as text; a missing file, or one that is not
/// UTF-8, is reported as absent.
fn read_text_file(root: &Path, path: &RelPath) -> Result<Option<String>> {
    match std::fs::read(path.to_path(root)) {
        Ok(bytes) => Ok(read_utf8(Some(bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io(error)),
    }
}

/// What limits the completeness of a comparison.
fn collect_limits(
    repo: Option<&Repo>,
    head: &RevisionGraph,
    _changed_files: &[ChangedFile],
    changed_lines: &BTreeMap<RelPath, Vec<RangeInclusive<u32>>>,
    options: &AnalysisOptions,
) -> Result<Vec<Limit>> {
    let mut limits = Vec::new();
    if options.exec == ExecMode::Restricted {
        limits.push(Limit::Restricted);
    }
    if let Some(repo) = repo
        && repo.has_submodules()?
    {
        limits.push(Limit::Submodules);
    }
    if let Some(repo) = repo
        && repo.uses_lfs()?
    {
        limits.push(Limit::Lfs);
    }
    if !head.unresolved.is_empty() {
        limits.push(Limit::Unresolved {
            count: u32::try_from(head.unresolved.len()).unwrap_or(u32::MAX),
        });
    }
    // Only inactive code that the comparison actually touched is a limit; an
    // inactive block elsewhere in a changed file is not.
    limits.extend(
        head.inactive
            .iter()
            .filter(|location| {
                changed_lines.get(&location.file).is_some_and(|ranges| {
                    overlaps(ranges, location.span.start_line, location.span.end_line)
                })
            })
            .cloned()
            .map(Limit::ChangedButInactive),
    );
    Ok(limits)
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
fn remove_dir_if_present(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_under(dir: &Path) -> blastr_store::Paths {
        blastr_store::Paths {
            cache_dir: dir.to_path_buf(),
            config_dir: dir.join("config"),
        }
    }

    #[test]
    fn engine_uses_the_given_store() {
        let store = blastr_store::Store::open_in_memory().unwrap();
        let engine = Engine::with_store(store);
        let _ = engine.store();
    }

    #[test]
    fn fresh_cache_reports_zeros() {
        let dir = tempfile::tempdir().unwrap();
        let store = blastr_store::Store::open_in_memory().unwrap();
        let engine = Engine::with_store_and_paths(store, paths_under(dir.path()));

        let status = engine.cache_status().unwrap();
        assert_eq!(status.graphs, 0);
        assert_eq!(status.graph_bytes, 0);
        assert_eq!(status.snapshots, 0);
        assert_eq!(status.snapshot_bytes, 0);
        assert_eq!(status.database, dir.path().join("blastr.db"));
        assert_eq!(status.snapshots_dir, dir.path().join("snapshots"));
    }

    #[test]
    fn clear_reports_and_removes_graphs_and_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let store = blastr_store::Store::open_in_memory().unwrap();
        store.put_graph("key", "{}", b"graph").unwrap();
        let paths = paths_under(dir.path());

        let snapshot = paths.snapshots_dir().join("4f2a91c");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("lib.rs"), b"fn main() {}").unwrap();
        let build = paths.target_dir().join("debug");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("out"), b"x").unwrap();

        let engine = Engine::with_store_and_paths(store, paths.clone());
        let status = engine.cache_clear().unwrap();

        assert_eq!(status.graphs, 1);
        assert_eq!(status.graph_bytes, 5);
        assert_eq!(status.snapshots, 1);
        assert_eq!(status.snapshot_bytes, 12);
        assert!(engine.store().list_graphs().unwrap().is_empty());
        assert!(!paths.snapshots_dir().exists());
        assert!(!paths.target_dir().exists());
        assert_eq!(engine.cache_status().unwrap().graphs, 0);
        assert_eq!(engine.cache_status().unwrap().snapshots, 0);
    }
}
