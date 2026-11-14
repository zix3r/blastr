//! The error type shared by the analysis pipeline and its front ends.

/// Everything that can go wrong while running the analysis pipeline.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A Git operation failed.
    #[error(transparent)]
    Git(#[from] blastr_git::Error),
    /// A cache operation failed.
    #[error(transparent)]
    Store(#[from] blastr_store::Error),
    /// The operation was cancelled through its `CancelToken`.
    #[error("cancelled")]
    Cancelled,
    /// The feature exists in the interface but is not implemented yet.
    #[error("`{0}` is not implemented yet")]
    NotImplemented(&'static str),
}

impl Error {
    /// One line telling the user what to do next, when there is something useful to say.
    pub fn hint(&self) -> Option<String> {
        let hint = match self {
            Error::Git(blastr_git::Error::NotARepository { .. }) => {
                "run blastr inside a Git repository, or pass its path"
            }
            Error::Git(blastr_git::Error::Bare { .. }) => {
                "blastr needs a repository with a working tree"
            }
            Error::Store(blastr_store::Error::NewerSchema { .. }) => {
                "update blastr, or run `blastr cache clear` with the newer version"
            }
            Error::Store(blastr_store::Error::Sqlite(_))
            | Error::Store(blastr_store::Error::Io(_)) => {
                "if the cache is damaged, delete it with `blastr cache clear`"
            }
            Error::Git(_) | Error::Store(_) | Error::Cancelled | Error::NotImplemented(_) => {
                return None;
            }
        };
        Some(hint.to_owned())
    }
}

/// A convenient alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_a_repository_hint() {
        let err = Error::Git(blastr_git::Error::NotARepository { path: "x".into() });
        assert_eq!(
            err.hint().as_deref(),
            Some("run blastr inside a Git repository, or pass its path")
        );
    }

    #[test]
    fn bare_repository_hint() {
        let err = Error::Git(blastr_git::Error::Bare { path: "x".into() });
        assert_eq!(
            err.hint().as_deref(),
            Some("blastr needs a repository with a working tree")
        );
    }

    #[test]
    fn newer_schema_hint() {
        let err = Error::Store(blastr_store::Error::NewerSchema {
            found: 2,
            supported: 1,
        });
        assert_eq!(
            err.hint().as_deref(),
            Some("update blastr, or run `blastr cache clear` with the newer version")
        );
    }

    #[test]
    fn damaged_cache_hint() {
        let io = Error::Store(blastr_store::Error::Io(std::io::Error::other("x")));
        assert_eq!(
            io.hint().as_deref(),
            Some("if the cache is damaged, delete it with `blastr cache clear`")
        );
    }

    #[test]
    fn no_hint_for_cancelled_and_unimplemented() {
        assert_eq!(Error::Cancelled.hint(), None);
        assert_eq!(Error::NotImplemented("diff").hint(), None);
    }

    #[test]
    fn no_hint_for_other_git_and_store_errors() {
        assert_eq!(Error::Git(blastr_git::Error::Git("x".into())).hint(), None);
        assert_eq!(Error::Store(blastr_store::Error::NoUserDirs).hint(), None);
    }

    #[test]
    fn not_implemented_display() {
        assert_eq!(
            Error::NotImplemented("diff").to_string(),
            "`diff` is not implemented yet"
        );
    }
}
