//! Applying file edits to a loaded project.
//!
//! rust-analyzer's change pipeline has three parts, and all three are needed to
//! reach the state a second project load would have produced:
//!
//! - the virtual file system records the new text of every file and keeps the
//!   [`ra_ap_vfs::FileId`] a file already has, so a file that exists in both
//!   revisions keeps its id; a file it has never seen gets a fresh one,
//! - the source roots are partitioned again, so that an added file belongs to
//!   the same local source root as the rest of its package (the crate graph
//!   itself is unchanged, which is why the caller requires the two revisions to
//!   share their `Cargo.toml` and `Cargo.lock`),
//! - one [`ChangeWithProcMacros`] carries the new texts into the database;
//!   `AnalysisHost::apply_change` then invalidates everything derived from them.

use std::collections::HashMap;

use blastr_model::RelPath;
use ra_ap_hir::ChangeWithProcMacros;
use ra_ap_vfs::{Change, FileId, VfsPath};

use crate::load::absolute;
use crate::locate::lexical_rel_path;
use crate::{Error, FileEdit, Workspace};

impl Workspace {
    /// Applies edits on top of the loaded project (changed, added or removed files).
    ///
    /// After this the project has the same files and the same crate graph as
    /// before, with the given texts, so a later
    /// [`extract`](Workspace::extract) reports the edited revision. Files the
    /// project never had are only added when they are Rust sources; a changed
    /// `README.md` or an added documentation file is ignored.
    pub fn apply(&mut self, edits: &[FileEdit]) -> Result<(), Error> {
        let known = self.files_by_path();
        for edit in edits {
            let path = match known.get(&edit.path) {
                // Reusing the path the file is known under keeps its id.
                Some(file_id) => self.vfs.file_path(*file_id).clone(),
                None => {
                    if !is_rust_source(edit.path.as_str()) {
                        continue;
                    }
                    VfsPath::from(absolute(&edit.path.to_path(&self.root))?)
                }
            };
            let contents = edit.content.as_ref().map(|text| text.clone().into_bytes());
            self.vfs.set_file_contents(path, contents);
        }

        let changes = self.vfs.take_changes();
        if changes.is_empty() {
            return Ok(());
        }

        let mut change = ChangeWithProcMacros::default();
        let mut structure_changed = false;
        for (file_id, file) in changes {
            structure_changed |= file.is_created_or_deleted();
            let text = match file.change {
                Change::Create(bytes, _) | Change::Modify(bytes, _) => {
                    Some(decode_text(bytes, file_id)?)
                }
                Change::Delete => None,
            };
            change.change_file(file_id, text);
        }
        // Adding or removing a file changes which files a source root holds; the
        // roots are therefore partitioned again, which also gives the new file
        // ids a source root before their text is set. Editing the text of a file
        // that is already known leaves the roots alone, which keeps the churn
        // in the database small.
        if structure_changed {
            let roots = self.source_roots.partition(&self.vfs);
            change.set_roots(roots);
        }
        self.host.apply_change(change);
        Ok(())
    }

    /// The id of every file in the virtual file system, by path relative to the root.
    ///
    /// The comparison is lexical: the loading code and the caller both derive
    /// their paths from the same root directory, so the same file is spelled the
    /// same way in both.
    fn files_by_path(&self) -> HashMap<RelPath, FileId> {
        let mut files = HashMap::new();
        for (file_id, path) in self.vfs.iter() {
            let Some(absolute) = path.as_path() else {
                continue;
            };
            if let Some(relative) = lexical_rel_path(&self.root, absolute.as_ref()) {
                files.insert(relative, file_id);
            }
        }
        files
    }
}

/// The text of a changed file.
///
/// The bytes come from a [`String`] in [`FileEdit::content`], so decoding cannot
/// fail; the error is still reported rather than assumed away.
fn decode_text(bytes: Vec<u8>, file_id: FileId) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|error| {
        Error::Internal(format!(
            "the text of `{file_id:?}` is not valid UTF-8: {error}"
        ))
    })
}

/// True when `path` names a Rust source file.
fn is_rust_source(path: &str) -> bool {
    path.ends_with(".rs")
}

#[cfg(test)]
mod tests {
    use super::is_rust_source;

    #[test]
    fn recognizes_rust_sources() {
        assert!(is_rust_source("src/lib.rs"));
        assert!(is_rust_source("crates/a/build.rs"));
        assert!(!is_rust_source("README.md"));
        assert!(!is_rust_source("src/lib.rsi"));
        assert!(!is_rust_source("Cargo.toml"));
    }
}
