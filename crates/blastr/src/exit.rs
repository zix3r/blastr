//! Process exit codes.

/// The codes blastr exits with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    /// Everything ran and nothing was wrong.
    Success = 0,
    /// Something went wrong.
    Error = 1,
    /// Findings of the class given to `--fail-on` were reported.
    Findings = 2,
}

impl From<Exit> for std::process::ExitCode {
    fn from(exit: Exit) -> Self {
        std::process::ExitCode::from(exit as u8)
    }
}
