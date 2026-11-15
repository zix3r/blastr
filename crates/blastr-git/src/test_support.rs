//! A helper that builds real repositories for the tests.

use std::path::Path;

use blastr_model::Source;

/// A repository in a temporary directory, with commits made through `gix` alone.
pub(crate) struct TestRepo {
    directory: tempfile::TempDir,
    clock: i64,
}

impl TestRepo {
    /// Creates an empty repository.
    pub(crate) fn new() -> TestRepo {
        let directory = tempfile::tempdir().expect("a temporary directory");
        gix::init(directory.path()).expect("an initialised repository");
        TestRepo {
            directory,
            clock: 1_700_000_000,
        }
    }

    /// The path of the working tree.
    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }

    /// Opens the repository the way the crate does.
    pub(crate) fn open(&self) -> crate::Repo {
        let root = crate::discover(self.directory.path()).expect("a repository");
        crate::Repo::open(&root).expect("an opened repository")
    }

    /// The committed side that `commit` produces.
    pub(crate) fn source(&self, commit: &str) -> Source {
        Source::Tree {
            id: self.tree_id(commit),
            label: commit[..7].to_owned(),
        }
    }

    /// The id of the tree of `commit`.
    pub(crate) fn tree_id(&self, commit: &str) -> String {
        let repo = self.repo();
        let id = gix::ObjectId::from_hex(commit.as_bytes()).expect("a valid object id");
        let commit = repo.find_commit(id).expect("a commit");
        commit.tree_id().expect("a tree").detach().to_string()
    }

    /// Commits `files` as the complete content of the repository and returns the commit id.
    ///
    /// The files are also written to the working tree and recorded in the index.
    pub(crate) fn commit(&mut self, files: &[(&str, &str)], message: &str) -> String {
        let tree = self.build_tree(files);
        for &(path, content) in files {
            self.write(path, content);
        }
        let parents: Vec<gix::ObjectId> = self
            .head_id()
            .map(|id| gix::ObjectId::from_hex(id.as_bytes()).expect("a valid commit id"))
            .into_iter()
            .collect();
        self.clock += 60;
        let signature = gix::actor::Signature {
            name: "Blastr Test".into(),
            email: "blastr@example.test".into(),
            time: gix::date::Time {
                seconds: self.clock,
                offset: 0,
            },
        };
        let mut buffer = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut buffer);
        let repo = self.repo();
        let id = repo
            .commit_as(signature, signature, "HEAD", message, tree, parents)
            .expect("a commit");
        self.write_tree_as_index(tree);
        id.detach().to_string()
    }

    /// Writes a file into the working tree.
    pub(crate) fn write(&self, path: &str, content: &str) {
        let full = self.directory.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("a parent directory");
        }
        std::fs::write(&full, content).expect("the file to be written");
    }

    /// Removes a file from the working tree.
    pub(crate) fn remove(&self, path: &str) {
        std::fs::remove_file(self.directory.path().join(path)).expect("the file to be removed");
    }

    /// Replaces the index with exactly `files`, as staging them would.
    pub(crate) fn write_index(&self, files: &[(&str, &str)]) {
        let tree = self.build_tree(files);
        self.write_tree_as_index(tree);
    }

    /// The commit `HEAD` points to, if there is one.
    fn head_id(&self) -> Option<String> {
        let repo = self.repo();
        let head = repo.head().expect("a head");
        head.id().map(|id| id.detach().to_string())
    }

    /// A fresh handle on the repository on disk.
    fn repo(&self) -> gix::Repository {
        gix::open(self.directory.path()).expect("an opened repository")
    }

    /// Writes a blob per file and builds a tree that holds all of them.
    fn build_tree(&self, files: &[(&str, &str)]) -> gix::ObjectId {
        let repo = self.repo();
        let empty = gix::ObjectId::empty_tree(repo.object_hash());
        let mut editor = repo.edit_tree(empty).expect("a tree editor");
        for &(path, content) in files {
            let blob = repo.write_blob(content.as_bytes()).expect("a blob");
            editor
                .upsert(path, gix::objs::tree::EntryKind::Blob, blob.detach())
                .expect("a tree entry");
        }
        editor.write().expect("a written tree").detach()
    }

    /// Records the content of a tree in the index file.
    fn write_tree_as_index(&self, tree: gix::ObjectId) {
        let repo = self.repo();
        let mut index = repo.index_from_tree(&tree).expect("an index from a tree");
        index
            .write(gix::index::write::Options::default())
            .expect("a written index");
    }
}

impl Default for TestRepo {
    fn default() -> Self {
        TestRepo::new()
    }
}
