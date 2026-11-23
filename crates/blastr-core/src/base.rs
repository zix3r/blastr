//! One project loaded once, compared with many edited variants of itself.
//!
//! Loading a project through rust-analyzer costs seconds; extracting a graph
//! from an already loaded project costs a fraction of that. A comparison of two
//! directories pays for one load per side, which is the right trade for a user
//! who asks once, and the wrong one for a caller that asks the same question a
//! hundred times: seeded-edit harnesses, ranking searches and any other sweep
//! over "what if this definition were different".
//!
//! [`ProjectBase`] loads the project in a directory, extracts its graph, and
//! then extracts any number of edited variants of it — one file at a time or
//! several — without loading Cargo again. Every variant is compared with the
//! base graph and analysed like [`Engine::analyze_dirs`] would have analysed
//! the same two directories.
//!
//! [`Engine::analyze_dirs`]: crate::Engine::analyze_dirs

use std::path::{Path, PathBuf};
use std::sync::Arc;

use blastr_model::{
    Analysis, AnalysisOptions, AnalysisParams, CancelToken, ChangedFile, Comparison, DiffResult,
    FileStatus, Progress, RelPath, RevisionGraph, Source,
};

use crate::config::IgnoreSet;
use crate::engine::{Engine, collect_limits};
use crate::error::{Error, Result};
use crate::hunks::changed_lines;
use crate::impact::ImpactInput;

/// An edit of one file of a project: new content, or none to remove it.
pub use blastr_ra::FileEdit;

/// A project loaded once, from which edited variants are extracted.
///
/// The base state is what the directory holds; a variant is that state with
/// one or more files replaced. The project itself is unchanged throughout: the
/// edits live in the loaded project's in-memory file system, and the text of
/// the previous variant is put back before the next one is applied.
pub struct ProjectBase {
    /// The directory the project was loaded from.
    dir: PathBuf,
    /// The configuration the project was loaded under.
    options: AnalysisOptions,
    /// The loaded project.
    workspace: blastr_ra::Workspace,
    /// The graph of the directory as it is on disk.
    base: Arc<RevisionGraph>,
    /// The procedural-macro crates the project could not use, sorted and
    /// without duplicates. They do not change while the handle lives.
    missing_macros: Vec<String>,
    /// The text the files that the previous variant replaced had, so that the
    /// next variant starts from the base state.
    restore: Vec<blastr_ra::FileEdit>,
}

/// One extracted variant of a [`ProjectBase`]: the graph of the edited project
/// and the analysis of how it differs from the base.
#[derive(Debug, Clone)]
pub struct Variant {
    /// The graph of the directory as it is on disk.
    pub base: Arc<RevisionGraph>,
    /// The graph of the same project with the edits applied.
    pub head: Arc<RevisionGraph>,
    /// The comparison of the two and its impact.
    pub analysis: Analysis,
}

impl ProjectBase {
    /// The graph of the directory as it is on disk.
    pub fn graph(&self) -> &Arc<RevisionGraph> {
        &self.base
    }

    /// The procedural-macro crates the loaded project could not use.
    pub fn missing_proc_macros(&self) -> &[String] {
        &self.missing_macros
    }

    /// The directory the project was loaded from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Extracts the graph of the project with `edits` applied.
    ///
    /// The edits are relative to the directory the project was loaded from, and
    /// a `None` content removes a file. The base text of every edited file is
    /// remembered and put back before the next call, so a series of variants
    /// never leaks into each other; a variant that changes a file twice is
    /// therefore also the same as one that changes it once.
    pub fn variant_graph(
        &mut self,
        edits: &[blastr_ra::FileEdit],
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Arc<RevisionGraph>> {
        cancel.check()?;
        if !self.restore.is_empty() {
            let restore = std::mem::take(&mut self.restore);
            self.workspace.apply(&restore)?;
        }
        cancel.check()?;

        let mut restore = Vec::with_capacity(edits.len());
        for edit in edits {
            let original = match std::fs::read_to_string(edit.path.to_path(&self.dir)) {
                Ok(text) => Some(text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(Error::Io(error)),
            };
            restore.push(blastr_ra::FileEdit {
                path: edit.path.clone(),
                content: original,
            });
        }
        self.restore = restore;

        self.workspace.apply(edits)?;
        cancel.check()?;
        let graph = self.workspace.extract(progress, cancel)?;
        Ok(Arc::new(graph))
    }

    /// Extracts the graph of the project with `edits` applied, compares it with
    /// the base graph and analyses the impact of the difference.
    ///
    /// The result is what [`Engine::analyze_dirs`](crate::Engine::analyze_dirs)
    /// produces for a copy of this directory with the same edits written into
    /// it, except that the head side is named after the base directory: the
    /// edits are recorded in [`Variant`]'s graph and not in the comparison.
    pub fn variant(
        &mut self,
        edits: &[blastr_ra::FileEdit],
        params: &AnalysisParams,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Variant> {
        let changed_files = self.changed_files(edits)?;
        let head = self.variant_graph(edits, progress, cancel)?;
        cancel.check()?;

        let mut changes = crate::diff::classify(
            &self.base,
            &head,
            &changed_files,
            &crate::diff::MatchParams::default(),
        );
        changes.extend(manifest_changes(&self.dir, &changed_files)?);

        let changed_lines = self.changed_lines_of(&changed_files)?;
        let limits = collect_limits(
            None,
            &head,
            &changed_files,
            &changed_lines,
            &self.options,
            &self.missing_macros,
        )?;

        let diff = DiffResult {
            comparison: Comparison {
                base: Source::Dir(self.dir.clone()),
                head: Source::Dir(self.dir.clone()),
            },
            base_key: None,
            head_key: None,
            changed_files,
            changes,
            limits,
        };

        progress.report("impact", 0, None);
        let ignore = IgnoreSet::new(&params.ignore)?;
        let output = crate::impact::analyze(
            &ImpactInput {
                base: &self.base,
                head: &head,
                changes: &diff.changes,
                params,
                ignore: &ignore,
            },
            cancel,
        )?;
        cancel.check()?;

        Ok(Variant {
            base: Arc::clone(&self.base),
            head,
            analysis: Analysis {
                diff,
                items: output.items,
                tests: output.tests,
                limits: output.limits,
                params: params.clone(),
            },
        })
    }

    /// The files the edits really change, comparing them with what is on disk.
    fn changed_files(&self, edits: &[blastr_ra::FileEdit]) -> Result<Vec<ChangedFile>> {
        let mut files = Vec::new();
        for edit in edits {
            let base = match std::fs::read(edit.path.to_path(&self.dir)) {
                Ok(bytes) => String::from_utf8(bytes).ok(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(Error::Io(error)),
            };
            let status = match (&base, &edit.content) {
                (None, Some(_)) => FileStatus::Added,
                (Some(_), None) => FileStatus::Deleted,
                (Some(text), Some(new)) if text != new => FileStatus::Modified,
                // Unchanged text is not a change, whatever the caller meant.
                _ => continue,
            };
            files.push(ChangedFile {
                path: edit.path.clone(),
                old_path: None,
                status,
            });
        }
        Ok(files)
    }

    /// The head-side lines that differ from the base side, per changed file.
    fn changed_lines_of(
        &self,
        changed_files: &[ChangedFile],
    ) -> Result<std::collections::BTreeMap<RelPath, Vec<std::ops::RangeInclusive<u32>>>> {
        let mut lines = std::collections::BTreeMap::new();
        for file in changed_files {
            if !matches!(file.status, FileStatus::Added | FileStatus::Modified) {
                continue;
            }
            let Some(head) = read_text(&file.path, &self.dir)? else {
                continue;
            };
            let base = if file.status == FileStatus::Added {
                None
            } else {
                read_text(file.old_path.as_ref().unwrap_or(&file.path), &self.dir)?
            };
            let ranges = changed_lines(base.as_deref(), &head);
            if !ranges.is_empty() {
                lines.insert(file.path.clone(), ranges);
            }
        }
        Ok(lines)
    }
}

impl Engine {
    /// Loads the Cargo project in `dir` once and returns a handle that extracts
    /// edited variants of it without loading it again.
    ///
    /// The directory is loaded as it is: callers that need a pristine copy of a
    /// project they are about to edit write the copy first and load that.
    pub fn project_base(
        &self,
        dir: &Path,
        options: &AnalysisOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<ProjectBase> {
        cancel.check()?;
        let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        progress.report("load", 0, None);
        let workspace = blastr_ra::Workspace::load(
            &root,
            &root,
            &self.load_options(options),
            progress,
            cancel,
        )?;
        cancel.check()?;
        let missing_macros = workspace.missing_proc_macros().to_vec();
        progress.report("extract", 0, None);
        let base = workspace.extract(progress, cancel)?;
        cancel.check()?;
        Ok(ProjectBase {
            dir: root,
            options: options.clone(),
            workspace,
            base: Arc::new(base),
            missing_macros,
            restore: Vec::new(),
        })
    }
}

/// The text of `path` below `root`; a missing file or one that is not UTF-8 is
/// reported as absent, as the engine does for the same comparison.
fn read_text(path: &RelPath, root: &Path) -> Result<Option<String>> {
    match std::fs::read(path.to_path(root)) {
        Ok(bytes) => Ok(String::from_utf8(bytes).ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io(error)),
    }
}

/// Feature and dependency changes between the `Cargo.toml` files of a variant.
fn manifest_changes(
    root: &Path,
    changed_files: &[ChangedFile],
) -> Result<Vec<blastr_model::Change>> {
    let mut changes = Vec::new();
    for file in changed_files {
        if file.path.as_str().rsplit('/').next() != Some("Cargo.toml") {
            continue;
        }
        let old_path = file.old_path.as_ref().unwrap_or(&file.path);
        let old = read_text(old_path, root)?;
        let new = read_text(&file.path, root)?;
        changes.extend(crate::diff::manifest_changes(
            &file.path,
            old.as_deref(),
            new.as_deref(),
        ));
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::ChangeKind;

    /// A small crate whose only file the tests edit.
    fn project(dir: &Path) {
        std::fs::create_dir_all(dir.join("src")).expect("create the source directory");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"base_app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("write the manifest");
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn double(value: i64) -> i64 {\n    value * 2\n}\n\n\
             pub fn total() -> i64 {\n    double(2) + 1\n}\n",
        )
        .expect("write the source");
    }

    fn engine(dir: &Path) -> Engine {
        let paths = blastr_model::Paths {
            cache_dir: dir.to_path_buf(),
            config_dir: dir.join("config"),
        };
        Engine::with_paths(paths)
    }

    /// One edit replacing `src/lib.rs`.
    fn edit(text: &str) -> blastr_ra::FileEdit {
        blastr_ra::FileEdit {
            path: RelPath::new("src/lib.rs"),
            content: Some(text.to_owned()),
        }
    }

    #[test]
    fn variants_do_not_leak_into_each_other() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let root = dir.path().join("project");
        project(&root);
        let engine = engine(dir.path());
        let options = AnalysisOptions::default();
        let cancel = CancelToken::new();
        let mut base = engine
            .project_base(&root, &options, &blastr_model::NoProgress, &cancel)
            .expect("the project loads");

        // The base graph is what is on disk.
        assert!(
            base.graph()
                .symbols
                .iter()
                .any(|symbol| symbol.path.path == "base_app::total")
        );

        // A variant whose body changed: one body change, nothing else.
        let first = base
            .variant(
                &[edit(
                    "pub fn double(value: i64) -> i64 {\n    value + 2\n}\n\n\
                     pub fn total() -> i64 {\n    double(2) + 1\n}\n",
                )],
                &AnalysisParams::default(),
                &blastr_model::NoProgress,
                &cancel,
            )
            .expect("the first variant extracts");
        let kinds: Vec<ChangeKind> = first
            .analysis
            .diff
            .changes
            .iter()
            .map(|change| change.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![ChangeKind::Body],
            "{:?}",
            first.analysis.diff.changes
        );
        // `total` calls `double`, so it is reached.
        assert!(
            first
                .analysis
                .items
                .iter()
                .any(|item| item.symbol.path == "base_app::total")
        );

        // A second variant that only adds a comment: the text of the first
        // variant must be gone, so nothing but a non-semantic change is left.
        let second = base
            .variant(
                &[edit(
                    "// a comment\n\
                     pub fn double(value: i64) -> i64 {\n    value * 2\n}\n\n\
                     pub fn total() -> i64 {\n    double(2) + 1\n}\n",
                )],
                &AnalysisParams::default(),
                &blastr_model::NoProgress,
                &cancel,
            )
            .expect("the second variant extracts");
        assert!(
            !second
                .analysis
                .diff
                .changes
                .iter()
                .any(|change| change.kind == ChangeKind::Body),
            "the first variant's body must not survive into the second: {:?}",
            second.analysis.diff.changes
        );

        // A third variant with the signature changed.
        let third = base
            .variant(
                &[edit(
                    "pub fn double(value: i64, extra: i64) -> i64 {\n    value * 2 + extra\n}\n\n\
                     pub fn total() -> i64 {\n    double(2, 0) + 1\n}\n",
                )],
                &AnalysisParams::default(),
                &blastr_model::NoProgress,
                &cancel,
            )
            .expect("the third variant extracts");
        assert!(third.analysis.diff.changes.iter().any(|change| {
            change.symbol.path == "base_app::double" && change.kind == ChangeKind::Signature
        }));

        // Extracting the base state again gives the base graph back.
        let restored = base
            .variant_graph(&[], &blastr_model::NoProgress, &cancel)
            .expect("the base extracts again");
        assert_eq!(restored.symbols, base.graph().symbols);
        assert_eq!(restored.edges, base.graph().edges);
    }

    #[test]
    fn unchanged_text_is_not_a_change() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let root = dir.path().join("project");
        project(&root);
        let engine = engine(dir.path());
        let options = AnalysisOptions::default();
        let cancel = CancelToken::new();
        let mut base = engine
            .project_base(&root, &options, &blastr_model::NoProgress, &cancel)
            .expect("the project loads");
        let text = std::fs::read_to_string(root.join("src/lib.rs")).expect("read the source");
        let variant = base
            .variant(
                &[edit(&text)],
                &AnalysisParams::default(),
                &blastr_model::NoProgress,
                &cancel,
            )
            .expect("the variant extracts");
        assert!(variant.analysis.diff.changed_files.is_empty());
        assert!(variant.analysis.diff.changes.is_empty());
        assert!(variant.analysis.items.is_empty());
    }
}
