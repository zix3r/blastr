//! Git access for Blastr.
//!
//! The crate talks to Git through the pure-Rust [`gix`] library, so no `git`
//! executable is needed. At this stage it only locates the repository that a
//! path belongs to.

use std::path::{Path, PathBuf};

/// The root directory of a Git working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRoot {
    path: PathBuf,
}

impl RepoRoot {
    /// The working-tree directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Errors returned when locating a Git repository.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `path` is not inside a Git repository.
    #[error("`{}` is not inside a Git repository", .path.display())]
    NotARepository { path: PathBuf },
    /// The repository has no working tree (it is bare).
    #[error("the repository at `{}` is bare and has no working tree", .path.display())]
    Bare { path: PathBuf },
    /// Any other failure reported by the Git library.
    #[error("git error: {0}")]
    Git(String),
}

/// The error for a path that has no repository above it.
fn not_a_repository(path: &Path) -> Error {
    Error::NotARepository {
        path: path.to_path_buf(),
    }
}

/// Removes the Windows verbatim prefix (`\\?\`) from a plain drive path.
fn simplify(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(text) = path.to_str()
            && let Some(rest) = text.strip_prefix(r"\\?\")
            && matches!(rest.as_bytes(), [drive, b':', ..] if drive.is_ascii_alphabetic())
        {
            return PathBuf::from(rest);
        }
    }
    path
}

/// Finds the Git repository that contains `path`, searching upwards.
pub fn discover(path: &Path) -> Result<RepoRoot, Error> {
    let repo = gix::discover(path).map_err(|_| not_a_repository(path))?;

    let Some(workdir) = repo.workdir() else {
        return Err(Error::Bare {
            path: path.to_path_buf(),
        });
    };

    let root = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());

    Ok(RepoRoot {
        path: simplify(root),
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// Canonicalises and simplifies `path`, so that different spellings of the
    /// same location compare equal.
    fn canonical(path: &Path) -> PathBuf {
        simplify(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
    }

    #[test]
    fn discovers_from_nested_directory() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        let repo = gix::init(temp.path()).expect("init repository");
        let expected = canonical(repo.workdir().expect("repository has a working tree"));

        let nested = temp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("create nested directory");

        let found = discover(&nested).expect("discover repository");
        assert_eq!(found.path(), expected.as_path());
    }

    #[test]
    fn discovers_from_repository_root() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        let repo = gix::init(temp.path()).expect("init repository");
        let expected = canonical(repo.workdir().expect("repository has a working tree"));

        let found = discover(temp.path()).expect("discover repository");
        assert_eq!(found.path(), expected.as_path());
    }

    #[test]
    fn discovered_path_has_no_verbatim_prefix() {
        let temp = tempfile::tempdir().expect("create temporary directory");
        gix::init(temp.path()).expect("init repository");

        let found = discover(temp.path()).expect("discover repository");
        assert!(!found.path().to_string_lossy().starts_with(r"\\?\"));
    }

    #[cfg(windows)]
    #[test]
    fn simplify_strips_the_prefix_from_a_drive_path() {
        let simplified = simplify(PathBuf::from(r"\\?\C:\a\b"));
        assert_eq!(simplified, PathBuf::from(r"C:\a\b"));
    }

    #[cfg(windows)]
    #[test]
    fn simplify_keeps_a_verbatim_unc_path() {
        let path = PathBuf::from(r"\\?\UNC\server\share\x");
        assert_eq!(simplify(path.clone()), path);
    }

    #[cfg(windows)]
    #[test]
    fn simplify_keeps_a_plain_drive_path() {
        let path = PathBuf::from(r"C:\a\b");
        assert_eq!(simplify(path.clone()), path);
    }

    #[test]
    fn reports_a_path_outside_any_repository() {
        let temp = tempfile::tempdir().expect("create temporary directory");

        let error = discover(temp.path()).expect_err("no repository expected");
        assert!(matches!(error, Error::NotARepository { .. }));
    }
}
