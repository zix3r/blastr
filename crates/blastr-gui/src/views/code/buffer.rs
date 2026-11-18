//! The text of a file the source view edits, and how it goes back to disk.
//!
//! Nothing here draws. This decides which line breaks a file uses, writes it
//! back with the same ones, and remembers whether the editor has moved away
//! from the text that is on disk. The last part is what keeps a save from
//! touching a file outside the repository: a path that leaves the root is
//! refused instead of resolved.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use blastr_model::RelPath;

/// The line breaks a file uses, so that saving writes back what it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Endings {
    /// Unix line breaks, `\n`.
    #[default]
    Lf,
    /// Windows line breaks, `\r\n`.
    Crlf,
}

/// The line breaks of `text`: the more common of the two, Unix when it is a
/// tie or when there is no line break at all.
pub(crate) fn detect(text: &str) -> Endings {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count().saturating_sub(crlf);
    if crlf > lf {
        Endings::Crlf
    } else {
        Endings::Lf
    }
}

/// `text` with every line break written as `\n`.
///
/// A buffer is always edited with Unix breaks, so that what the editor holds
/// does not depend on the file it came from; [`write_text`] puts the file's own
/// breaks back.
pub(crate) fn unix(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n")
    } else {
        text.to_owned()
    }
}

/// `text`, whose breaks are `\n`, written with `endings`.
pub(crate) fn write_text(text: &str, endings: Endings) -> String {
    match endings {
        Endings::Lf => text.to_owned(),
        Endings::Crlf => text.replace('\n', "\r\n"),
    }
}

/// What the version of a file on disk looks like, without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    /// The size in bytes.
    len: u64,
    /// When it was last written, when the platform says.
    modified: Option<SystemTime>,
}

/// The stamp of the file at `file`, when it can be read and is a file.
pub(crate) fn stamp(file: &Path) -> Option<Stamp> {
    let metadata = std::fs::metadata(file).ok()?;
    metadata.is_file().then(|| Stamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// The whole text of the file at `file` and the stamp it had when read.
///
/// This is the read the caller runs on the background executor: a large file
/// must not hold up a frame.
pub(crate) fn load(file: &Path) -> Option<(String, Option<Stamp>)> {
    let stamp = stamp(file);
    let bytes = std::fs::read(file).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    Some((text, stamp))
}

/// One file open in the source view: the text it was read from or last written
/// to, and whether the editor has moved away from it.
#[derive(Debug, Clone)]
pub(crate) struct Buffer {
    endings: Endings,
    baseline: String,
    dirty: bool,
}

impl Buffer {
    /// A buffer for `text` as it is on disk, keeping the line breaks it uses.
    pub(crate) fn new(text: &str) -> Buffer {
        Buffer {
            endings: detect(text),
            baseline: unix(text),
            dirty: false,
        }
    }

    /// The line breaks the file uses.
    pub(crate) fn endings(&self) -> Endings {
        self.endings
    }

    /// The text as it was read or saved, with Unix line breaks.
    pub(crate) fn baseline(&self) -> &str {
        &self.baseline
    }

    /// Whether the editor text differs from the baseline.
    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }

    /// Notes the editor's text after a change, and returns whether it is dirty.
    pub(crate) fn edited(&mut self, text: &str) -> bool {
        self.dirty = text != self.baseline;
        self.dirty
    }

    /// Notes the text as it was written to disk, which is no change again.
    pub(crate) fn saved(&mut self, text: &str) {
        self.baseline = text.to_owned();
        self.dirty = false;
    }

    /// Takes the text of a file that changed on disk while it was not edited.
    pub(crate) fn reloaded(&mut self, text: &str) {
        self.endings = detect(text);
        self.saved(&unix(text));
    }
}

/// The file `path` names inside `root`, when it stays inside it.
///
/// Only plain components are accepted: an absolute path, a drive, a root or a
/// parent (`..`) means the path leaves the repository, and the source view
/// never writes outside it. Symbolic links are not resolved; the path is the
/// one the comparison reported.
pub(crate) fn inside(root: &Path, path: &RelPath) -> Option<PathBuf> {
    let mut file = root.to_path_buf();
    let mut plain = false;
    for part in Path::new(path.as_str()).components() {
        match part {
            Component::Normal(name) => {
                file.push(name);
                plain = true;
            }
            _ => return None,
        }
    }
    plain.then_some(file)
}

/// Writes `text` to the file `path` names under `root`, with the line breaks
/// `endings` names, and returns the path written and the file's new stamp.
///
/// A path that leaves `root` is refused rather than written, so a mistake in a
/// path can never touch another file.
pub(crate) fn save(
    root: &Path,
    path: &RelPath,
    text: &str,
    endings: Endings,
) -> io::Result<(PathBuf, Option<Stamp>)> {
    let file = inside(root, path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is outside the repository", path.as_str()),
        )
    })?;
    std::fs::write(&file, write_text(text, endings))?;
    let written = stamp(&file);
    Ok((file, written))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(directory: &tempfile::TempDir, name: &str) -> Vec<u8> {
        std::fs::read(directory.path().join(name)).expect("the written file")
    }

    #[test]
    fn windows_and_unix_breaks_are_told_apart() {
        assert_eq!(detect("fn a() {}\nfn b() {}\n"), Endings::Lf);
        assert_eq!(detect("fn a() {}\r\nfn b() {}\r\n"), Endings::Crlf);
        assert_eq!(detect("fn a() {}\nfn b() {}\r\nfn c() {}\n"), Endings::Lf);
        assert_eq!(detect("one line"), Endings::Lf);
        assert_eq!(detect(""), Endings::Lf);
    }

    #[test]
    fn a_text_is_normalised_to_unix_breaks_and_written_back() {
        assert_eq!(unix("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(unix("a\nb\n"), "a\nb\n");
        assert_eq!(write_text("a\nb\n", Endings::Crlf), "a\r\nb\r\n");
        assert_eq!(write_text("a\nb\n", Endings::Lf), "a\nb\n");
    }

    #[test]
    fn a_buffer_is_clean_until_the_text_moves_away_from_disk() {
        let mut buffer = Buffer::new("one\r\ntwo\r\n");
        assert_eq!(buffer.endings(), Endings::Crlf);
        assert_eq!(buffer.baseline(), "one\ntwo\n");
        assert!(!buffer.dirty());

        assert!(buffer.edited("one\ntwo\nthree\n"));
        assert!(buffer.dirty());
        // Undoing back to the text on disk is not a change either.
        assert!(!buffer.edited("one\ntwo\n"));
        assert!(!buffer.dirty());

        assert!(buffer.edited("changed\n"));
        buffer.saved("changed\n");
        assert!(!buffer.dirty());
        assert_eq!(buffer.baseline(), "changed\n");
        assert_eq!(
            write_text(buffer.baseline(), buffer.endings()),
            "changed\r\n"
        );
    }

    #[test]
    fn a_reloaded_file_takes_its_own_breaks_again() {
        let mut buffer = Buffer::new("one\n");
        buffer.edited("edited\n");
        buffer.reloaded("new\r\nfile\r\n");
        assert!(!buffer.dirty());
        assert_eq!(buffer.endings(), Endings::Crlf);
        assert_eq!(buffer.baseline(), "new\nfile\n");
    }

    #[test]
    fn only_paths_inside_the_root_are_accepted() {
        let root = Path::new("/work/repo");
        assert_eq!(
            inside(root, &RelPath::new("src/lib.rs")),
            Some(root.join("src").join("lib.rs"))
        );
        assert_eq!(inside(root, &RelPath::new("../src/lib.rs")), None);
        assert_eq!(inside(root, &RelPath::new("src/../../lib.rs")), None);
        assert_eq!(inside(root, &RelPath::new("/etc/passwd")), None);
        assert_eq!(inside(root, &RelPath::new("")), None);
    }

    #[test]
    fn saving_writes_the_line_breaks_the_file_had() {
        let directory = tempfile::tempdir().expect("a directory");
        std::fs::write(directory.path().join("a.rs"), "one\r\ntwo\r\n").expect("a file");
        let text = unix(&std::fs::read_to_string(directory.path().join("a.rs")).expect("text"));
        let mut buffer = Buffer::new(&text);

        let (file, stamp) = save(
            directory.path(),
            &RelPath::new("a.rs"),
            "one\r\ntwo\r\nthree\r\n",
            buffer.endings(),
        )
        .expect("a saved file");

        assert_eq!(file, directory.path().join("a.rs"));
        assert_eq!(written(&directory, "a.rs"), b"one\r\ntwo\r\nthree\r\n");
        assert_eq!(stamp, super::stamp(&file));
        buffer.saved("one\ntwo\nthree\n");
        assert!(!buffer.dirty());
    }

    #[test]
    fn saving_outside_the_root_is_refused() {
        let directory = tempfile::tempdir().expect("a directory");
        let outside = directory.path().join("outside.rs");

        let error = save(
            directory.path(),
            &RelPath::new("../outside.rs"),
            "text\n",
            Endings::Lf,
        )
        .expect_err("a refusal");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!outside.exists(), "nothing is written outside the root");
    }

    #[test]
    fn a_stamp_changes_when_the_file_does() {
        let directory = tempfile::tempdir().expect("a directory");
        let file = directory.path().join("a.rs");
        std::fs::write(&file, "one\n").expect("a file");
        let first = stamp(&file).expect("a stamp");

        std::fs::write(&file, "one\ntwo\n").expect("a longer file");

        assert_ne!(stamp(&file), Some(first));
        assert!(stamp(&directory.path().join("missing.rs")).is_none());
    }

    #[test]
    fn a_file_is_loaded_with_the_stamp_it_had() {
        let directory = tempfile::tempdir().expect("a directory");
        let file = directory.path().join("a.rs");
        std::fs::write(&file, "one\r\n").expect("a file");

        let (text, stamp) = load(&file).expect("the file's text");

        assert_eq!(text, "one\r\n");
        assert_eq!(stamp, super::stamp(&file));
        assert!(load(&directory.path().join("missing.rs")).is_none());
    }
}
