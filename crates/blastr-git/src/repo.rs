//! Opening a repository and turning a revision specification into two sides.

use std::path::{Path, PathBuf};

use blastr_model::{Comparison, RelPath, RevSpec, Source};
use gix::bstr::{BStr, ByteSlice};
use gix::objs::tree::EntryKind;

use crate::{Error, RepoRoot};

/// An opened repository.
#[derive(Debug, Clone)]
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    /// Opens the repository whose working tree is `root`.
    pub fn open(root: &RepoRoot) -> Result<Repo, Error> {
        let repo = gix::open(root.path()).map_err(Error::git)?;
        if repo.workdir().is_none() {
            return Err(Error::Bare {
                path: root.path().to_path_buf(),
            });
        }
        Ok(Repo {
            root: root.path().to_path_buf(),
        })
    }

    /// The root of the working tree.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A fresh handle on the repository on disk.
    pub(crate) fn repo(&self) -> Result<gix::Repository, Error> {
        gix::open(&self.root).map_err(Error::git)
    }

    /// Turns what the user typed into the two sides to compare.
    ///
    /// A revision is compared with its first parent, or with parent `N` when
    /// `spec.parent` is set; `a..b` compares two commits; `worktree` and
    /// `staged` compare `HEAD` with the working tree and the index. Without any
    /// of these, the working tree is compared with `HEAD` when something
    /// tracked is modified or staged, and `HEAD` with its first parent
    /// otherwise.
    pub fn resolve(&self, spec: &RevSpec) -> Result<Comparison, Error> {
        let repo = self.repo()?;
        if let Some(text) = spec.rev.as_deref() {
            if text.contains("...") {
                return Err(Error::UnknownRevision(text.to_string()));
            }
            if let Some((from, to)) = text.split_once("..") {
                let base = commit_source(&repo, from)?;
                let head = commit_source(&repo, to)?;
                return Ok(Comparison { base, head });
            }
            return against_parent(&repo, text, spec.parent);
        }
        let base = head_tree_source(&repo)?;
        if spec.staged {
            return Ok(Comparison {
                base,
                head: Source::Index,
            });
        }
        if spec.worktree || (spec.parent.is_none() && repo.is_dirty().map_err(Error::git)?) {
            return Ok(Comparison {
                base,
                head: Source::Worktree,
            });
        }
        against_parent(&repo, "HEAD", spec.parent)
    }

    /// A stable identifier of the repository: BLAKE3 of the root path.
    pub fn repository_id(&self) -> String {
        blake3::hash(self.root.as_os_str().as_encoded_bytes())
            .to_hex()
            .to_string()
    }

    /// True when `HEAD` holds a `.gitmodules` file.
    pub fn has_submodules(&self) -> Result<bool, Error> {
        Ok(self.head_file(".gitmodules")?.is_some())
    }

    /// True when `.gitattributes` at `HEAD` assigns the LFS filter.
    pub fn uses_lfs(&self) -> Result<bool, Error> {
        let Some(content) = self.head_file(".gitattributes")? else {
            return Ok(false);
        };
        let needle: &[u8] = b"filter=lfs";
        Ok(content.windows(needle.len()).any(|part| part == needle))
    }

    /// The content of a file in the tree of `HEAD`.
    fn head_file(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        let repo = self.repo()?;
        let source = head_tree_source(&repo)?;
        self.read_file(&source, &RelPath::new(path))
    }
}

/// The committed side for `text`, compared with the parent that `parent` names.
fn against_parent(
    repo: &gix::Repository,
    text: &str,
    parent: Option<u32>,
) -> Result<Comparison, Error> {
    let commit = find_commit(repo, text)?;
    let head = tree_source(&commit)?;
    let parents: Vec<gix::ObjectId> = commit.parent_ids().map(|id| id.detach()).collect();
    let available = u32::try_from(parents.len()).unwrap_or(u32::MAX);
    let requested = parent.unwrap_or(1);
    if requested == 0 || requested > available {
        if parent.is_some() {
            return Err(Error::NoSuchParent {
                commit: commit.id.to_string(),
                parents: available,
                requested,
            });
        }
        return Err(Error::RootCommit(commit.id.to_string()));
    }
    let parent = repo
        .find_commit(parents[(requested - 1) as usize])
        .map_err(Error::git)?;
    Ok(Comparison {
        base: tree_source(&parent)?,
        head,
    })
}

/// The commit that `text` names, peeling annotated tags.
fn find_commit<'repo>(
    repo: &'repo gix::Repository,
    text: &str,
) -> Result<gix::Commit<'repo>, Error> {
    let unknown = || Error::UnknownRevision(text.to_string());
    let id = repo.rev_parse_single(text).map_err(|_| unknown())?;
    let object = repo.find_object(id).map_err(|_| unknown())?;
    object.peel_to_commit().map_err(|_| unknown())
}

/// The tree of a commit, labelled with a short version of the commit id.
fn tree_source(commit: &gix::Commit<'_>) -> Result<Source, Error> {
    let tree = commit.tree_id().map_err(Error::git)?.detach();
    Ok(Source::Tree {
        id: tree.to_string(),
        label: short_id(commit.id),
    })
}

/// The committed side that `text` names.
fn commit_source(repo: &gix::Repository, text: &str) -> Result<Source, Error> {
    let commit = find_commit(repo, text)?;
    tree_source(&commit)
}

/// The id of the tree `HEAD` points to, or of the empty tree without commits.
pub(crate) fn head_tree_id(repo: &gix::Repository) -> Result<gix::ObjectId, Error> {
    let head = repo.head().map_err(Error::git)?;
    let Some(id) = head.id() else {
        return Ok(gix::ObjectId::empty_tree(repo.object_hash()));
    };
    let commit = repo.find_commit(id).map_err(Error::git)?;
    Ok(commit.tree_id().map_err(Error::git)?.detach())
}

/// The tree of `HEAD` as a source, or the empty tree when there are no commits.
pub(crate) fn head_tree_source(repo: &gix::Repository) -> Result<Source, Error> {
    let label = match repo.head().map_err(Error::git)?.id() {
        Some(id) => short_id(id.detach()),
        None => "HEAD".to_owned(),
    };
    Ok(Source::Tree {
        id: head_tree_id(repo)?.to_string(),
        label,
    })
}

/// The first seven hexadecimal characters of an object id.
pub(crate) fn short_id(id: gix::ObjectId) -> String {
    id.to_hex_with_len(7).to_string()
}

/// Finds a tree by the hexadecimal id that a [`Source`] holds.
pub(crate) fn find_tree<'repo>(
    repo: &'repo gix::Repository,
    id: &str,
) -> Result<gix::Tree<'repo>, Error> {
    let id = gix::ObjectId::from_hex(id.as_bytes()).map_err(Error::git)?;
    repo.find_tree(id).map_err(Error::git)
}

/// The content of a blob.
pub(crate) fn blob_bytes(repo: &gix::Repository, id: gix::ObjectId) -> Result<Vec<u8>, Error> {
    let mut blob = repo.find_blob(id).map_err(Error::git)?;
    Ok(blob.take_data())
}

/// True for tree entries that hold file content, as opposed to directories,
/// submodules and symbolic links.
pub(crate) fn is_file(kind: EntryKind) -> bool {
    matches!(kind, EntryKind::Blob | EntryKind::BlobExecutable)
}

/// The text of a Git path, with `/` separators.
pub(crate) fn rel_path(path: impl AsRef<BStr>) -> RelPath {
    RelPath::new(path.as_ref().to_str_lossy())
}

#[cfg(test)]
mod tests {
    use blastr_model::{Comparison, RevSpec, Source};

    use crate::Error;
    use crate::test_support::TestRepo;

    /// The committed side that `commit` produces.
    fn tree(test: &TestRepo, commit: &str) -> Source {
        Source::Tree {
            id: test.tree_id(commit),
            label: commit[..7].to_owned(),
        }
    }

    /// Resolves `spec` in the repository of `test`.
    fn resolve(test: &TestRepo, spec: RevSpec) -> Result<Comparison, Error> {
        test.open().resolve(&spec)
    }

    #[test]
    fn without_arguments_compares_head_with_its_parent() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "first");
        let second = test.commit(&[("a.txt", "two")], "second");

        let comparison = resolve(&test, RevSpec::default()).expect("a comparison");

        assert_eq!(comparison.base, tree(&test, &first));
        assert_eq!(comparison.head, tree(&test, &second));
    }

    #[test]
    fn without_arguments_compares_head_with_the_working_tree_when_dirty() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let second = test.commit(&[("a.txt", "two")], "second");
        test.write("a.txt", "three");

        let comparison = resolve(&test, RevSpec::default()).expect("a comparison");

        assert_eq!(comparison.base, tree(&test, &second));
        assert_eq!(comparison.head, Source::Worktree);
    }

    #[test]
    fn a_single_revision_is_compared_with_its_first_parent() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "first");
        let second = test.commit(&[("a.txt", "two")], "second");
        let spec = RevSpec {
            rev: Some("HEAD".to_owned()),
            ..Default::default()
        };

        let comparison = resolve(&test, spec).expect("a comparison");

        assert_eq!(comparison.base, tree(&test, &first));
        assert_eq!(comparison.head, tree(&test, &second));
    }

    #[test]
    fn a_range_compares_the_two_commits() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "first");
        test.commit(&[("a.txt", "two")], "second");
        let third = test.commit(&[("a.txt", "three")], "third");
        let spec = RevSpec {
            rev: Some(format!("{first}..{third}")),
            ..Default::default()
        };

        let comparison = resolve(&test, spec).expect("a comparison");

        assert_eq!(comparison.base, tree(&test, &first));
        assert_eq!(comparison.head, tree(&test, &third));
    }

    #[test]
    fn the_worktree_flag_compares_head_with_the_working_tree() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let second = test.commit(&[("a.txt", "two")], "second");
        let spec = RevSpec {
            worktree: true,
            ..Default::default()
        };

        let comparison = resolve(&test, spec).expect("a comparison");

        assert_eq!(comparison.base, tree(&test, &second));
        assert_eq!(comparison.head, Source::Worktree);
    }

    #[test]
    fn the_staged_flag_compares_head_with_the_index() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let second = test.commit(&[("a.txt", "two")], "second");
        let spec = RevSpec {
            staged: true,
            ..Default::default()
        };

        let comparison = resolve(&test, spec).expect("a comparison");

        assert_eq!(comparison.base, tree(&test, &second));
        assert_eq!(comparison.head, Source::Index);
    }

    #[test]
    fn an_unknown_revision_is_reported() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let spec = RevSpec {
            rev: Some("no-such-revision".to_owned()),
            ..Default::default()
        };

        let error = resolve(&test, spec).expect_err("no such revision");

        assert!(matches!(error, Error::UnknownRevision(text) if text == "no-such-revision"));
    }

    #[test]
    fn an_unknown_side_of_a_range_is_reported() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let spec = RevSpec {
            rev: Some("nope..HEAD".to_owned()),
            ..Default::default()
        };

        let error = resolve(&test, spec).expect_err("no such revision");

        assert!(matches!(error, Error::UnknownRevision(text) if text == "nope"));
    }

    #[test]
    fn a_three_dot_range_is_not_supported() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "first");
        let second = test.commit(&[("a.txt", "two")], "second");
        let text = format!("{first}...{second}");
        let spec = RevSpec {
            rev: Some(text.clone()),
            ..Default::default()
        };

        let error = resolve(&test, spec).expect_err("three dots are not supported");

        assert!(matches!(error, Error::UnknownRevision(given) if given == text));
    }

    #[test]
    fn a_root_commit_has_no_parent_to_compare_against() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "first");
        let spec = RevSpec {
            rev: Some(first.clone()),
            ..Default::default()
        };

        let error = resolve(&test, spec).expect_err("a root commit");

        assert!(matches!(error, Error::RootCommit(commit) if commit == first));
    }

    #[test]
    fn the_first_parent_can_be_named_explicitly() {
        let mut test = TestRepo::new();
        let first = test.commit(&[("a.txt", "one")], "first");
        let second = test.commit(&[("a.txt", "two")], "second");
        let spec = RevSpec {
            rev: Some("HEAD".to_owned()),
            parent: Some(1),
            ..Default::default()
        };

        let comparison = resolve(&test, spec).expect("a comparison");

        assert_eq!(comparison.base, tree(&test, &first));
        assert_eq!(comparison.head, tree(&test, &second));
    }

    #[test]
    fn an_out_of_range_parent_is_reported() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let second = test.commit(&[("a.txt", "two")], "second");
        let spec = RevSpec {
            rev: Some("HEAD".to_owned()),
            parent: Some(2),
            ..Default::default()
        };

        let error = resolve(&test, spec).expect_err("only one parent");

        let expected = Error::NoSuchParent {
            commit: second,
            parents: 1,
            requested: 2,
        };
        assert_eq!(format!("{error}"), format!("{expected}"));
    }

    #[test]
    fn the_repository_id_is_stable() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let repo = test.open();

        let once = repo.repository_id();
        let twice = repo.repository_id();

        assert_eq!(once, twice);
        assert_eq!(once.len(), 64);
    }

    #[test]
    fn submodules_and_lfs_are_detected_at_head() {
        let mut test = TestRepo::new();
        test.commit(&[("a.txt", "one")], "first");
        let repo = test.open();
        assert!(!repo.has_submodules().expect("submodules"));
        assert!(!repo.uses_lfs().expect("lfs"));

        test.commit(
            &[
                (".gitmodules", "[submodule \"x\"]\n\tpath = x\n"),
                (
                    ".gitattributes",
                    "*.bin filter=lfs diff=lfs merge=lfs -text\n",
                ),
            ],
            "second",
        );

        assert!(repo.has_submodules().expect("submodules"));
        assert!(repo.uses_lfs().expect("lfs"));
    }
}
