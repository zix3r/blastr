//! File ids, byte ranges, line numbers and repository-relative paths.
//!
//! rust-analyzer hands out `FileId`s of a virtual file system and text ranges
//! inside them; the graph wants a [`Location`] with a path relative to the
//! repository root and 1-based line numbers.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::{Arc, Mutex};

use blastr_model::{Location, RelPath, Span};
use ra_ap_hir::{HirFileId, InFile};
use ra_ap_ide_db::base_db::SourceDatabase;
use ra_ap_syntax::TextRange;
use ra_ap_vfs::{FileId, Vfs};

/// The line structure of one file's text, so that offsets can be turned into
/// 1-based line numbers without scanning the text again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineMap {
    /// Byte offset of the first character of every line, ascending.
    starts: Vec<u32>,
}

impl LineMap {
    /// Indexes the line starts of `text`.
    pub(crate) fn new(text: &str) -> LineMap {
        let mut starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset as u32 + 1);
            }
        }
        LineMap { starts }
    }

    /// The 1-based line containing the byte `offset`.
    pub(crate) fn line(&self, offset: u32) -> u32 {
        // Every line start at or before the offset adds one line.
        self.starts.partition_point(|&start| start <= offset) as u32
    }
}

/// Turns rust-analyzer file ids and text ranges into [`Location`]s.
///
/// Files outside the repository root (the standard library, dependencies in the
/// Cargo registry) have no relative path and therefore no location: nothing in
/// the graph ever points at them.
///
/// One locator serves every worker of an extraction, so the line maps it builds
/// are shared behind a lock: a worker looks up the map of a file once and keeps
/// its own reference to it.
pub(crate) struct Locator<'a> {
    /// Where the file texts come from.
    db: &'a dyn SourceDatabase,
    /// The repository-relative path of every file in the virtual file system,
    /// shared with the locators of the workers.
    paths: Arc<Paths>,
    /// The line maps of the files that were asked about, built on first use.
    lines: Mutex<HashMap<FileId, Arc<LineMap>>>,
}

/// The repository-relative path of every file of the virtual file system.
pub(crate) type Paths = HashMap<FileId, Option<RelPath>>;

impl<'a> Locator<'a> {
    /// Records the relative path of every file of `vfs`.
    pub(crate) fn new(db: &'a dyn SourceDatabase, vfs: &Vfs, root: &Path) -> Locator<'a> {
        let root = normalize(root);
        // When the root is written differently from how the file system spells
        // it, the files have to be canonicalized too. When it is not, a lexical
        // comparison decides, which is exact and needs no file system access for
        // the thousands of files of the standard library and the registry.
        let canonical_root = std::fs::canonicalize(&root)
            .ok()
            .filter(|canonical| canonical != &root);
        let mut paths = HashMap::new();
        for (file, path) in vfs.iter() {
            let relative = path.as_path().and_then(|absolute| {
                let absolute = normalize(absolute.as_ref());
                relative_to(&root, &absolute).or_else(|| {
                    let canonical_root = canonical_root.as_ref()?;
                    let canonical = std::fs::canonicalize(&absolute).ok()?;
                    relative_to(canonical_root, &canonical)
                })
            });
            paths.insert(file, relative);
        }
        Locator {
            db,
            paths: Arc::new(paths),
            lines: Mutex::new(HashMap::new()),
        }
    }

    /// The paths of every file, for building the locator of a worker thread.
    pub(crate) fn paths(&self) -> Arc<Paths> {
        Arc::clone(&self.paths)
    }

    /// A locator for `db` over `paths`, which it shares with other locators.
    pub(crate) fn with_paths(db: &'a dyn SourceDatabase, paths: Arc<Paths>) -> Locator<'a> {
        Locator {
            db,
            paths,
            lines: Mutex::new(HashMap::new()),
        }
    }

    /// The location of `range` in `file`, which may be a macro expansion: it is
    /// mapped to the range of the macro call or derive attribute that produced
    /// it, and to the range of the whole call when a partial mapping is
    /// impossible.
    pub(crate) fn location(&self, file: HirFileId, range: TextRange) -> Option<Location> {
        let rooted = InFile::new(file, range).original_node_file_range_rooted(self.db);
        let rooted = rooted.into_file_id(self.db);
        self.location_in_file(rooted.file_id, rooted.range)
    }

    /// The relative path of `file`, or `None` when it lies outside the root or
    /// is a macro expansion.
    pub(crate) fn file_path(&self, file: HirFileId) -> Option<RelPath> {
        let file = file.file_id()?.file_id(self.db);
        self.paths.get(&file)?.clone()
    }

    /// The location of `range`, which is already known to be in the real file
    /// `file`.
    pub(crate) fn location_in_file(&self, file: FileId, range: TextRange) -> Option<Location> {
        let path = self.paths.get(&file)?.clone()?;
        let span = self.span(file, range)?;
        Some(Location { file: path, span })
    }

    fn span(&self, file: FileId, range: TextRange) -> Option<Span> {
        let lines = self.line_map(file);
        let start = u32::from(range.start());
        let end = u32::from(range.end());
        let start_line = lines.line(start);
        // `end` is one past the last byte, so the last line is the one holding
        // the last byte of the range.
        let end_line = lines.line(end.saturating_sub(1).max(start));
        Some(Span {
            start,
            end,
            start_line,
            end_line,
        })
    }

    fn line_map(&self, file: FileId) -> Arc<LineMap> {
        let mut lines = self.lines.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(lines) = lines.get(&file) {
            return Arc::clone(lines);
        }
        let text = self.db.file_text(file);
        let map = Arc::new(LineMap::new(text.text(self.db)));
        lines.insert(file, Arc::clone(&map));
        map
    }
}

/// The path of `file` relative to `root`, with `/` separators, or `None` when
/// `file` is outside `root` or not UTF-8.
pub(crate) fn rel_path(root: &Path, file: &Path) -> Option<RelPath> {
    let root = normalize(root);
    let file = normalize(file);
    if let Some(relative) = relative_to(&root, &file) {
        return Some(relative);
    }
    // The two paths can spell the same directory differently: symlinks, `..`,
    // or shortened Windows names. Cargo metadata and a repository root found by
    // walking upwards are usually fine, but be forgiving when they are not.
    let canonical_root = std::fs::canonicalize(&root).ok()?;
    let canonical_file = std::fs::canonicalize(&file).ok()?;
    relative_to(&canonical_root, &canonical_file)
}

/// The path of `file` relative to `root` decided by a plain comparison of the
/// two, without touching the file system.
pub(crate) fn lexical_rel_path(root: &Path, file: &Path) -> Option<RelPath> {
    relative_to(&normalize(root), &normalize(file))
}

/// The components of `file` below `root`, with `/` separators.
fn relative_to(root: &Path, file: &Path) -> Option<RelPath> {
    let rest = file.strip_prefix(root).ok()?;
    let mut text = String::new();
    for component in rest.components() {
        let segment = component.as_os_str().to_str()?;
        if !text.is_empty() {
            text.push('/');
        }
        text.push_str(segment);
    }
    if text.is_empty() {
        return None;
    }
    Some(RelPath::new(text))
}

/// Removes `.`, applies `..`, drops a trailing separator and drops the
/// Windows verbatim prefix, without touching the filesystem.
///
/// `\\?\C:\dir` and `C:\dir` name the same directory, and a load hands out
/// both spellings: [`Locator::new`] is given a canonical root, which has the
/// prefix, while rust-analyzer adds some of the files below it without one.
/// Comparing the two as text would put those files outside the root, so the
/// prefix is removed from both sides and they compare equal.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Prefix(prefix) => match prefix.kind() {
                // `\\?\C:\dir` is `C:\dir`; the drive letter replaces the
                // prefix, so the rest of the path still appends below it.
                Prefix::VerbatimDisk(letter) => out.push(format!("{}:\\", letter as char)),
                // `\\?\UNC\server\share` is `\\server\share`.
                Prefix::VerbatimUNC(server, share) => out.push(format!(
                    "\\\\{}\\{}",
                    server.to_string_lossy(),
                    share.to_string_lossy()
                )),
                _ => out.push(component.as_os_str()),
            },
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{LineMap, lexical_rel_path, rel_path};
    use std::path::{Path, PathBuf};

    fn joined(parts: &[&str]) -> PathBuf {
        let mut path = PathBuf::new();
        for part in parts {
            path.push(part);
        }
        path
    }

    #[test]
    fn empty_text_has_one_line() {
        let map = LineMap::new("");
        assert_eq!(map.line(0), 1);
    }

    #[test]
    fn lines_are_one_based() {
        let map = LineMap::new("abc\ndef\nghi");
        assert_eq!(map.line(0), 1);
        assert_eq!(map.line(2), 1);
        assert_eq!(map.line(3), 1);
        assert_eq!(map.line(4), 2);
        assert_eq!(map.line(10), 3);
    }

    #[test]
    fn offsets_past_the_end_stay_on_the_last_line() {
        let map = LineMap::new("abc");
        assert_eq!(map.line(99), 1);
    }

    #[test]
    fn trailing_newline_starts_an_empty_line() {
        let map = LineMap::new("abc\n");
        assert_eq!(map.line(3), 1);
        assert_eq!(map.line(4), 2);
    }

    #[test]
    fn relative_path_uses_slashes() {
        let root = joined(&["repo"]);
        let file = joined(&["repo", "src", "lib.rs"]);
        assert_eq!(
            rel_path(&root, &file).map(|it| it.as_str().to_owned()),
            Some("src/lib.rs".to_owned())
        );
    }

    #[test]
    fn relative_path_ignores_dot_segments() {
        let root = joined(&["repo"]);
        let file = joined(&["repo", ".", "src", "..", "src", "main.rs"]);
        assert_eq!(
            rel_path(&root, &file).map(|it| it.as_str().to_owned()),
            Some("src/main.rs".to_owned())
        );
    }

    #[test]
    fn files_outside_the_root_have_no_path() {
        let root = joined(&["repo"]);
        let outside = joined(&["elsewhere", "lib.rs"]);
        assert_eq!(rel_path(&root, &outside), None);
    }

    #[test]
    fn the_root_itself_has_no_path() {
        let root = joined(&["repo"]);
        assert_eq!(rel_path(&root, &root), None);
    }

    #[test]
    fn rooted_absolute_paths_work() {
        let root = Path::new("/a/b");
        let file = Path::new("/a/b/c.rs");
        // Only the lexical comparison is exercised here; `Path` never touches
        // the filesystem, so this holds on every platform.
        assert_eq!(
            rel_path(root, file).map(|it| it.as_str().to_owned()),
            Some("c.rs".to_owned())
        );
    }

    #[test]
    fn a_verbatim_root_and_a_plain_file_are_the_same_directory() {
        // Windows canonical paths carry a `\\?\` prefix that the paths Cargo
        // reports for the files below them do not. A load hands out both
        // spellings, and a file of a crate has to be found below the root
        // either way.
        let root = Path::new(r"\\?\C:\work\project");
        let file = Path::new(r"C:\work\project\crate\src\lib.rs");
        assert_eq!(
            lexical_rel_path(root, file).map(|it| it.as_str().to_owned()),
            Some("crate/src/lib.rs".to_owned())
        );
        let plain_root = Path::new(r"C:\work\project");
        let verbatim_file = Path::new(r"\\?\C:\work\project\crate\src\lib.rs");
        assert_eq!(
            lexical_rel_path(plain_root, verbatim_file).map(|it| it.as_str().to_owned()),
            Some("crate/src/lib.rs".to_owned())
        );
    }
}
