//! The commands that do real work in this stage, and what they share.

pub mod cache;
pub mod diff;
pub mod index;

use std::io::Write;
use std::path::{Path, PathBuf};

use blastr_model::{AnalysisOptions, ExecMode, RelPath, RevSpec};

use crate::cli::{Globals, RevArgs};

/// Builds the engine options from the global command-line options.
///
/// The repository path is the current directory, so `--manifest-path` only
/// contributes a directory when it lies below the current directory.
pub fn analysis_options(globals: &Globals, cwd: &Path) -> AnalysisOptions {
    AnalysisOptions {
        features: globals.features.clone(),
        all_features: globals.all_features,
        target: globals.target.clone(),
        exec: if globals.no_exec {
            ExecMode::Restricted
        } else {
            ExecMode::Full
        },
        manifest_dir: manifest_dir(globals.manifest_path.as_deref(), cwd),
    }
}

/// The directory of `--manifest-path`, relative to the current directory when
/// it is below it and as written otherwise.
fn manifest_dir(manifest_path: Option<&Path>, cwd: &Path) -> Option<RelPath> {
    let manifest_path = manifest_path?;
    let dir = match manifest_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => return None,
    };
    if let Ok(relative) = dir.strip_prefix(cwd) {
        if relative.as_os_str().is_empty() {
            return None;
        }
        return Some(
            RelPath::from_path(relative)
                .unwrap_or_else(|| RelPath::new(relative.to_string_lossy())),
        );
    }
    Some(RelPath::new(dir.to_string_lossy()))
}

/// Builds the revision specification from the revision arguments.
pub fn rev_spec(args: &RevArgs) -> RevSpec {
    RevSpec {
        rev: args.rev.clone(),
        worktree: args.worktree,
        staged: args.staged,
        parent: args.parent,
    }
}

/// The repository the commands work on: the current directory.
pub fn repository_root() -> Result<PathBuf, blastr_core::Error> {
    Ok(std::env::current_dir()?)
}

/// Writes text to stdout, where the result and nothing else goes.
pub fn write_stdout(text: &str) -> Result<(), blastr_core::Error> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()?;
    Ok(())
}
