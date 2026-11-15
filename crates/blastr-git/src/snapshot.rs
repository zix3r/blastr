//! Writing the content of a side into a directory.

use std::path::{Path, PathBuf};

use blastr_model::{RelPath, Source};
use gix::index::entry::{Mode, Stage};

use crate::repo::{blob_bytes, find_tree, is_file, rel_path};
use crate::{Error, Repo, SNAPSHOT_MARKER};

impl Repo {
    /// A directory holding the content of `source`.
    ///
    /// Trees and the index are written into `snapshots_dir` once and carry a
    /// `.blastr-complete` marker; a directory without it is written again. The
    /// working tree is the repository root itself, and a plain directory is
    /// returned unchanged.
    pub fn materialize(&self, source: &Source, snapshots_dir: &Path) -> Result<PathBuf, Error> {
        match source {
            Source::Tree { id, .. } => self.materialize_tree(id, snapshots_dir),
            Source::Index => self.materialize_index(snapshots_dir),
            Source::Worktree => Ok(self.root().to_path_buf()),
            Source::Dir(path) => Ok(path.clone()),
        }
    }

    /// Writes the files of a tree into `snapshots_dir`, once.
    fn materialize_tree(&self, id: &str, snapshots_dir: &Path) -> Result<PathBuf, Error> {
        let destination = snapshots_dir.join(id);
        if is_complete(&destination) {
            return Ok(destination);
        }
        let repo = self.repo()?;
        let tree = find_tree(&repo, id)?;
        let mut files = Vec::new();
        for record in tree.traverse().breadthfirst.files().map_err(Error::git)? {
            if !is_file(record.mode.kind()) {
                continue;
            }
            let content = blob_bytes(&repo, record.oid)?;
            files.push((rel_path(record.filepath), content));
        }
        write_snapshot(snapshots_dir, &destination, files)?;
        Ok(destination)
    }

    /// Writes the files of the index into `snapshots_dir`, once.
    fn materialize_index(&self, snapshots_dir: &Path) -> Result<PathBuf, Error> {
        let content_id = self.uncommitted_content_id(&Source::Index)?;
        let destination = snapshots_dir.join(format!("index-{content_id}"));
        if is_complete(&destination) {
            return Ok(destination);
        }
        let repo = self.repo()?;
        let index = repo.index_or_empty().map_err(Error::git)?;
        let state: &gix::index::State = &index;
        let mut files = Vec::new();
        for entry in state.entries() {
            if entry.stage() != Stage::Unconflicted {
                continue;
            }
            if entry.mode != Mode::FILE && entry.mode != Mode::FILE_EXECUTABLE {
                continue;
            }
            let path = rel_path(entry.path(state));
            let content = blob_bytes(&repo, entry.id)?;
            files.push((path, content));
        }
        write_snapshot(snapshots_dir, &destination, files)?;
        Ok(destination)
    }
}

/// True when `dir` holds a snapshot that was written completely.
fn is_complete(dir: &Path) -> bool {
    dir.join(SNAPSHOT_MARKER).is_file()
}

/// Writes `files` into `destination` through a temporary directory that is renamed into place.
fn write_snapshot(
    snapshots_dir: &Path,
    destination: &Path,
    files: Vec<(RelPath, Vec<u8>)>,
) -> Result<(), Error> {
    std::fs::create_dir_all(snapshots_dir)?;
    if destination.exists() {
        if is_complete(destination) {
            return Ok(());
        }
        std::fs::remove_dir_all(destination)?;
    }
    let temporary = create_temporary_dir(snapshots_dir)?;
    let written = write_files(&temporary, &files);
    let result = written.and_then(|()| std::fs::rename(&temporary, destination).map_err(Error::Io));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temporary);
    }
    result
}

/// Writes the files and the marker into the temporary snapshot directory.
fn write_files(directory: &Path, files: &[(RelPath, Vec<u8>)]) -> Result<(), Error> {
    for (path, content) in files {
        let target = directory.join(path.as_str());
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, content)?;
    }
    std::fs::write(directory.join(SNAPSHOT_MARKER), b"")?;
    Ok(())
}

/// Reserves a fresh directory inside `parent`.
fn create_temporary_dir(parent: &Path) -> Result<PathBuf, Error> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0u128, |elapsed| elapsed.as_nanos());
    for attempt in 0..1000u32 {
        let name = format!(".blastr-tmp-{}-{nanos}-{attempt}", std::process::id());
        let candidate = parent.join(name);
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(Error::Io(error)),
        }
    }
    Err(Error::Git(
        "could not create a temporary snapshot directory".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use blastr_model::Source;

    use crate::SNAPSHOT_MARKER;
    use crate::test_support::TestRepo;

    #[test]
    fn writes_the_files_of_a_tree_and_returns_the_directory() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one"), ("dir/b.txt", "two")], "first");
        let snapshots = tempfile::tempdir().expect("a temporary directory");
        let repo = test.open();

        let directory = repo
            .materialize(&test.source(&first), snapshots.path())
            .expect("a snapshot");

        assert_eq!(directory, snapshots.path().join(test.tree_id(&first)));
        assert_eq!(
            std::fs::read_to_string(directory.join("a.txt")).expect("the file"),
            "one"
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("dir").join("b.txt")).expect("the file"),
            "two"
        );
        assert!(directory.join(SNAPSHOT_MARKER).is_file());
    }

    #[test]
    fn a_complete_snapshot_is_not_written_again() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "first");
        let snapshots = tempfile::tempdir().expect("a temporary directory");
        let repo = test.open();
        let source = test.source(&first);
        let directory = repo
            .materialize(&source, snapshots.path())
            .expect("a snapshot");
        std::fs::write(directory.join("a.txt"), "edited").expect("the file");

        let again = repo
            .materialize(&source, snapshots.path())
            .expect("the same snapshot");

        assert_eq!(again, directory);
        assert_eq!(
            std::fs::read_to_string(directory.join("a.txt")).expect("the file"),
            "edited"
        );
    }

    #[test]
    fn a_snapshot_without_the_marker_is_written_again() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one"), ("b.txt", "two")], "first");
        let snapshots = tempfile::tempdir().expect("a temporary directory");
        let repo = test.open();
        let source = test.source(&first);
        let directory = repo
            .materialize(&source, snapshots.path())
            .expect("a snapshot");
        std::fs::remove_file(directory.join(SNAPSHOT_MARKER)).expect("the marker");
        std::fs::remove_file(directory.join("a.txt")).expect("the file");
        std::fs::write(directory.join("left-over.txt"), "stale").expect("the file");

        let again = repo
            .materialize(&source, snapshots.path())
            .expect("a snapshot");

        assert_eq!(again, directory);
        assert_eq!(
            std::fs::read_to_string(directory.join("a.txt")).expect("the file"),
            "one"
        );
        assert!(!directory.join("left-over.txt").exists());
        assert!(directory.join(SNAPSHOT_MARKER).is_file());
    }

    #[test]
    fn writes_the_files_of_the_index() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        test.write_index(&[("a.txt", "one!"), ("b.txt", "two")]);
        let snapshots = tempfile::tempdir().expect("a temporary directory");
        let repo = test.open();

        let directory = repo
            .materialize(&Source::Index, snapshots.path())
            .expect("a snapshot");

        assert_eq!(
            std::fs::read_to_string(directory.join("a.txt")).expect("the file"),
            "one!"
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("b.txt")).expect("the file"),
            "two"
        );
        assert!(directory.join(SNAPSHOT_MARKER).is_file());
    }

    #[test]
    fn the_working_tree_is_the_repository_root() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let snapshots = tempfile::tempdir().expect("a temporary directory");
        let repo = test.open();

        let directory = repo
            .materialize(&Source::Worktree, snapshots.path())
            .expect("a directory");

        assert_eq!(directory, repo.root());
        assert_eq!(
            std::fs::read(directory.join("a.txt")).expect("the file"),
            &b"one"[..]
        );
    }

    #[test]
    fn a_plain_directory_is_returned_unchanged() {
        let test = TestRepo::new();
        let directory = tempfile::tempdir().expect("a temporary directory");
        let snapshots = tempfile::tempdir().expect("a temporary directory");

        let found = test
            .open()
            .materialize(
                &Source::Dir(directory.path().to_path_buf()),
                snapshots.path(),
            )
            .expect("a directory");

        assert_eq!(found, directory.path());
        assert_eq!(
            std::fs::read_dir(&found).expect("a directory").count(),
            0,
            "nothing is written for a plain directory"
        );
    }
}
