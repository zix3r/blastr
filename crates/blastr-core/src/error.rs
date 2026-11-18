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
    /// Loading or analysing the project failed.
    #[error(transparent)]
    Ra(#[from] blastr_ra::Error),
    /// A file could not be read or written.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A cached graph could not be encoded or decoded.
    #[error("cache data error: {0}")]
    Codec(String),
    /// The operation was cancelled through its `CancelToken`.
    #[error("cancelled")]
    Cancelled,
    /// An ignore pattern is not a usable glob.
    #[error(transparent)]
    Ignore(#[from] crate::config::PatternError),
    /// A symbol the user named could not be found.
    #[error(transparent)]
    Lookup(#[from] crate::impact::lookup::LookupError),
    /// A what-if query does not fit the symbol it names.
    #[error(transparent)]
    WhatIf(#[from] crate::impact::whatif::WhatIfError),
    /// A session was asked for on a comparison it cannot watch.
    #[error("this comparison cannot be watched: {0}")]
    SessionNotWatching(String),
    /// A cache directory could not be removed.
    #[error("cannot remove `{}`: {message}", .path.display())]
    RemoveDir {
        /// The entry that could not be removed.
        path: std::path::PathBuf,
        /// What the system reported for it.
        message: String,
    },
    /// The feature exists in the interface but is not implemented yet.
    #[error("`{0}` is not implemented yet")]
    NotImplemented(&'static str),
}

impl Error {
    /// One line telling the user what to do next, when there is something useful to say.
    pub fn hint(&self) -> Option<String> {
        let hint = match self {
            Error::Ignore(error) => return error.hint(),
            Error::Lookup(error)
            | Error::WhatIf(crate::impact::whatif::WhatIfError::Lookup(error)) => {
                return lookup_hint(error);
            }
            Error::WhatIf(crate::impact::whatif::WhatIfError::WrongKind { .. }) => {
                "assume a kind of change that fits the symbol, such as a body or signature change"
            }
            Error::SessionNotWatching(_) => {
                "only the working tree can be followed, so compare a revision with it"
            }
            Error::Cancelled | Error::NotImplemented(_) => {
                return None;
            }
            Error::Git(blastr_git::Error::NotARepository { .. }) => {
                "open a folder inside a Git repository"
            }
            Error::Git(blastr_git::Error::Bare { .. }) => {
                "blastr needs a repository with a working tree"
            }
            Error::Store(blastr_store::Error::NewerSchema { .. }) => {
                "install the newer Blastr, or delete the cache database"
            }
            Error::Store(blastr_store::Error::Sqlite(_))
            | Error::Store(blastr_store::Error::Io(_)) => {
                "if the cache is damaged, delete the cache database"
            }
            Error::Git(blastr_git::Error::UnknownRevision(_)) => {
                "check the revision with `git log`; branches, tags and commit ids all work"
            }
            Error::Ra(blastr_ra::Error::NoManifest { .. }) => {
                "open the root of a Cargo project or of a workspace"
            }
            Error::Ra(blastr_ra::Error::MissingRustSrc) => {
                "install them with `rustup component add rust-src`"
            }
            Error::Ra(blastr_ra::Error::Cargo(_)) => {
                "check that `cargo metadata` works in this project"
            }
            Error::Ra(blastr_ra::Error::Toolchain(_)) => {
                "check that `cargo --version` works in this directory"
            }
            Error::Codec(_) => "delete the cache database",
            Error::RemoveDir { .. } => {
                "close whatever holds the file (an editor, an antivirus scan, another build), \
                 then clear the cache again"
            }
            Error::Git(_) | Error::Store(_) | Error::Ra(_) | Error::Io(_) => {
                return None;
            }
        };
        Some(hint.to_owned())
    }
}

/// What to try after a symbol lookup failed.
fn lookup_hint(error: &crate::impact::lookup::LookupError) -> Option<String> {
    use crate::impact::lookup::LookupError;
    match error {
        LookupError::NotFound { suggestions, .. } if suggestions.is_empty() => Some(
            "write the path as the result shows it, such as `my_crate::module::item`".to_owned(),
        ),
        LookupError::NotFound { suggestions, .. } => {
            Some(format!("did you mean {}?", suggestions.join(", ")))
        }
        LookupError::Ambiguous { candidates, .. } => {
            Some(format!("use a longer path: {}", candidates.join(", ")))
        }
    }
}

/// A convenient alias for results produced by this crate.
impl From<blastr_model::Cancelled> for Error {
    fn from(_: blastr_model::Cancelled) -> Self {
        Error::Cancelled
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_a_repository_hint() {
        let err = Error::Git(blastr_git::Error::NotARepository { path: "x".into() });
        assert_eq!(
            err.hint().as_deref(),
            Some("open a folder inside a Git repository")
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
            Some("install the newer Blastr, or delete the cache database")
        );
    }

    #[test]
    fn damaged_cache_hint() {
        let io = Error::Store(blastr_store::Error::Io(std::io::Error::other("x")));
        assert_eq!(
            io.hint().as_deref(),
            Some("if the cache is damaged, delete the cache database")
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
