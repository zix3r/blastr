//! Where Blastr keeps its files for the current user.

use std::path::{Path, PathBuf};

/// The per-user directories: the cache and the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Cache directory: cached graphs and analyses, snapshots and build output.
    pub cache_dir: PathBuf,
    /// Configuration directory: the settings file.
    pub config_dir: PathBuf,
}

impl Paths {
    /// Both directories below `dir`: the cache in `dir/cache`, the
    /// configuration in `dir/config`.
    pub fn under(dir: &Path) -> Paths {
        Paths {
            cache_dir: dir.join("cache"),
            config_dir: dir.join("config"),
        }
    }

    /// Directory holding materialised revisions inside `cache_dir`.
    pub fn snapshots_dir(&self) -> PathBuf {
        self.cache_dir.join("snapshots")
    }

    /// Directory used as `CARGO_TARGET_DIR` for analysis builds.
    pub fn target_dir(&self) -> PathBuf {
        self.cache_dir.join("target")
    }
}

/// The per-user directories (`ProjectDirs::from("", "", "blastr")`), or
/// `None` when the user has no home directory. Does not create them.
pub fn paths() -> Option<Paths> {
    let dirs = directories::ProjectDirs::from("", "", "blastr")?;
    Some(Paths {
        cache_dir: dirs.cache_dir().to_path_buf(),
        config_dir: dirs.config_dir().to_path_buf(),
    })
}
