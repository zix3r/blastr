//! File-level differences between the sides of a comparison.

use std::collections::BTreeMap;
use std::path::Path;

use blastr_model::{ChangedFile, Comparison, FileStatus, RelPath, Source};
use gix::index::entry::{Mode, Stage};
use gix::object::tree::diff::ChangeDetached;
use gix::objs::tree::EntryKind;

use crate::repo::{blob_bytes, find_tree, is_file, rel_path};
use crate::{Error, Repo};

/// Files that differ between two plain directories, sorted by path.
///
/// Directories named `target` or `.git` are skipped at any depth, symbolic
/// links are ignored and no rename detection is performed.
pub fn changed_files_between_dirs(base: &Path, head: &Path) -> Result<Vec<ChangedFile>, Error> {
    let base_files = dir_entries(base)?;
    let head_files = dir_entries(head)?;
    let mut out = Vec::new();
    for (path, content) in &base_files {
        match head_files.get(path) {
            None => out.push(changed(path, FileStatus::Deleted)),
            Some(other) if other != content => out.push(changed(path, FileStatus::Modified)),
            Some(_) => {}
        }
    }
    for path in head_files.keys() {
        if !base_files.contains_key(path) {
            out.push(changed(path, FileStatus::Added));
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// A change that involves a single path.
fn changed(path: &RelPath, status: FileStatus) -> ChangedFile {
    ChangedFile {
        path: path.clone(),
        old_path: None,
        status,
    }
}

/// How one side identifies the content of a file.
enum Content {
    /// An object id, as a tree or the index records it.
    Object(gix::ObjectId),
    /// Raw bytes, as a plain directory has them.
    Bytes(Vec<u8>),
}

/// One file on one side of a comparison.
struct FileEntry {
    /// Identifies the content.
    content: Content,
    /// The kind Git records for it, when that side records one.
    kind: Option<EntryKind>,
}

impl Repo {
    /// Files that differ between the two sides of `comparison`, sorted by path.
    ///
    /// Renames are detected between two trees. Untracked files in the working
    /// tree that are not ignored count as added, and nothing below `.git` is
    /// ever reported.
    pub fn changed_files(&self, comparison: &Comparison) -> Result<Vec<ChangedFile>, Error> {
        let repo = self.repo()?;
        if let (Source::Tree { id: base, .. }, Source::Tree { id: head, .. }) =
            (&comparison.base, &comparison.head)
        {
            return tree_changes(&repo, base, head);
        }
        let base = entries_of(&repo, &comparison.base)?;
        let head = entries_of(&repo, &comparison.head)?;
        let mut out = Vec::new();
        for (path, entry) in &head {
            match base.get(path) {
                None => out.push(changed(path, FileStatus::Added)),
                Some(old) => {
                    if !same_content(&repo, old, entry)? {
                        out.push(changed(path, FileStatus::Modified));
                    }
                }
            }
        }
        for path in base.keys() {
            if !head.contains_key(path) {
                out.push(changed(path, FileStatus::Deleted));
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }
}

/// True when two entries hold the same content and the same kind.
fn same_content(repo: &gix::Repository, base: &FileEntry, head: &FileEntry) -> Result<bool, Error> {
    if let (Some(base_kind), Some(head_kind)) = (base.kind, head.kind)
        && base_kind != head_kind
    {
        return Ok(false);
    }
    match (&base.content, &head.content) {
        (Content::Object(base), Content::Object(head)) => Ok(base == head),
        (Content::Bytes(base), Content::Bytes(head)) => Ok(base == head),
        (Content::Object(base), Content::Bytes(head)) => {
            let bytes = blob_bytes(repo, *base)?;
            Ok(same_text(&bytes, head))
        }
        (Content::Bytes(base), Content::Object(head)) => {
            let bytes = blob_bytes(repo, *head)?;
            Ok(same_text(base, &bytes))
        }
    }
}

/// True when two files are equal once every CRLF line ending is read as LF.
///
/// Git checks files out with the platform's line endings when asked to, and
/// does not count that as a modification; neither does this comparison.
fn same_text(a: &[u8], b: &[u8]) -> bool {
    a == b || unix_lines(a).eq(unix_lines(b))
}

/// The bytes of a file, without the carriage return of each CRLF pair.
fn unix_lines(bytes: &[u8]) -> impl Iterator<Item = u8> + '_ {
    bytes
        .iter()
        .enumerate()
        .filter(|&(index, &byte)| byte != b'\r' || bytes.get(index + 1) != Some(&b'\n'))
        .map(|(_, &byte)| byte)
}

/// The files of one side, by path.
fn entries_of(
    repo: &gix::Repository,
    source: &Source,
) -> Result<BTreeMap<RelPath, FileEntry>, Error> {
    match source {
        Source::Tree { id, .. } => tree_entries(repo, id),
        Source::Index => index_entries(repo),
        Source::Worktree => worktree_entries(repo),
        Source::Dir(path) => dir_entries(path).map(|files| {
            files
                .into_iter()
                .map(|(path, bytes)| {
                    let entry = FileEntry {
                        content: Content::Bytes(bytes),
                        kind: None,
                    };
                    (path, entry)
                })
                .collect()
        }),
    }
}

/// The files a tree holds, by path.
fn tree_entries(repo: &gix::Repository, id: &str) -> Result<BTreeMap<RelPath, FileEntry>, Error> {
    let tree = find_tree(repo, id)?;
    let mut out = BTreeMap::new();
    for record in tree.traverse().breadthfirst.files().map_err(Error::git)? {
        let kind = record.mode.kind();
        if !is_file(kind) {
            continue;
        }
        let entry = FileEntry {
            content: Content::Object(record.oid),
            kind: Some(kind),
        };
        out.insert(rel_path(record.filepath), entry);
    }
    Ok(out)
}

/// The files the index holds, by path.
fn index_entries(repo: &gix::Repository) -> Result<BTreeMap<RelPath, FileEntry>, Error> {
    let index = repo.index_or_empty().map_err(Error::git)?;
    let state: &gix::index::State = &index;
    let mut out = BTreeMap::new();
    for entry in state.entries() {
        if entry.stage() != Stage::Unconflicted {
            continue;
        }
        let kind = if entry.mode == Mode::FILE {
            EntryKind::Blob
        } else if entry.mode == Mode::FILE_EXECUTABLE {
            EntryKind::BlobExecutable
        } else {
            continue;
        };
        let path = rel_path(entry.path(state));
        let file = FileEntry {
            content: Content::Object(entry.id),
            kind: Some(kind),
        };
        out.insert(path, file);
    }
    Ok(out)
}

/// The files the working tree holds, by path.
///
/// Files that Git ignores are left out.
fn worktree_entries(repo: &gix::Repository) -> Result<BTreeMap<RelPath, FileEntry>, Error> {
    let mut out = BTreeMap::new();
    let hash = repo.object_hash();
    walk_worktree(repo, &mut |path, full, _| {
        let bytes = std::fs::read(full)?;
        // A file with carriage returns may differ from the committed one
        // in line endings alone, which only a look at the text can tell.
        let content = if bytes.contains(&b'\r') {
            Content::Bytes(bytes)
        } else {
            Content::Object(
                gix::objs::compute_hash(hash, gix::objs::Kind::Blob, &bytes).map_err(Error::git)?,
            )
        };
        let file = FileEntry {
            content,
            kind: None,
        };
        out.insert(RelPath::new(path), file);
        Ok(())
    })?;
    Ok(out)
}

/// How recently a file must have been written for its stat to be distrusted.
const RACY: std::time::Duration = std::time::Duration::from_secs(2);

/// A cheap fingerprint of the working tree: the commit `HEAD` names, the index
/// file, and the path, size and modification time of every file Git does not
/// ignore. No file is read, so it is fast enough to ask several times a second;
/// when it changes, the content may have changed.
pub(crate) fn worktree_stamp(repo: &gix::Repository) -> Result<String, Error> {
    let mut hasher = blake3::Hasher::new();
    if let Ok(id) = repo.head_id() {
        hasher.update(id.detach().as_bytes());
    }
    hasher.update(b"\0");
    let stat = |hasher: &mut blake3::Hasher, metadata: &std::fs::Metadata| {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |elapsed| elapsed.as_nanos());
        hasher.update(&metadata.len().to_le_bytes());
        hasher.update(&modified.to_le_bytes());
    };
    if let Ok(metadata) = std::fs::metadata(repo.index_path()) {
        stat(&mut hasher, &metadata);
    }
    let now = std::time::SystemTime::now();
    let mut racy = false;
    walk_worktree(repo, &mut |path, _, metadata| {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        stat(&mut hasher, metadata);
        // A file written in the last moments may be written again within the
        // same tick of the clock, without its time or size moving.
        racy |= metadata
            .modified()
            .is_ok_and(|time| now.duration_since(time).map_or(true, |age| age < RACY));
        Ok(())
    })?;
    if racy {
        // A stamp that never repeats sends the caller to the content.
        let nanos = now
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        hasher.update(&nanos.to_le_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Calls `on_file` with the path, full path and metadata of every file in the
/// working tree that Git does not ignore, in path order.
fn walk_worktree(
    repo: &gix::Repository,
    on_file: &mut dyn FnMut(String, &Path, &std::fs::Metadata) -> Result<(), Error>,
) -> Result<(), Error> {
    let Some(root) = repo.workdir() else {
        return Err(Error::Bare {
            path: repo.path().to_path_buf(),
        });
    };
    let index = repo.index_or_empty().map_err(Error::git)?;
    let state: &gix::index::State = &index;
    let mut excludes = repo
        .excludes(
            state,
            None,
            gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
        )
        .map_err(Error::git)?;
    collect_worktree(root, "", &mut excludes, on_file)
}

/// Walks one directory of the working tree, handing each file it holds to `on_file`.
fn collect_worktree(
    dir: &Path,
    prefix: &str,
    excludes: &mut gix::AttributeStack<'_>,
    on_file: &mut dyn FnMut(String, &Path, &std::fs::Metadata) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let full = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if is_excluded(excludes, &path, Mode::DIR)? {
                continue;
            }
            collect_worktree(&full, &path, excludes, on_file)?;
        } else if file_type.is_file() {
            if is_excluded(excludes, &path, Mode::FILE)? {
                continue;
            }
            let metadata = entry.metadata()?;
            on_file(path, &full, &metadata)?;
        }
    }
    Ok(())
}

/// True when Git ignores `path`.
fn is_excluded(
    excludes: &mut gix::AttributeStack<'_>,
    path: &str,
    mode: Mode,
) -> Result<bool, Error> {
    let platform = excludes.at_path(path, Some(mode)).map_err(Error::git)?;
    Ok(platform.is_excluded())
}

/// The changes needed to turn the tree `base` into the tree `head`, sorted by path.
fn tree_changes(repo: &gix::Repository, base: &str, head: &str) -> Result<Vec<ChangedFile>, Error> {
    let base = find_tree(repo, base)?;
    let head = find_tree(repo, head)?;
    let rewrites = gix::diff::Rewrites::default();
    let options = gix::diff::Options::default().with_rewrites(Some(rewrites));
    let changes = repo
        .diff_tree_to_tree(Some(&base), Some(&head), Some(options))
        .map_err(Error::git)?;
    let mut out = Vec::new();
    for change in changes {
        match change {
            ChangeDetached::Addition {
                location,
                entry_mode,
                ..
            } => {
                if is_file(entry_mode.kind()) {
                    out.push(changed(&rel_path(location), FileStatus::Added));
                }
            }
            ChangeDetached::Deletion {
                location,
                entry_mode,
                ..
            } => {
                if is_file(entry_mode.kind()) {
                    out.push(changed(&rel_path(location), FileStatus::Deleted));
                }
            }
            ChangeDetached::Modification {
                location,
                previous_entry_mode,
                entry_mode,
                ..
            } => {
                let status = match (
                    is_file(previous_entry_mode.kind()),
                    is_file(entry_mode.kind()),
                ) {
                    (true, true) => FileStatus::Modified,
                    (true, false) => FileStatus::Deleted,
                    (false, true) => FileStatus::Added,
                    // A directory whose content changed: the diff reports the
                    // files inside it on their own.
                    (false, false) => continue,
                };
                out.push(changed(&rel_path(location), status));
            }
            ChangeDetached::Rewrite {
                source_location,
                location,
                copy,
                ..
            } => {
                if copy {
                    out.push(changed(&rel_path(location), FileStatus::Added));
                } else {
                    let mut file = changed(&rel_path(location), FileStatus::Renamed);
                    file.old_path = Some(rel_path(source_location));
                    out.push(file);
                }
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The files of a plain directory, by path, skipping `target` and `.git`.
fn dir_entries(dir: &Path) -> Result<BTreeMap<RelPath, Vec<u8>>, Error> {
    let mut out = BTreeMap::new();
    collect_dir(dir, "", &mut out)?;
    Ok(out)
}

/// Walks one directory of a plain directory tree, collecting the files it holds.
fn collect_dir(
    dir: &Path,
    prefix: &str,
    out: &mut BTreeMap<RelPath, Vec<u8>>,
) -> Result<(), Error> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "target" || name == ".git" {
            continue;
        }
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let full = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_dir(&full, &path, out)?;
        } else if file_type.is_file() {
            out.insert(RelPath::new(path), std::fs::read(&full)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use blastr_model::{ChangedFile, Comparison, FileStatus, RelPath, Source};

    use super::same_text;
    use crate::changed_files_between_dirs;
    use crate::test_support::TestRepo;

    /// A file that is only on the head side.
    fn added(path: &str) -> ChangedFile {
        file(path, FileStatus::Added)
    }

    /// A file with different content on both sides.
    fn modified(path: &str) -> ChangedFile {
        file(path, FileStatus::Modified)
    }

    /// A file that is only on the base side.
    fn deleted(path: &str) -> ChangedFile {
        file(path, FileStatus::Deleted)
    }

    /// A file that moved from `from` to `path`.
    fn renamed(path: &str, from: &str) -> ChangedFile {
        ChangedFile {
            path: RelPath::new(path),
            old_path: Some(RelPath::new(from)),
            status: FileStatus::Renamed,
        }
    }

    /// A change without an old path.
    fn file(path: &str, status: FileStatus) -> ChangedFile {
        ChangedFile {
            path: RelPath::new(path),
            old_path: None,
            status,
        }
    }

    #[test]
    fn between_two_trees_tracks_renames() {
        let mut test = TestRepo::new();
        let first = test.commit(
            &[
                ("a.txt", "one"),
                ("b.txt", "keep"),
                ("old.txt", "moved content"),
                ("gone.txt", "bye"),
                ("sub/deep.txt", "deep"),
            ],
            "first",
        );
        let second = test.commit(
            &[
                ("a.txt", "one changed"),
                ("b.txt", "keep"),
                ("new.txt", "fresh"),
                ("renamed.txt", "moved content"),
                ("sub/deep.txt", "deeper"),
            ],
            "second",
        );
        let comparison = Comparison {
            base: test.source(&first),
            head: test.source(&second),
        };

        let files = test.open().changed_files(&comparison).expect("changes");

        assert_eq!(
            files,
            vec![
                modified("a.txt"),
                deleted("gone.txt"),
                added("new.txt"),
                renamed("renamed.txt", "old.txt"),
                modified("sub/deep.txt"),
            ]
        );
    }

    #[test]
    fn between_a_tree_and_the_index() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one"), ("b.txt", "two")], "first");
        test.write_index(&[("a.txt", "one!"), ("c.txt", "three")]);
        let comparison = Comparison {
            base: test.source(&first),
            head: Source::Index,
        };

        let files = test.open().changed_files(&comparison).expect("changes");

        assert_eq!(
            files,
            vec![modified("a.txt"), deleted("b.txt"), added("c.txt")]
        );
    }

    #[test]
    fn between_a_tree_and_the_working_tree() {
        let mut test = TestRepo::new();
        let first = test.commit(
            &[
                ("a.txt", "one"),
                ("b.txt", "two"),
                (".gitignore", "target/\n"),
            ],
            "first",
        );
        test.write("a.txt", "one!");
        test.remove("b.txt");
        test.write("c.txt", "three");
        test.write("target/garbage.txt", "ignored");
        let comparison = Comparison {
            base: test.source(&first),
            head: Source::Worktree,
        };

        let files = test.open().changed_files(&comparison).expect("changes");

        assert_eq!(
            files,
            vec![modified("a.txt"), deleted("b.txt"), added("c.txt")]
        );
    }

    #[test]
    fn line_endings_alone_are_not_a_difference() {
        assert!(same_text(b"a\nb\n", b"a\r\nb\r\n"));
        assert!(same_text(b"a\r\nb\n", b"a\nb\r\n"));
        assert!(!same_text(b"a\nb\n", b"a\nc\n"));
        assert!(!same_text(b"a\rb", b"ab"));
    }

    #[test]
    fn between_two_directories() {
        let base = tempfile::tempdir().expect("a temporary directory");
        let head = tempfile::tempdir().expect("a temporary directory");
        for (root, path, content) in [
            (base.path(), "a.txt", "one"),
            (base.path(), "b.txt", "two"),
            (base.path(), "sub/deep.txt", "deep"),
            (base.path(), "target/generated.txt", "generated"),
            (base.path(), ".git/config", "base"),
            (head.path(), "a.txt", "one!"),
            (head.path(), "b.txt", "two"),
            (head.path(), "sub/deep2.txt", "new"),
            (head.path(), "target/generated.txt", "other"),
            (head.path(), ".git/config", "head"),
        ] {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().expect("a parent")).expect("a directory");
            std::fs::write(&full, content).expect("a file");
        }

        let files =
            changed_files_between_dirs(base.path(), head.path()).expect("filesystem changes");

        assert_eq!(
            files,
            vec![
                modified("a.txt"),
                deleted("sub/deep.txt"),
                added("sub/deep2.txt"),
            ]
        );
    }
}
