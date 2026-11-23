//! The entry point of the analysis pipeline.
//!
//! [`Engine`] turns the two sides of a comparison into [`RevisionGraph`]
//! values (from the cache when possible, by loading the project with
//! rust-analyzer otherwise), hands them to the structural comparison in
//! [`crate::diff`], and on request carries the changes through the impact
//! analysis of [`crate::impact`].

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use blastr_git::Repo;
use blastr_model::{
    Analysis, AnalysisKey, AnalysisOptions, AnalysisParams, Assume, CancelToken, Change,
    ChangedFile, Comparison, DiffResult, ExecMode, FileStatus, Limit, Paths, Progress, RelPath,
    RevSpec, RevisionGraph, Source, SymbolPath,
};

use blastr_model::protocol::CacheStatus;

use crate::cache::{Cache, Kind};
use crate::codec;
use crate::error::{Error, Result};
use crate::hunks::{changed_lines, overlaps};
use crate::impact::ImpactInput;
use blastr_model::IgnoreSet;

/// The toolchain recorded in an analysis key when `rustc --version` cannot be run.
const UNKNOWN_TOOLCHAIN: &str = "unknown";

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

/// The file name whose changes are reported as manifest changes.
const MANIFEST_FILE: &str = "Cargo.toml";

/// The file name that fixes the dependency graph of the project.
const LOCK_FILE: &str = "Cargo.lock";

/// Text hashed into every result key ahead of the two analysis keys, so that a
/// result key can never repeat a digest computed for another purpose.
const RESULT_KEY_DOMAIN: &[u8] = b"analysis-1";

/// Everything one call to [`Engine::graph_for`] needs beyond the source.
pub(crate) struct GraphRequest<'a> {
    /// Configuration to analyse the project under.
    pub(crate) options: &'a AnalysisOptions,
    /// Which side this is, used in the progress phase names: `base` or `head`.
    pub(crate) phase: &'a str,
    /// Toolchain version to record in the analysis key; `None` when no key is built.
    pub(crate) toolchain: Option<&'a str>,
    /// Where progress is reported.
    pub(crate) progress: &'a dyn Progress,
    /// Cancellation flag.
    pub(crate) cancel: &'a CancelToken,
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
    /// The procedural-macro crates the graphs may be missing, sorted and
    /// without duplicates; empty when every macro the extracted code calls
    /// loaded.
    missing_macros: Vec<String>,
}

/// One graph as [`Engine::graph_for`] produced it.
pub(crate) struct FetchedGraph {
    /// The graph.
    pub(crate) graph: Arc<RevisionGraph>,
    /// The cache key it belongs to; `None` when the source has no content id.
    pub(crate) key: Option<AnalysisKey>,
    /// The procedural-macro crates the graph may be missing knowledge of,
    /// sorted and without duplicates.
    pub(crate) missing_macros: Vec<String>,
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

/// Everything one structural comparison reads beyond the revision spec.
struct CompareRequest<'a> {
    /// The repository the two sides belong to.
    repo: &'a Repo,
    /// The two sides being compared.
    comparison: &'a Comparison,
    /// Configuration to analyse the project under.
    options: &'a AnalysisOptions,
    /// Toolchain version to record in the analysis keys; `None` when no key is built.
    toolchain: Option<&'a str>,
    /// Where progress is reported.
    progress: &'a dyn Progress,
    /// Cancellation flag.
    cancel: &'a CancelToken,
}

/// A structural comparison together with the graphs it was computed from.
struct Compared {
    /// What was compared and what changed structurally.
    diff: DiffResult,
    /// The graph of the base side.
    base: Arc<RevisionGraph>,
    /// The graph of the head side.
    head: Arc<RevisionGraph>,
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

    /// Full impact analysis of a comparison in the repository at `repo_root`.
    /// Served from the result cache when possible.
    ///
    /// A comparison whose two graphs were analysed before with the same
    /// parameters is answered from the result cache without a snapshot or a
    /// project load. Otherwise progress reports the phases of the structural
    /// comparison followed by `impact`.
    pub fn analyze(
        &self,
        repo_root: &Path,
        spec: &RevSpec,
        options: &AnalysisOptions,
        params: &AnalysisParams,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Analysis> {
        self.analyze_with(
            repo_root,
            spec,
            options,
            params,
            progress,
            cancel,
            &mut |_| {},
        )
    }

    /// The same as [`Engine::analyze`], handing over the classified changes as
    /// soon as they are known and before the impact is traced.
    ///
    /// `on_changes` is not called when the whole analysis comes from the
    /// result cache, because then nothing arrives in steps.
    #[allow(clippy::too_many_arguments)]
    pub fn analyze_with(
        &self,
        repo_root: &Path,
        spec: &RevSpec,
        options: &AnalysisOptions,
        params: &AnalysisParams,
        progress: &dyn Progress,
        cancel: &CancelToken,
        on_changes: &mut dyn FnMut(&DiffResult),
    ) -> Result<Analysis> {
        cancel.check()?;
        let root = blastr_git::discover(repo_root)?;
        let repo = Repo::open(&root)?;
        let comparison = repo.resolve(spec)?;
        let toolchain = toolchain_version(repo.root());

        // Both analysis keys are known without loading anything, so a finished
        // analysis is found before any snapshot or project load starts.
        let base_key = self.key_for(
            Some(&repo),
            &comparison.base,
            options,
            Some(toolchain.as_str()),
        )?;
        let head_key = self.key_for(
            Some(&repo),
            &comparison.head,
            options,
            Some(toolchain.as_str()),
        )?;
        let key_hash = match (&base_key, &head_key) {
            (Some(base_key), Some(head_key)) => Some(result_key(base_key, head_key, params)?),
            // A side without a content id cannot be named, so its analysis
            // cannot be shared through the cache.
            _ => None,
        };
        if let Some(key_hash) = &key_hash
            && let Some(bytes) = self.cache.get(Kind::Result, key_hash)?
            // A damaged entry is not a failure of this run: the analysis is
            // computed again and stored over it.
            && let Ok(analysis) = codec::decode::<Analysis>(&bytes)
        {
            return Ok(analysis);
        }

        let compared = self.compare(CompareRequest {
            repo: &repo,
            comparison: &comparison,
            options,
            toolchain: Some(toolchain.as_str()),
            progress,
            cancel,
        })?;
        on_changes(&compared.diff);

        progress.report("impact", 0, None);
        let ignore = IgnoreSet::new(&params.ignore)?;
        let output = crate::impact::analyze(
            &ImpactInput {
                base: &compared.base,
                head: &compared.head,
                changes: &compared.diff.changes,
                params,
                ignore: &ignore,
            },
            cancel,
        )?;
        cancel.check()?;

        let analysis = Analysis {
            diff: compared.diff,
            items: output.items,
            tests: output.tests,
            limits: output.limits,
            params: params.clone(),
        };
        if let Some(key_hash) = &key_hash
            // An incomplete analysis may not be the answer of a later run, so
            // it is not stored: the next run tries the project again.
            && !missing_macros(&analysis.diff.limits)
        {
            self.cache
                .put(Kind::Result, key_hash, &codec::encode(&analysis)?)?;
        }
        Ok(analysis)
    }

    /// The same for two plain directories; nothing is cached.
    ///
    /// Progress reports the phases of [`Engine::diff_dirs`] followed by
    /// `impact`.
    pub fn analyze_dirs(
        &self,
        base: &Path,
        head: &Path,
        options: &AnalysisOptions,
        params: &AnalysisParams,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Analysis> {
        let compared = self.diff_dirs(base, head, options, progress, cancel)?;

        progress.report("impact", 0, None);
        let ignore = IgnoreSet::new(&params.ignore)?;
        let output = crate::impact::analyze(
            &ImpactInput {
                base: &compared.base,
                head: &compared.head,
                changes: &compared.result.changes,
                params,
                ignore: &ignore,
            },
            cancel,
        )?;
        cancel.check()?;

        Ok(Analysis {
            diff: compared.result,
            items: output.items,
            tests: output.tests,
            limits: output.limits,
            params: params.clone(),
        })
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
        let root = blastr_git::discover(repo_root)?;
        let repo = Repo::open(&root)?;
        let toolchain = toolchain_version(repo.root());

        let fetched = self.graph_for(
            Some(&repo),
            &Source::Worktree,
            GraphRequest {
                options,
                phase: "head",
                toolchain: Some(toolchain.as_str()),
                progress,
                cancel,
            },
        )?;
        cancel.check()?;
        let graph = fetched.graph;
        let key = fetched.key;
        let change = crate::impact::whatif::assumed_change(&graph, symbol, assume)?;
        // The working tree is compared with itself, so none of its lines
        // changed and only the limits that come from the repository itself and
        // from unresolved references apply.
        let limits = collect_limits(
            Some(&repo),
            &graph,
            &[],
            &BTreeMap::new(),
            options,
            &fetched.missing_macros,
        )?;

        progress.report("impact", 0, None);
        let ignore = IgnoreSet::new(&params.ignore)?;
        let changes = [change.clone()];
        let output = crate::impact::analyze(
            &ImpactInput {
                base: &graph,
                head: &graph,
                changes: &changes,
                params,
                ignore: &ignore,
            },
            cancel,
        )?;
        cancel.check()?;

        Ok(Analysis {
            diff: DiffResult {
                comparison: Comparison {
                    base: Source::Worktree,
                    head: Source::Worktree,
                },
                base_key: key.clone(),
                head_key: key,
                changed_files: Vec::new(),
                changes: vec![change],
                limits,
            },
            items: output.items,
            tests: output.tests,
            limits: output.limits,
            params: params.clone(),
        })
    }

    /// The structural comparison of both sides of `request`, together with the
    /// two graphs it was computed from.
    ///
    /// [`Engine::analyze`] starts here.
    fn compare(&self, request: CompareRequest<'_>) -> Result<Compared> {
        let CompareRequest {
            repo,
            comparison,
            options,
            toolchain,
            progress,
            cancel,
        } = request;
        cancel.check()?;
        let changed_files = repo.changed_files(comparison)?;

        let sides = match self.overlay_graphs(OverlayRequest {
            repo: Some(repo),
            comparison,
            changed_files: &changed_files,
            options,
            toolchain,
            progress,
            cancel,
        })? {
            Some(sides) => sides,
            None => {
                // The two sides are different projects, or one of them is
                // cached, so each side is loaded on its own.
                let base = self.graph_for(
                    Some(repo),
                    &comparison.base,
                    GraphRequest {
                        options,
                        phase: "base",
                        toolchain,
                        progress,
                        cancel,
                    },
                )?;
                cancel.check()?;
                let head = self.graph_for(
                    Some(repo),
                    &comparison.head,
                    GraphRequest {
                        options,
                        phase: "head",
                        toolchain,
                        progress,
                        cancel,
                    },
                )?;
                cancel.check()?;
                let missing_macros = union_of(&base.missing_macros, &head.missing_macros);
                Sides {
                    base: base.graph,
                    base_key: base.key,
                    head: head.graph,
                    head_key: head.key,
                    missing_macros,
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
        changes.extend(manifest_changes_of_repo(repo, comparison, &changed_files)?);
        let changed_lines = self.changed_lines_of(Some(repo), comparison, &changed_files)?;
        let limits = collect_limits(
            Some(repo),
            &sides.head,
            &changed_files,
            &changed_lines,
            options,
            &sides.missing_macros,
        )?;

        Ok(Compared {
            diff: DiffResult {
                comparison: comparison.clone(),
                base_key: sides.base_key,
                head_key: sides.head_key,
                changed_files,
                changes,
                limits,
            },
            base: sides.base,
            head: sides.head,
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

        let (base_graph, missing_base) = {
            let fetched = self.graph_for(
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
            (fetched.graph, fetched.missing_macros)
        };
        cancel.check()?;
        let (head_graph, missing_head) = {
            let fetched = self.graph_for(
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
            (fetched.graph, fetched.missing_macros)
        };
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
        let missing_macros = union_of(&missing_base, &missing_head);
        let limits = collect_limits(
            None,
            &head_graph,
            &changed_files,
            &changed_lines,
            options,
            &missing_macros,
        )?;

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
        let root = blastr_git::discover(repo_root)?;
        let repo = Repo::open(&root)?;
        let toolchain = toolchain_version(repo.root());
        let fetched = self.graph_for(
            Some(&repo),
            source,
            GraphRequest {
                options,
                phase: "head",
                toolchain: Some(toolchain.as_str()),
                progress,
                cancel,
            },
        )?;
        Ok(fetched.graph)
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

    /// Loads the graph of one side of a comparison, from the cache when the
    /// same content was analysed before.
    ///
    /// The returned graph, its analysis key (`None` when the source has no
    /// content id, which is the case for a plain directory and for a working
    /// tree without a repository) and the
    /// procedural-macro crates it may be missing.
    ///
    /// A graph made from a project whose macros did not all load is not stored:
    /// an incomplete graph may not be the answer of a later run, which has a
    /// chance of building the macros or of the server starting properly.
    ///
    /// `toolchain` is the toolchain version to record in the key; it is only
    /// read when a key is built.
    pub(crate) fn graph_for(
        &self,
        repo: Option<&Repo>,
        source: &Source,
        request: GraphRequest<'_>,
    ) -> Result<FetchedGraph> {
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
            && let Some(bytes) = self.cache.get(Kind::Graph, &key.hash())?
            && let Ok(graph) = codec::decode::<RevisionGraph>(&bytes)
        {
            return Ok(FetchedGraph {
                graph: Arc::new(graph),
                key: Some(key.clone()),
                // An incomplete graph is never stored, so what came back is
                // complete whatever the machine that produced it could build.
                missing_macros: Vec::new(),
            });
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
        let (graph, missing_macros) = {
            let workspace = blastr_ra::Workspace::load(
                &root_dir,
                &manifest_dir,
                &load_options,
                progress,
                cancel,
            )?;
            cancel.check()?;
            let missing_macros = workspace.missing_proc_macros().to_vec();
            progress.report(&format!("extract {phase}"), 0, None);
            (workspace.extract(progress, cancel)?, missing_macros)
        };

        if missing_macros.is_empty() {
            self.store_graph(&key, &graph)?;
        }
        Ok(FetchedGraph {
            graph: Arc::new(graph),
            key,
            missing_macros,
        })
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
        let in_cache = |key: &Option<AnalysisKey>| {
            key.as_ref()
                .is_some_and(|key| self.cache.contains(Kind::Graph, &key.hash()))
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
        let (base, head, missing_macros) = {
            progress.report("load base", 0, None);
            let mut workspace = blastr_ra::Workspace::load(
                &root_dir,
                &manifest_dir,
                &load_options,
                progress,
                cancel,
            )?;
            cancel.check()?;
            let missing_macros = workspace.missing_proc_macros().to_vec();
            progress.report("extract base", 0, None);
            let base = workspace.extract(progress, cancel)?;
            cancel.check()?;

            let edits = self.file_edits(repo, &comparison.head, changed_files)?;
            workspace.apply(&edits)?;
            cancel.check()?;
            progress.report("extract head", 0, None);
            let head = workspace.extract(progress, cancel)?;
            cancel.check()?;
            (base, head, missing_macros)
        };

        // One project made both graphs, so what its macros could not expand is
        // missing from both; neither may be cached in that case.
        if missing_macros.is_empty() {
            self.store_graph(&base_key, &base)?;
            self.store_graph(&head_key, &head)?;
        }
        Ok(Some(Sides {
            base: Arc::new(base),
            base_key,
            head: Arc::new(head),
            head_key,
            missing_macros,
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
    /// This is the code path [`Engine::analyze`] takes when both sides are the same
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
    pub(crate) fn key_for(
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
    pub(crate) fn load_options(&self, options: &AnalysisOptions) -> blastr_ra::LoadOptions {
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
        self.cache
            .put(Kind::Graph, &key.hash(), &codec::encode(graph)?)?;
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
            blastr: build_id().to_owned(),
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
pub(crate) fn manifest_changes_of_repo(
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
pub(crate) fn collect_limits(
    repo: Option<&Repo>,
    head: &RevisionGraph,
    _changed_files: &[ChangedFile],
    changed_lines: &BTreeMap<RelPath, Vec<RangeInclusive<u32>>>,
    options: &AnalysisOptions,
    missing_macros: &[String],
) -> Result<Vec<Limit>> {
    let mut limits = Vec::new();
    if options.exec == ExecMode::Restricted {
        limits.push(Limit::Restricted);
    }
    limits.extend(macros_limit(missing_macros));
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

/// The limit for procedural-macro crates whose macros the extracted code calls
/// but did not get, if there are any.
pub(crate) fn macros_limit(missing: &[String]) -> Option<Limit> {
    if missing.is_empty() {
        return None;
    }
    let mut crates = missing.to_vec();
    crates.sort();
    crates.dedup();
    Some(Limit::MacrosNotExpanded { crates })
}

/// Whether a result is incomplete because procedural macros did not expand.
///
/// Such a result may never be cached: it is not an answer, it is what could be
/// made of a project whose macros the environment could not build.
fn missing_macros(limits: &[Limit]) -> bool {
    limits
        .iter()
        .any(|limit| matches!(limit, Limit::MacrosNotExpanded { .. }))
}

/// The union of two lists of crates that did not bring their macros, sorted and
/// without duplicates.
pub(crate) fn union_of(left: &[String], right: &[String]) -> Vec<String> {
    let mut union: Vec<String> = left.iter().chain(right).cloned().collect();
    union.sort();
    union.dedup();
    union
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
    use blastr_model::{ImpactLimits, TestSelection};

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
