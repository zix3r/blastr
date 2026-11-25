//! The user's settings, kept as JSON in the configuration directory.
//!
//! Every field has a default, so a file written by another version reads as
//! far as it can. A file that does not parse is moved aside to `.bad` and the
//! defaults are used.

use std::path::{Path, PathBuf};

use blastr_model::{AnalysisOptions, AnalysisParams, Confidence, ExecMode, IgnoreSet};
use serde::{Deserialize, Serialize};

/// How many repositories the start screen remembers.
const MAX_RECENT: usize = 12;

/// The depths the impact can be followed to; the first,
/// [`AnalysisParams::UNLIMITED`], follows it to any distance.
pub const DEPTHS: std::ops::RangeInclusive<u32> = AnalysisParams::UNLIMITED..=64;

/// Where and how large the window was.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowPlacement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

/// The sizes of the resizable panes, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Panes {
    /// The left column.
    pub left: f32,
    /// The evidence column.
    pub right: f32,
    /// The code pane under the graph.
    pub code: f32,
}

impl Default for Panes {
    fn default() -> Self {
        Panes {
            left: 330.0,
            right: 350.0,
            code: 300.0,
        }
    }
}

/// A repository that was opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recent {
    pub path: PathBuf,
    #[serde(default)]
    pub branch: Option<String>,
}

/// How the analysis runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisSettings {
    /// How many steps the impact is followed; 0 follows it to any distance.
    pub depth: u32,
    /// The lowest confidence the lists and the graph show.
    pub min_confidence: Confidence,
    /// Whether a comparison of the working tree runs again on save.
    pub refresh_on_save: bool,
}

impl Default for AnalysisSettings {
    fn default() -> Self {
        AnalysisSettings {
            depth: AnalysisParams::default().depth,
            min_confidence: Confidence::Possible,
            refresh_on_save: true,
        }
    }
}

/// What one repository is analysed with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepositorySettings {
    pub path: PathBuf,
    pub features: Vec<String>,
    pub all_features: bool,
    pub target: Option<String>,
    pub ignore: Vec<String>,
    /// Whether build scripts and procedural macros are skipped; `None` until
    /// the user was asked.
    pub restricted: Option<bool>,
}

impl Default for RepositorySettings {
    /// Every feature on, so that code behind a feature is analysed too.
    fn default() -> Self {
        RepositorySettings {
            path: PathBuf::new(),
            features: Vec::new(),
            all_features: true,
            target: None,
            ignore: Vec::new(),
            restricted: None,
        }
    }
}

/// Everything the window remembers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub window: Option<WindowPlacement>,
    pub panes: Panes,
    pub recent: Vec<Recent>,
    /// The command that opens a file, with `{file}`, `{line}` and `{column}`;
    /// empty until an editor is found or chosen.
    pub editor: String,
    /// The code pane's text size in percent; 0 means 100.
    pub code_zoom: u16,
    pub analysis: AnalysisSettings,
    pub repositories: Vec<RepositorySettings>,
}

impl Settings {
    /// The settings file in the configuration directory.
    pub fn path() -> PathBuf {
        blastr_model::paths()
            .map(|paths| paths.config_dir)
            .unwrap_or_default()
            .join("settings.json")
    }

    /// Reads the settings, moving a file that does not parse aside.
    pub fn load(path: &Path) -> Settings {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Settings::default();
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(mut settings) => {
                settings.analysis.depth = settings
                    .analysis
                    .depth
                    .clamp(*DEPTHS.start(), *DEPTHS.end());
                settings
            }
            Err(_) => {
                let _ = std::fs::rename(path, path.with_extension("json.bad"));
                Settings::default()
            }
        }
    }

    /// Writes the settings.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// Remembers that the repository at `path` was opened.
    pub fn opened(&mut self, path: &Path, branch: Option<String>) {
        self.recent.retain(|recent| recent.path != path);
        self.recent.insert(
            0,
            Recent {
                path: path.to_path_buf(),
                branch,
            },
        );
        self.recent.truncate(MAX_RECENT);
    }

    /// The settings of one repository, when it has any.
    pub fn repository(&self, path: &Path) -> Option<&RepositorySettings> {
        self.repositories.iter().find(|entry| entry.path == path)
    }

    /// The settings of one repository as they apply now: its own, or the
    /// defaults until it has any.
    pub fn repository_or_default(&self, path: &Path) -> RepositorySettings {
        self.repository(path)
            .cloned()
            .unwrap_or_else(|| RepositorySettings {
                path: path.to_path_buf(),
                ..RepositorySettings::default()
            })
    }

    /// The settings of one repository, created on first use.
    pub fn repository_mut(&mut self, path: &Path) -> &mut RepositorySettings {
        match self
            .repositories
            .iter()
            .position(|entry| entry.path == path)
        {
            Some(index) => &mut self.repositories[index],
            None => {
                self.repositories.push(RepositorySettings {
                    path: path.to_path_buf(),
                    ..RepositorySettings::default()
                });
                self.repositories
                    .last_mut()
                    .expect("an entry was just added")
            }
        }
    }

    /// How build scripts run in the repository at `root`, once the user said.
    pub fn exec_mode(&self, root: &Path) -> Option<ExecMode> {
        let restricted = self.repository(root)?.restricted?;
        Some(if restricted {
            ExecMode::Restricted
        } else {
            ExecMode::Full
        })
    }

    /// What a comparison of the repository at `root` is run with.
    pub fn analysis_for(&self, root: &Path) -> (AnalysisOptions, AnalysisParams) {
        let repository = self.repository_or_default(root);
        let options = AnalysisOptions {
            features: repository.features,
            all_features: repository.all_features,
            target: repository.target,
            exec: self.exec_mode(root).unwrap_or(ExecMode::Full),
            manifest_dir: None,
            watch: self.analysis.refresh_on_save,
        };
        // The lists filter by confidence themselves, so the engine lists all.
        let params = AnalysisParams {
            depth: self.analysis.depth,
            min_confidence: Confidence::Possible,
            ignore: repository.ignore,
        };
        (options, params)
    }
}

/// The words of a list typed as text: separated by commas or white space for
/// features, by commas or lines for patterns.
pub fn parse_list(text: &str, separators: &[char]) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in text.split(separators).map(str::trim) {
        if !word.is_empty() && !words.iter().any(|known| known == word) {
            words.push(word.to_owned());
        }
    }
    words
}

/// Why a list of ignore patterns cannot be used.
pub fn ignore_problem(patterns: &[String]) -> Option<String> {
    IgnoreSet::new(patterns)
        .err()
        .map(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let directory = tempfile::tempdir().expect("a directory");
        let settings = Settings::load(&directory.path().join("settings.json"));
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.analysis.depth, AnalysisParams::UNLIMITED);
        assert_eq!(settings.analysis.min_confidence, Confidence::Possible);
        assert!(settings.analysis.refresh_on_save);
        assert!(settings.editor.is_empty());
        let (options, params) = settings.analysis_for(Path::new("/code/a"));
        assert!(options.all_features);
        assert!(options.watch);
        assert_eq!(params.step_limit(), u32::MAX);
    }

    #[test]
    fn a_new_repository_entry_and_a_partial_one_have_every_feature_on() {
        let mut settings = Settings::default();
        assert!(settings.repository_mut(Path::new("/code/a")).all_features);
        let directory = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, r#"{ "repositories": [{ "path": "/code/b" }] }"#).expect("a file");
        let settings = Settings::load(&path);
        assert!(
            settings
                .repository_or_default(Path::new("/code/b"))
                .all_features
        );
    }

    #[test]
    fn depth_zero_survives_loading_and_a_large_one_is_capped() {
        let directory = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, r#"{ "analysis": { "depth": 0 } }"#).expect("a file");
        assert_eq!(Settings::load(&path).analysis.depth, 0);
        std::fs::write(&path, r#"{ "analysis": { "depth": 900 } }"#).expect("a file");
        assert_eq!(Settings::load(&path).analysis.depth, *DEPTHS.end());
    }

    #[test]
    fn a_damaged_file_is_moved_aside() {
        let directory = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, "{ not json").expect("a file");
        assert_eq!(Settings::load(&path), Settings::default());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.bad")).expect("the old file"),
            "{ not json"
        );
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let directory = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("settings.json");
        std::fs::write(&path, r#"{ "analysis": { "depth": 3 }, "unknown": 1 }"#).expect("a file");
        let settings = Settings::load(&path);
        assert_eq!(settings.analysis.depth, 3);
        assert!(settings.analysis.refresh_on_save);
        assert_eq!(settings.panes, Panes::default());
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let directory = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("nested").join("settings.json");
        let mut settings = Settings {
            editor: "code -g {file}:{line}:{column}".to_owned(),
            ..Settings::default()
        };
        settings.opened(Path::new("/code/a"), Some("main".to_owned()));
        let repository = settings.repository_mut(Path::new("/code/a"));
        repository.features = vec!["serde".to_owned()];
        repository.restricted = Some(true);
        settings.save(&path).expect("saved");
        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn a_repository_is_asked_about_once_and_analysed_as_answered() {
        let mut settings = Settings::default();
        let root = Path::new("/code/a");
        assert_eq!(settings.exec_mode(root), None);
        settings.repository_mut(root).restricted = Some(true);
        settings.repository_mut(root).ignore = vec!["vendor/**".to_owned()];
        assert_eq!(settings.exec_mode(root), Some(ExecMode::Restricted));
        let (options, params) = settings.analysis_for(root);
        assert_eq!(options.exec, ExecMode::Restricted);
        assert_eq!(params.ignore, vec!["vendor/**"]);
        assert_eq!(params.min_confidence, Confidence::Possible);
    }

    #[test]
    fn lists_are_split_and_deduplicated() {
        assert_eq!(
            parse_list("a, b  a\nc", &[',', ' ', '\n']),
            vec!["a", "b", "c"]
        );
        assert!(parse_list(" , ", &[',']).is_empty());
    }
}
