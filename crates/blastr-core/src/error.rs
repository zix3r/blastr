//! The error type shared by the analysis pipeline and its front ends.

/// Everything that can go wrong while running the analysis pipeline.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A Git operation failed.
    #[error(transparent)]
    Git(#[from] blastr_git::Error),
    /// No home directory could be determined for the current user.
    #[error("could not determine the user's cache and configuration directories")]
    NoUserDirs,
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
}

impl Error {
    /// One line telling the user what to do next, when there is something useful to say.
    pub fn hint(&self) -> Option<String> {
        let hint = match self {
            Error::Ignore(error) => return error.hint(),
            Error::WhatIf(crate::impact::whatif::WhatIfError::NotFound(_)) => {
                "the working tree changed since the symbol was listed; compare again"
            }
            Error::WhatIf(crate::impact::whatif::WhatIfError::WrongKind { .. }) => {
                "assume a kind of change that fits the symbol, such as a body or signature change"
            }
            Error::SessionNotWatching(_) => {
                "only the working tree can be followed, so compare a revision with it"
            }
            Error::Cancelled => {
                return None;
            }
            Error::Git(blastr_git::Error::NotARepository { .. }) => {
                "open a folder inside a Git repository"
            }
            Error::Git(blastr_git::Error::Bare { .. }) => {
                "blastr needs a repository with a working tree"
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
            Error::Codec(_) => "clear the cache",
            Error::RemoveDir { .. } => {
                "close whatever holds the file (an editor, an antivirus scan, another build), \
                 then clear the cache again"
            }
            Error::Git(_) | Error::NoUserDirs | Error::Ra(_) | Error::Io(_) => {
                return None;
            }
        };
        Some(hint.to_owned())
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
    fn no_hint_for_cancelled() {
        assert_eq!(Error::Cancelled.hint(), None);
    }

    #[test]
    fn no_hint_for_other_git_and_cache_errors() {
        assert_eq!(Error::Git(blastr_git::Error::Git("x".into())).hint(), None);
        assert_eq!(Error::NoUserDirs.hint(), None);
    }
}
