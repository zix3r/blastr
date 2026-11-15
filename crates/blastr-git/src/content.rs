//! Reading file content and identifying the content of a side.

use std::path::Path;

use blastr_model::{Comparison, FileStatus, RelPath, Source};
use gix::index::entry::Stage;

use crate::repo::{blob_bytes, find_tree, head_tree_source, is_file};
use crate::{Error, Repo};

impl Repo {
    /// Content of one file on one side; `None` if it does not exist there.
    pub fn read_file(&self, source: &Source, path: &RelPath) -> Result<Option<Vec<u8>>, Error> {
        match source {
            Source::Tree { id, .. } => {
                let repo = self.repo()?;
                let tree = find_tree(&repo, id)?;
                let entry = tree
                    .lookup_entry_by_path(Path::new(path.as_str()))
                    .map_err(Error::git)?;
                let Some(entry) = entry else {
                    return Ok(None);
                };
                if !is_file(entry.mode().kind()) {
                    return Ok(None);
                }
                let object = entry.object().map_err(Error::git)?;
                let mut blob = object.try_into_blob().map_err(Error::git)?;
                Ok(Some(blob.take_data()))
            }
            Source::Index => {
                let repo = self.repo()?;
                let index = repo.index_or_empty().map_err(Error::git)?;
                let state: &gix::index::State = &index;
                let entry =
                    state.entry_by_path_and_stage(path.as_str().into(), Stage::Unconflicted);
                let Some(entry) = entry else {
                    return Ok(None);
                };
                let id = entry.id;
                Ok(Some(blob_bytes(&repo, id)?))
            }
            Source::Worktree => read_existing(&self.root().join(path.as_str())),
            Source::Dir(dir) => read_existing(&dir.join(path.as_str())),
        }
    }

    /// An identifier that changes whenever the content of `source` changes.
    ///
    /// This is the tree id for a tree, a BLAKE3 hash over the `HEAD` tree id and
    /// every file that differs from it for the index and the working tree, and
    /// `None` for a plain directory.
    pub fn content_id(&self, source: &Source) -> Result<Option<String>, Error> {
        match source {
            Source::Tree { id, .. } => Ok(Some(id.clone())),
            Source::Index | Source::Worktree => Ok(Some(self.uncommitted_content_id(source)?)),
            Source::Dir(_) => Ok(None),
        }
    }

    /// BLAKE3 over the `HEAD` tree id and the content of every file that differs from it.
    ///
    /// Paths are fed in sorted order, so the result is deterministic.
    pub(crate) fn uncommitted_content_id(&self, source: &Source) -> Result<String, Error> {
        let repo = self.repo()?;
        let base = head_tree_source(&repo)?;
        let mut hasher = blake3::Hasher::new();
        if let Source::Tree { id, .. } = &base {
            hasher.update(id.as_bytes());
            hasher.update(b"\0");
        }
        let comparison = Comparison {
            base,
            head: source.clone(),
        };
        for file in self.changed_files(&comparison)? {
            hasher.update(status_name(file.status).as_bytes());
            hasher.update(b"\0");
            hasher.update(file.path.as_str().as_bytes());
            hasher.update(b"\0");
            if let Some(old) = &file.old_path {
                hasher.update(old.as_str().as_bytes());
                hasher.update(b"\0");
            }
            match self.read_file(source, &file.path)? {
                Some(bytes) => hasher.update(&bytes),
                None => hasher.update(b"-"),
            };
            hasher.update(b"\0");
        }
        Ok(hasher.finalize().to_hex().to_string())
    }
}

/// Reads a file if it exists; directories and missing paths give `None`.
fn read_existing(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(std::fs::read(path)?))
}

/// The stable name of a file status.
fn status_name(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "added",
        FileStatus::Modified => "modified",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
    }
}

#[cfg(test)]
mod tests {
    use blastr_model::{RelPath, Source};

    use crate::test_support::TestRepo;

    #[test]
    fn reads_a_file_of_a_tree() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one"), ("dir/b.txt", "two")], "first");
        let repo = test.open();
        let source = test.source(&first);

        let a = repo
            .read_file(&source, &RelPath::new("a.txt"))
            .expect("content");
        let b = repo
            .read_file(&source, &RelPath::new("dir/b.txt"))
            .expect("content");
        let missing = repo
            .read_file(&source, &RelPath::new("nope.txt"))
            .expect("content");

        assert_eq!(a.as_deref(), Some(&b"one"[..]));
        assert_eq!(b.as_deref(), Some(&b"two"[..]));
        assert_eq!(missing, None);
    }

    #[test]
    fn reads_a_file_of_the_working_tree() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let repo = test.open();

        let a = repo
            .read_file(&Source::Worktree, &RelPath::new("a.txt"))
            .expect("content");
        let missing = repo
            .read_file(&Source::Worktree, &RelPath::new("nope.txt"))
            .expect("content");

        assert_eq!(a.as_deref(), Some(&b"one"[..]));
        assert_eq!(missing, None);
    }

    #[test]
    fn the_content_id_of_a_tree_is_its_id() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "first");
        let repo = test.open();

        let id = repo.content_id(&test.source(&first)).expect("a content id");

        assert_eq!(id, Some(test.tree_id(&first)));
    }

    #[test]
    fn the_content_id_of_a_directory_is_unknown() {
        let test = TestRepo::new();
        let repo = test.open();

        let id = repo
            .content_id(&Source::Dir(test.path().to_path_buf()))
            .expect("no content id");

        assert_eq!(id, None);
    }

    #[test]
    fn the_content_id_of_the_working_tree_is_stable_and_follows_the_content() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let repo = test.open();

        let before = repo.content_id(&Source::Worktree).expect("a content id");
        let again = repo.content_id(&Source::Worktree).expect("a content id");

        test.write("a.txt", "changed");
        let after = repo.content_id(&Source::Worktree).expect("a content id");

        assert!(before.is_some());
        assert_eq!(before, again);
        assert_ne!(before, after);
    }

    #[test]
    fn the_content_id_of_the_index_follows_the_staged_content() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let repo = test.open();
        let before = repo.content_id(&Source::Index).expect("a content id");

        test.write_index(&[("a.txt", "one!"), ("b.txt", "two")]);
        let after = repo.content_id(&Source::Index).expect("a content id");

        assert!(before.is_some());
        assert_ne!(before, after);
    }
}
