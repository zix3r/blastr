//! The text of a file on either side of a comparison, read once and kept.
//!
//! The views need file text for the source panes; reading it from disk or from
//! a Git tree is fast, but doing it on every frame is not. A document is read
//! on first use and forgotten when another repository is opened.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use blastr_git::Repo;
use blastr_model::{RelPath, Source};

/// One file's text, split into lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Doc {
    /// Where the text comes from.
    pub source: Source,
    /// The path inside the repository.
    pub path: RelPath,
    /// The lines, without their line breaks.
    pub lines: Vec<String>,
}

impl Doc {
    /// The number of lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }
}

/// The documents read so far, for one repository.
#[derive(Debug, Default)]
pub(crate) struct Docs {
    root: PathBuf,
    repo: Option<Repo>,
    cache: HashMap<(Source, RelPath), Option<Arc<Doc>>>,
}

impl Docs {
    /// A cache for the repository at `root`.
    pub(crate) fn new(root: PathBuf) -> Docs {
        Docs {
            root,
            repo: None,
            cache: HashMap::new(),
        }
    }

    /// Forgets everything read for the previous repository.
    pub(crate) fn reset(&mut self, root: PathBuf) {
        if self.root != root {
            *self = Docs::new(root);
        } else {
            self.cache.clear();
        }
    }

    /// Forgets what was read for one source, after it changed on disk.
    pub(crate) fn invalidate(&mut self, source: &Source) {
        self.cache.retain(|(cached, _), _| cached != source);
    }

    /// The text of `path` on `source`.
    pub(crate) fn get(&mut self, source: &Source, path: &RelPath) -> Option<Arc<Doc>> {
        let key = (source.clone(), path.clone());
        if let Some(cached) = self.cache.get(&key) {
            return cached.clone();
        }
        let text = self.read(source, path);
        let doc = text.map(|text| {
            Arc::new(Doc {
                source: source.clone(),
                path: path.clone(),
                lines: text.lines().map(str::to_owned).collect(),
            })
        });
        self.cache.insert(key, doc.clone());
        doc
    }

    /// Reads the text of `path` on `source`.
    fn read(&mut self, source: &Source, path: &RelPath) -> Option<String> {
        match source {
            Source::Worktree => std::fs::read(self.root.join(path.as_str()))
                .ok()
                .and_then(utf8),
            Source::Dir(directory) => std::fs::read(directory.join(path.as_str()))
                .ok()
                .and_then(utf8),
            Source::Index | Source::Tree { .. } => {
                let repo = self.repo()?;
                repo.read_file(source, path).ok().flatten().and_then(utf8)
            }
        }
    }

    /// The repository of the working tree, opened on first use.
    fn repo(&mut self) -> Option<&Repo> {
        if self.repo.is_none() {
            let root = blastr_git::discover(&self.root).ok()?;
            self.repo = Repo::open(&root).ok();
        }
        self.repo.as_ref()
    }
}

/// The text of `bytes`, when it is UTF-8.
fn utf8(bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_counts_its_lines() {
        let doc = Doc {
            source: Source::Worktree,
            path: RelPath::new("src/lib.rs"),
            lines: vec!["fn a() {}".to_owned(), "fn b() {}".to_owned()],
        };
        assert_eq!(doc.len(), 2);
        assert_eq!(doc.lines.first().map(String::as_str), Some("fn a() {}"));
        assert_eq!(doc.lines.last().map(String::as_str), Some("fn b() {}"));
    }

    #[test]
    fn a_worktree_file_is_read_and_cached() {
        let directory = tempfile::tempdir().expect("a directory");
        std::fs::write(directory.path().join("a.rs"), "one\ntwo\n").expect("a file");
        let mut docs = Docs::new(directory.path().to_path_buf());

        let doc = docs
            .get(&Source::Worktree, &RelPath::new("a.rs"))
            .expect("the file");

        assert_eq!(doc.lines, vec!["one", "two"]);
        assert!(Arc::ptr_eq(
            &doc,
            &docs
                .get(&Source::Worktree, &RelPath::new("a.rs"))
                .expect("the cached file")
        ));
        assert!(
            docs.get(&Source::Worktree, &RelPath::new("missing.rs"))
                .is_none()
        );
    }

    #[test]
    fn clearing_forgets_a_source() {
        let directory = tempfile::tempdir().expect("a directory");
        std::fs::write(directory.path().join("a.rs"), "one\n").expect("a file");
        let mut docs = Docs::new(directory.path().to_path_buf());
        assert!(docs.get(&Source::Worktree, &RelPath::new("a.rs")).is_some());

        docs.invalidate(&Source::Worktree);

        assert!(docs.get(&Source::Worktree, &RelPath::new("a.rs")).is_some());
    }
}
