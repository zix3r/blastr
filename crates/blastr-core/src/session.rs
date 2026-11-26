//! A long-lived analysis of one working tree, refreshed as files change.
//!
//! A fresh analysis has to load the project, prime rust-analyzer's
//! queries and extract every file before it can say anything. That is fine for
//! a single run and far too slow for the edit-save loop, in which a developer
//! changes a line and wants the impact back. A [`Session`] pays those costs
//! once: the project stays loaded and primed, so a refresh mainly re-extracts
//! the files that were saved.

use std::sync::Arc;

use blastr_git::Repo;
use blastr_model::{
    Analysis, AnalysisKey, AnalysisOptions, AnalysisParams, CancelToken, ChangedFile, Comparison,
    DiffResult, FileStatus, Progress, RelPath, RevSpec, RevisionGraph, Source,
};

use crate::cache::{Cache, Kind};
use crate::codec;
use crate::engine::{AnalysisRequest, Engine, analysis_key, open_repo, toolchain_version};
use crate::error::{Error, Result};
use crate::pipeline::{Loaded, Run, Sides, pipeline, union_of};

/// One working tree kept loaded for repeated analysis.
///
/// The session compares a fixed base side with the working tree. It keeps
/// rust-analyzer's database, so refreshing after a file is saved costs a
/// fraction of a fresh run.
pub struct Session {
    repo: Repo,
    spec: RevSpec,
    comparison: Comparison,
    options: AnalysisOptions,
    params: AnalysisParams,
    toolchain: String,
    sides: Sides,
    loaded: Loaded,
    analysis: Analysis,
    /// The content id of the working tree that `analysis` describes.
    seen: String,
    /// The [`Repo::worktree_stamp`] at which `seen` was last confirmed.
    stamp: String,
    /// The files whose text in the loaded project may differ from the base
    /// side: the ones that differed when the project was loaded or last
    /// refreshed.
    applied: Vec<RelPath>,
    /// The engine's cache, which keeps the graph of the working tree so that
    /// a what-if or a symbol search of the same content does not load the
    /// project a second time.
    cache: Cache,
}

impl Session {
    /// The analysis of the working tree as it was last refreshed.
    pub fn analysis(&self) -> &Analysis {
        &self.analysis
    }

    /// The two sides being compared.
    pub fn comparison(&self) -> &Comparison {
        &self.comparison
    }

    /// The graph of the base side.
    pub fn base_graph(&self) -> &Arc<RevisionGraph> {
        &self.sides.base
    }

    /// The graph of the working tree as it was last refreshed.
    pub fn head_graph(&self) -> &Arc<RevisionGraph> {
        &self.sides.head
    }

    /// Whether the base side moved since the session was opened.
    ///
    /// A commit, a rebase or a checkout changes what the working tree is being
    /// compared with, so the session has to be built again.
    pub fn base_moved(&self) -> Result<bool> {
        Ok(self.repo.resolve(&self.spec)?.base != self.comparison.base)
    }

    /// Whether the working tree differs from what this session analysed.
    ///
    /// Cheap: it compares the sizes and modification times of the files with
    /// those seen last, and only when one moved does it read and hash the files
    /// that differ from the base side. A file touched but not changed is not
    /// stale.
    pub fn is_stale(&mut self) -> Result<bool> {
        let stamp = self.repo.worktree_stamp()?;
        if stamp == self.stamp {
            return Ok(false);
        }
        let stale = worktree_id(&self.repo)? != self.seen;
        if !stale {
            self.stamp = stamp;
        }
        Ok(stale)
    }

    /// Re-extracts the working tree and recomputes the analysis, handing over
    /// the classified changes as soon as they are known.
    ///
    /// Files that no longer differ from the base side get their base text back,
    /// so reverting an edit is handled like any other edit.
    pub fn refresh(
        &mut self,
        progress: &dyn Progress,
        cancel: &CancelToken,
        on_changes: &mut dyn FnMut(&DiffResult),
    ) -> Result<&Analysis> {
        cancel.check()?;
        let repo = &self.repo;
        let run = Run {
            repo,
            options: &self.options,
            toolchain: &self.toolchain,
            progress,
            cancel,
        };
        // Taken before anything is read, so that a save during the refresh
        // makes the session stale again.
        let stamp = repo.worktree_stamp()?;
        let head_key = analysis_key(&run, &Source::Worktree)?;
        let seen = head_key
            .as_ref()
            .map(|key| key.source.clone())
            .unwrap_or_default();
        let changed_files = repo.changed_files(&self.comparison)?;
        let touched = touched_paths(&changed_files);
        // The loaded project keeps the text a file was last given. A file
        // edited when the session opened, or given its text by an earlier
        // refresh, that no longer differs from the base side is not among the
        // changed files, so it is given the text it has now.
        let mut edits = changed_files.clone();
        for path in &self.applied {
            if touched.contains(path) {
                continue;
            }
            let status = if repo.read_file(&Source::Worktree, path)?.is_some() {
                FileStatus::Modified
            } else {
                FileStatus::Deleted
            };
            edits.push(ChangedFile {
                path: path.clone(),
                old_path: None,
                status,
            });
        }
        progress.report("apply", edits.len() as u64, None);
        let head = self.loaded.overlay(&run, &Source::Worktree, &edits)?;
        self.applied = touched;
        self.sides.head = Arc::new(head);
        self.sides.head_key = head_key;
        self.analysis = pipeline(
            &run,
            &self.comparison,
            changed_files,
            &self.sides,
            &self.params,
            on_changes,
        )?;
        remember(
            &self.cache,
            &self.sides.head_key,
            &analysis_key(&run, &Source::Worktree)?,
            &self.sides.head,
            &self.loaded.missing_macros,
        );
        self.seen = seen;
        self.stamp = stamp;
        Ok(&self.analysis)
    }
}

impl Engine {
    /// Opens a session that keeps the working tree loaded, handing over the
    /// classified changes as soon as they are known.
    ///
    /// The head side must be the working tree, because that is what a session
    /// watches; the base side is whatever the spec means. The base graph is
    /// built or taken from the cache like any other, and the working tree is
    /// loaded in place, so the project on disk is the project that is analysed.
    pub fn session(
        &self,
        request: &AnalysisRequest<'_>,
        progress: &dyn Progress,
        cancel: &CancelToken,
        on_changes: &mut dyn FnMut(&DiffResult),
    ) -> Result<Session> {
        cancel.check()?;
        let repo = open_repo(request.repo_root)?;
        let comparison = repo.resolve(request.spec)?;
        if comparison.head != Source::Worktree {
            return Err(Error::SessionNotWatching(format!(
                "the newer side is {}",
                comparison.head
            )));
        }
        let toolchain = toolchain_version(repo.root());
        let run = Run {
            repo: &repo,
            options: request.options,
            toolchain: &toolchain,
            progress,
            cancel,
        };
        let base_key = analysis_key(&run, &comparison.base)?;
        let head_key = analysis_key(&run, &comparison.head)?;
        let changed_files = repo.changed_files(&comparison)?;
        let applied = touched_paths(&changed_files);

        let base = self.fetch(&run, &comparison.base, &base_key, "base")?;
        let loaded = self.load(&run, &Source::Worktree, "head")?;
        let head = loaded.extract(&run, "head")?;
        let sides = Sides {
            missing_macros: union_of(&base.missing_macros, &loaded.missing_macros),
            base: Arc::new(base.graph),
            base_key,
            head: Arc::new(head),
            head_key,
        };
        let analysis = pipeline(
            &run,
            &comparison,
            changed_files,
            &sides,
            request.params,
            on_changes,
        )?;
        // Taken after the load, which may write `Cargo.lock`.
        let stamp = repo.worktree_stamp()?;
        let loaded_key = analysis_key(&run, &Source::Worktree)?;
        let seen = loaded_key
            .as_ref()
            .map(|key| key.source.clone())
            .unwrap_or_default();
        remember(
            self.cache(),
            &sides.head_key,
            &loaded_key,
            &sides.head,
            &loaded.missing_macros,
        );
        Ok(Session {
            repo,
            spec: request.spec.clone(),
            comparison,
            options: request.options.clone(),
            params: request.params.clone(),
            toolchain,
            sides,
            loaded,
            analysis,
            seen,
            stamp,
            applied,
            cache: self.cache().clone(),
        })
    }
}

/// Stores the graph of the working tree under `key`, so that a what-if or a
/// symbol search of the same content reads it instead of loading the project
/// again: a load of a large workspace takes the better part of a minute, and
/// the session has just done one.
///
/// `before` is the key of the working tree before the graph was made, `after`
/// the one once it was; the graph is stored under `after`, whose lock-file
/// hash already counts a `Cargo.lock` the load wrote. It is only stored when
/// it is complete and the content did not change in between: a file saved
/// while the graph was made would otherwise leave a graph of one content under
/// the key of another. A cache that cannot be written costs only speed, so the
/// failure is logged.
fn remember(
    cache: &Cache,
    before: &Option<AnalysisKey>,
    after: &Option<AnalysisKey>,
    graph: &RevisionGraph,
    missing_macros: &[String],
) {
    let (Some(before), Some(key)) = (before, after) else {
        return;
    };
    if !missing_macros.is_empty() || before.source != key.source {
        return;
    }
    let stored =
        codec::encode(graph).and_then(|bytes| Ok(cache.put(Kind::Graph, &key.hash(), &bytes)?));
    if let Err(error) = stored {
        tracing::warn!("could not keep the graph of the working tree: {error}");
    }
}

/// The paths of `files`, with the old path of a renamed file.
fn touched_paths(files: &[ChangedFile]) -> Vec<RelPath> {
    files
        .iter()
        .flat_map(|file| std::iter::once(&file.path).chain(&file.old_path))
        .cloned()
        .collect()
}

/// The content id of the working tree as it is on disk now.
fn worktree_id(repo: &Repo) -> Result<String> {
    Ok(repo.content_id(&Source::Worktree)?.unwrap_or_default())
}
