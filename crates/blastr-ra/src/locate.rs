//! File ids, byte ranges, line numbers and repository-relative paths.
//!
//! rust-analyzer hands out `FileId`s of a virtual file system and text ranges
//! inside them; the graph wants a [`Location`] with a path relative to the
//! repository root and 1-based line numbers.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf, Prefix};

use blastr_model::{Location, RelPath, Span};
use ra_ap_hir::{HirFileId, InFile};
use ra_ap_ide_db::base_db::SourceDatabase;
use ra_ap_ide_db::line_index::LineIndex;
use ra_ap_syntax::{TextRange, TextSize};
use ra_ap_vfs::{FileId, Vfs};

/// The directory the reported paths are relative to.
///
/// The caller and the file system may spell the same directory differently
/// (symlinks, the case of a Windows path, a verbatim prefix), so the root is
/// kept in both spellings, the canonical one read once.
#[derive(Debug, Clone)]
pub(crate) struct Root {
    /// The root as the caller spelled it, normalized.
    plain: PathBuf,
    /// The canonical spelling, normalized, when it is a different one.
    canonical: Option<PathBuf>,
}

impl Root {
    /// The root `path`, which is canonicalized once.
    pub(crate) fn new(path: &Path) -> Root {
        let plain = normalize(path);
        let canonical = std::fs::canonicalize(&plain)
            .ok()
            .map(|canonical| normalize(&canonical))
            .filter(|canonical| canonical != &plain);
        Root { plain, canonical }
    }

    /// The path of `file` relative to the root, with `/` separators, or `None`
    /// when `file` is the root itself, lies outside it or is not UTF-8.
    ///
    /// The comparison is lexical, against either spelling of the root, so the
    /// thousands of files of the standard library and the registry need no
    /// file system access.
    pub(crate) fn relative(&self, file: &Path) -> Option<RelPath> {
        let file = normalize(file);
        let below = |root: &Path| {
            let rest = file.strip_prefix(root).ok()?;
            let segments: Option<Vec<&str>> = rest
                .components()
                .map(|component| component.as_os_str().to_str())
                .collect();
            let text = segments?.join("/");
            (!text.is_empty()).then(|| RelPath::new(text))
        };
        below(&self.plain).or_else(|| below(self.canonical.as_ref()?))
    }
}

/// The repository-relative path of every file of the virtual file system that
/// lies below the root.
pub(crate) type Paths = HashMap<FileId, RelPath>;

/// The relative path of every file of `vfs` below `root`.
pub(crate) fn paths(vfs: &Vfs, root: &Root) -> Paths {
    vfs.iter()
        .filter_map(|(file, path)| Some((file, root.relative(path.as_path()?.as_ref())?)))
        .collect()
}

/// Turns rust-analyzer file ids and text ranges into [`Location`]s.
///
/// Files outside the repository root (the standard library, dependencies in the
/// Cargo registry) have no relative path and therefore no location: nothing in
/// the graph ever points at them.
#[derive(Clone, Copy)]
pub(crate) struct Locator<'a> {
    /// Where the file texts and their line indexes come from.
    db: &'a dyn SourceDatabase,
    /// The relative path of every file below the root.
    paths: &'a Paths,
}

impl<'a> Locator<'a> {
    /// A locator for `db` over `paths`.
    pub(crate) fn new(db: &'a dyn SourceDatabase, paths: &'a Paths) -> Locator<'a> {
        Locator { db, paths }
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

    /// The location of `range`, which is already known to be in the real file
    /// `file`.
    pub(crate) fn location_in_file(&self, file: FileId, range: TextRange) -> Option<Location> {
        let path = self.paths.get(&file)?.clone();
        let index = ra_ap_ide_db::line_index(self.db, file);
        let (start, end) = (range.start(), range.end());
        // `end` is one past the last byte, so the last line is the one holding
        // the last byte of the range.
        let last = end.checked_sub(TextSize::from(1)).unwrap_or(end).max(start);
        let span = Span {
            start: start.into(),
            end: end.into(),
            start_line: line(index, start),
            end_line: line(index, last),
        };
        Some(Location { file: path, span })
    }
}

/// The 1-based line holding the byte at `offset`; an offset past the end is on
/// the last line.
///
/// A byte inside a multi-byte character has no column of its own, so the start
/// of the character, which is on the same line, is asked instead.
fn line(index: &LineIndex, offset: TextSize) -> u32 {
    let offset = offset.min(index.len());
    (0..4u32)
        .find_map(|back| index.try_line_col(offset.checked_sub(TextSize::from(back))?))
        .map_or(1, |position| position.line + 1)
}

/// Removes `.`, applies `..`, drops a trailing separator and drops the
/// Windows verbatim prefix, without touching the filesystem.
///
/// `\\?\C:\dir` and `C:\dir` name the same directory, and a load hands out
/// both spellings: a canonical root has the prefix, while rust-analyzer adds
/// some of the files below it without one.
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
    use super::{LineIndex, Root, TextSize, line};
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
        let map = LineIndex::new("");
        assert_eq!(line(&map, TextSize::from(0)), 1);
    }

    #[test]
    fn lines_are_one_based() {
        let map = LineIndex::new("abc\ndef\nghi");
        assert_eq!(line(&map, TextSize::from(0)), 1);
        assert_eq!(line(&map, TextSize::from(2)), 1);
        assert_eq!(line(&map, TextSize::from(3)), 1);
        assert_eq!(line(&map, TextSize::from(4)), 2);
        assert_eq!(line(&map, TextSize::from(10)), 3);
    }

    #[test]
    fn offsets_past_the_end_stay_on_the_last_line() {
        let map = LineIndex::new("abc");
        assert_eq!(line(&map, TextSize::from(99)), 1);
    }

    #[test]
    fn a_byte_inside_a_character_is_on_its_line() {
        let map = LineIndex::new("\u{e9}\nx");
        assert_eq!(line(&map, TextSize::from(1)), 1);
        assert_eq!(line(&map, TextSize::from(3)), 2);
    }

    #[test]
    fn trailing_newline_starts_an_empty_line() {
        let map = LineIndex::new("abc\n");
        assert_eq!(line(&map, TextSize::from(3)), 1);
        assert_eq!(line(&map, TextSize::from(4)), 2);
    }

    #[test]
    fn relative_path_uses_slashes() {
        let root = joined(&["repo"]);
        let file = joined(&["repo", "src", "lib.rs"]);
        assert_eq!(
            Root::new(&root)
                .relative(&file)
                .map(|it| it.as_str().to_owned()),
            Some("src/lib.rs".to_owned())
        );
    }

    #[test]
    fn relative_path_ignores_dot_segments() {
        let root = joined(&["repo"]);
        let file = joined(&["repo", ".", "src", "..", "src", "main.rs"]);
        assert_eq!(
            Root::new(&root)
                .relative(&file)
                .map(|it| it.as_str().to_owned()),
            Some("src/main.rs".to_owned())
        );
    }

    #[test]
    fn files_outside_the_root_have_no_path() {
        let root = joined(&["repo"]);
        let outside = joined(&["elsewhere", "lib.rs"]);
        assert_eq!(Root::new(&root).relative(&outside), None);
    }

    #[test]
    fn the_root_itself_has_no_path() {
        let root = joined(&["repo"]);
        assert_eq!(Root::new(&root).relative(&root), None);
    }

    #[test]
    fn rooted_absolute_paths_work() {
        let root = Path::new("/a/b");
        let file = Path::new("/a/b/c.rs");
        // Only the lexical comparison is exercised here; `Path` never touches
        // the filesystem, so this holds on every platform.
        assert_eq!(
            Root::new(root)
                .relative(file)
                .map(|it| it.as_str().to_owned()),
            Some("c.rs".to_owned())
        );
    }

    // Windows only: `\\?\` and `C:` are path prefixes only on Windows. Elsewhere they parse as a
    // single file name with backslashes in it, and the mixed spellings this guards against never occur.
    #[cfg(windows)]
    #[test]
    fn a_verbatim_root_and_a_plain_file_are_the_same_directory() {
        // Windows canonical paths carry a `\\?\` prefix that the paths Cargo
        // reports for the files below them do not. A load hands out both
        // spellings, and a file of a crate has to be found below the root
        // either way.
        let root = Path::new(r"\\?\C:\work\project");
        let file = Path::new(r"C:\work\project\crate\src\lib.rs");
        assert_eq!(
            Root::new(root)
                .relative(file)
                .map(|it| it.as_str().to_owned()),
            Some("crate/src/lib.rs".to_owned())
        );
        let plain_root = Path::new(r"C:\work\project");
        let verbatim_file = Path::new(r"\\?\C:\work\project\crate\src\lib.rs");
        assert_eq!(
            Root::new(plain_root)
                .relative(verbatim_file)
                .map(|it| it.as_str().to_owned()),
            Some("crate/src/lib.rs".to_owned())
        );
    }
}
