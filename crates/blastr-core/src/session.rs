//! A long-lived analysis of one working tree, refreshed as files change.
//!
//! A fresh analysis has to load the project, prime rust-analyzer's
//! queries and extract every file before it can say anything. That is fine for
//! a single run and far too slow for the edit-save loop, in which a developer
//! changes a line and wants the impact back. A [`Session`] pays those costs
//! once: the project stays loaded and primed, so a refresh mainly re-extracts
//! the files that were saved.

use std::path::Path;
use std::sync::Arc;

use blastr_git::Repo;
use blastr_model::{
    Analysis, AnalysisKey, AnalysisOptions, AnalysisParams, CancelToken, ChangedFile, Comparison,
    DiffResult, FileStatus, Progress, RevSpec, RevisionGraph, Source,
};

use crate::config::IgnoreSet;
use crate::engine::{
    Engine, GraphRequest, macros_limit, manifest_changes_of_repo, toolchain_version, union_of,
};
use crate::error::{Error, Result};
use crate::impact::ImpactInput;

/// What a refresh found.
#[derive(Debug, Clone, PartialEq)]
pub struct Refresh {
    /// The analysis of the working tree as it is now.
    pub analysis: Analysis,
    /// Files that differ from the base side, whether or not they changed since
    /// the previous refresh.
    pub changed_files: usize,
}

/// What a session is opened on.
#[derive(Debug, Clone, Copy)]
pub struct SessionRequest<'a> {
    /// Any directory inside the repository.
    pub repo_root: &'a Path,
    /// The comparison to watch; its newer side must be the working tree.
    pub spec: &'a RevSpec,
    /// Configuration to analyse the project under.
    pub options: &'a AnalysisOptions,
    /// The parameters of the impact analysis.
    pub params: &'a AnalysisParams,
}

/// One working tree kept loaded for repeated analysis.
///
/// The session compares a fixed base side with the working tree. It keeps
/// rust-analyzer's database, so refreshing after a file is saved costs a
/// fraction of a fresh run.
pub struct Session {
    root: blastr_git::RepoRoot,
    spec: RevSpec,
    comparison: Comparison,
    params: AnalysisParams,
    ignore: IgnoreSet,
    base: Arc<RevisionGraph>,
    base_key: Option<AnalysisKey>,
    head: Arc<RevisionGraph>,
    head_key: Option<AnalysisKey>,
    workspace: blastr_ra::Workspace,
    /// The procedural-macro crates the loaded project could not use, sorted and
    /// without duplicates. They do not change while the session lives, because
    /// the project is not loaded again.
    missing_macros: Vec<String>,
    analysis: Analysis,
    seen: String,
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
        &self.base
    }

    /// The graph of the working tree as it was last refreshed.
    pub fn head_graph(&self) -> &Arc<RevisionGraph> {
        &self.head
    }

    /// Whether the base side moved since the session was opened.
    ///
    /// A commit, a rebase or a checkout changes what the working tree is being
    /// compared with, so the session has to be built again.
    pub fn base_moved(&self) -> Result<bool> {
        let repo = Repo::open(&self.root)?;
        let spec = RevSpec {
            rev: self.spec.rev.clone(),
            worktree: self.spec.worktree,
            staged: self.spec.staged,
            parent: self.spec.parent,
        };
        Ok(repo.resolve(&spec)?.base != self.comparison.base)
    }

    /// Whether the working tree differs from what this session analysed.
    ///
    /// Cheap: it reads the files that differ from the base side and hashes
    /// them, without touching rust-analyzer.
    pub fn is_stale(&self) -> Result<bool> {
        let repo = Repo::open(&self.root)?;
        Ok(self.content_id(&repo)? != self.seen)
    }

    /// Re-extracts the working tree and recomputes the analysis.
    ///
    /// Files that no longer differ from the base side get their base text back,
    /// so reverting an edit is handled like any other edit.
    pub fn refresh(&mut self, progress: &dyn Progress, cancel: &CancelToken) -> Result<Refresh> {
        self.refresh_with(progress, cancel, &mut |_| {})
    }

    /// The same as [`Session::refresh`], handing over the classified changes
    /// as soon as they are known and before the impact is traced.
    ///
    /// A front end that shows results as they arrive lists the changes from
    /// `on_changes` while the rest is still being computed.
    pub fn refresh_with(
        &mut self,
        progress: &dyn Progress,
        cancel: &CancelToken,
        on_changes: &mut dyn FnMut(&DiffResult),
    ) -> Result<Refresh> {
        cancel.check()?;
        let repo = Repo::open(&self.root)?;
        let changed_files = repo.changed_files(&self.comparison)?;
        let edits = self.edits(&changed_files)?;
        cancel.check()?;

        progress.report("apply", edits.len() as u64, None);
        self.workspace.apply(&edits)?;
        cancel.check()?;

        progress.report("extract head", 0, None);
        let head = Arc::new(self.workspace.extract(progress, cancel)?);
        cancel.check()?;

        progress.report("classify", 0, None);
        let mut changes = crate::diff::classify(
            &self.base,
            &head,
            &changed_files,
            &crate::diff::MatchParams::default(),
        );
        changes.extend(manifest_changes_of_repo(
            &repo,
            &self.comparison,
            &changed_files,
        )?);
        let diff = DiffResult {
            comparison: self.comparison.clone(),
            base_key: self.base_key.clone(),
            head_key: self.head_key.clone(),
            changed_files,
            changes,
            limits: macros_limit(&self.missing_macros).into_iter().collect(),
        };
        on_changes(&diff);

        progress.report("impact", 0, None);
        let output = crate::impact::analyze(
            &ImpactInput {
                base: &self.base,
                head: &head,
                changes: &diff.changes,
                params: &self.params,
                ignore: &self.ignore,
            },
            cancel,
        )?;
        cancel.check()?;

        let changed_files = diff.changed_files.len();
        self.head = head;
        self.analysis = Analysis {
            diff,
            items: output.items,
            tests: output.tests,
            limits: output.limits,
            params: self.params.clone(),
        };
        self.seen = self.content_id(&repo)?;
        Ok(Refresh {
            analysis: self.analysis.clone(),
            changed_files,
        })
    }

    /// The content id of the working tree as it is on disk now.
    fn content_id(&self, repo: &Repo) -> Result<String> {
        Ok(repo.content_id(&Source::Worktree)?.unwrap_or_default())
    }

    /// The texts the loaded project needs so that it matches the working tree.
    fn edits(&self, changed_files: &[ChangedFile]) -> Result<Vec<blastr_ra::FileEdit>> {
        let mut edits = Vec::new();
        for file in changed_files {
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
            // Text that is not UTF-8 cannot be a Rust source file, and
            // rust-analyzer would drop it as well.
            let content = std::fs::read(file.path.to_path(self.root.path()))
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            if let Some(content) = content {
                edits.push(blastr_ra::FileEdit {
                    path: file.path.clone(),
                    content: Some(content),
                });
            }
        }
        Ok(edits)
    }
}

impl Engine {
    /// Opens a session that keeps the working tree loaded.
    ///
    /// The head side must be the working tree, because that is what a session
    /// watches; the base side is whatever `spec` means. The base graph is built
    /// or taken from the cache like any other, and the working tree is loaded in
    /// place, so the project on disk is the project that is analysed.
    pub fn session(
        &self,
        repo_root: &Path,
        spec: &RevSpec,
        options: &AnalysisOptions,
        params: &AnalysisParams,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Session> {
        self.session_with(
            SessionRequest {
                repo_root,
                spec,
                options,
                params,
            },
            progress,
            cancel,
            &mut |_| {},
        )
    }

    /// The same as [`Engine::session`], handing over the classified changes
    /// as soon as they are known and before the impact is traced.
    pub fn session_with(
        &self,
        request: SessionRequest<'_>,
        progress: &dyn Progress,
        cancel: &CancelToken,
        on_changes: &mut dyn FnMut(&DiffResult),
    ) -> Result<Session> {
        let SessionRequest {
            repo_root,
            spec,
            options,
            params,
        } = request;
        cancel.check()?;
        let root = blastr_git::discover(repo_root)?;
        let repo = Repo::open(&root)?;
        let comparison = repo.resolve(spec)?;
        if comparison.head != Source::Worktree {
            return Err(Error::SessionNotWatching(format!(
                "the newer side is {}",
                comparison.head
            )));
        }
        let toolchain = toolchain_version(repo.root());
        let head_key = self.key_for(Some(&repo), &comparison.head, options, Some(&toolchain))?;
        let changed_files = repo.changed_files(&comparison)?;

        progress.report("load base", 0, None);
        let fetched = self.graph_for(
            Some(&repo),
            &comparison.base,
            GraphRequest {
                options,
                phase: "base",
                toolchain: Some(&toolchain),
                progress,
                cancel,
            },
        )?;
        cancel.check()?;
        let base = fetched.graph;
        let base_key = fetched.key;
        let base_missing = fetched.missing_macros;

        let manifest_dir = match &options.manifest_dir {
            Some(dir) => dir.to_path(root.path()),
            None => root.path().to_path_buf(),
        };
        progress.report("load head", 0, None);
        let workspace = blastr_ra::Workspace::load(
            root.path(),
            &manifest_dir,
            &self.load_options(options),
            progress,
            cancel,
        )?;
        cancel.check()?;

        progress.report("extract head", 0, None);
        let missing_macros = union_of(&base_missing, workspace.missing_proc_macros());
        let head = Arc::new(workspace.extract(progress, cancel)?);
        cancel.check()?;

        progress.report("classify", 0, None);
        let mut changes = crate::diff::classify(
            &base,
            &head,
            &changed_files,
            &crate::diff::MatchParams::default(),
        );
        changes.extend(manifest_changes_of_repo(
            &repo,
            &comparison,
            &changed_files,
        )?);
        let diff = DiffResult {
            comparison: comparison.clone(),
            base_key: base_key.clone(),
            head_key: head_key.clone(),
            changed_files,
            changes,
            limits: macros_limit(&missing_macros).into_iter().collect(),
        };
        on_changes(&diff);

        progress.report("impact", 0, None);
        let ignore = IgnoreSet::new(&params.ignore)?;
        let output = crate::impact::analyze(
            &ImpactInput {
                base: &base,
                head: &head,
                changes: &diff.changes,
                params,
                ignore: &ignore,
            },
            cancel,
        )?;
        cancel.check()?;

        let analysis = Analysis {
            diff,
            items: output.items,
            tests: output.tests,
            limits: output.limits,
            params: params.clone(),
        };
        let seen = repo.content_id(&Source::Worktree)?.unwrap_or_default();
        Ok(Session {
            root,
            spec: spec.clone(),
            comparison,
            params: params.clone(),
            ignore,
            base,
            base_key,
            head,
            head_key,
            workspace,
            missing_macros,
            analysis,
            seen,
        })
    }
}
