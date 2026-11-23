//! What the window remembers between runs: where it was, how it is laid out,
//! the repositories opened before, and the settings the user chose.
//!
//! Everything lives in one versioned JSON file in the user's configuration
//! directory. A file that cannot be read is kept aside as `<file>.bad` and the
//! window opens with the defaults; a file written by an older version is read
//! as far as it goes and written back in the current shape.
//!
//! The model is plain data with the rules that go with it: what an analysis
//! runs with, how a repository setting is read from a text field, and where a
//! damaged file is copied to. Nothing here draws.

use std::path::{Path, PathBuf};

use blastr_model::IgnoreSet;
use blastr_model::{AnalysisOptions, AnalysisParams, Confidence, ExecMode, RankingParams};
use serde::{Deserialize, Serialize};

use crate::editor::Editor;

/// The file name inside the configuration directory.
const FILE_NAME: &str = "settings.json";

/// The file name the window used before its settings moved into this file.
const LEGACY_FILE_NAME: &str = "window.json";

/// The version of the settings file this build writes.
///
/// 1 was the window state alone, in `window.json`; 2 adds the editor, the
/// analysis and the per-repository settings; 3 leaves the shape alone and
/// records that the analysis depth default rose from 6 to 8, so that a file
/// version 2 wrote is read with the tuned default and a file this build writes
/// is not migrated again.
pub const VERSION: u32 = 3;

/// The depth every analysis ran with before the tuning in version 3.
///
/// Version 2 wrote the depth of every analysis to the file, whether the user
/// had chosen it or not, so this is the value that means "the old default".
const OLD_DEFAULT_DEPTH: u32 = 6;

/// How many recent repositories are remembered.
const MAX_RECENT: usize = 12;

/// Narrowest the side columns may be restored to.
const MIN_COLUMN: f32 = 240.0;

/// Widest the side columns may be restored to.
const MAX_COLUMN: f32 = 640.0;

/// The smallest window the application opens with.
const MIN_WINDOW: (f32, f32) = (900.0, 600.0);

/// The size of a window that was never placed.
const DEFAULT_WINDOW: (f32, f32) = (1280.0, 800.0);

/// The shortest evidence path an analysis may follow.
pub const MIN_DEPTH: u32 = 1;

/// The longest evidence path an analysis may follow.
pub const MAX_DEPTH: u32 = 64;

/// Where the window was and how large.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowPlacement {
    /// Left edge, in screen pixels.
    pub x: f32,
    /// Top edge, in screen pixels.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
    /// Whether the window filled the screen.
    pub maximized: bool,
}

/// A screen of the desktop, in pixels.
///
/// The usable part, which leaves out a task bar or a dock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl WindowPlacement {
    /// The placement made usable on these screens.
    ///
    /// A window restored from a file can be larger than the screen it is
    /// restored to, or somewhere that screen no longer is; without this, it
    /// opens off the desktop and looks like it was never restored at all.
    /// With no screen known, only the size is made sane.
    pub fn fitted(self, screens: &[ScreenRect]) -> WindowPlacement {
        let mut placement = self;
        if !placement.width.is_finite() || placement.width <= 0.0 {
            placement.width = DEFAULT_WINDOW.0;
        }
        if !placement.height.is_finite() || placement.height <= 0.0 {
            placement.height = DEFAULT_WINDOW.1;
        }
        placement.width = placement.width.max(MIN_WINDOW.0);
        placement.height = placement.height.max(MIN_WINDOW.1);
        if !placement.x.is_finite() {
            placement.x = 0.0;
        }
        if !placement.y.is_finite() {
            placement.y = 0.0;
        }
        let Some(screen) = pick_screen(&placement, screens) else {
            return placement;
        };
        if placement.width > screen.width {
            placement.width = screen.width;
            placement.x = screen.x;
        } else {
            placement.x = placement
                .x
                .clamp(screen.x, screen.x + screen.width - placement.width);
        }
        if placement.height > screen.height {
            placement.height = screen.height;
            placement.y = screen.y;
        } else {
            placement.y = placement
                .y
                .clamp(screen.y, screen.y + screen.height - placement.height);
        }
        placement
    }
}

/// The screen the window belongs to: the one holding its title bar, else the
/// one it overlaps most, else the first.
fn pick_screen(placement: &WindowPlacement, screens: &[ScreenRect]) -> Option<ScreenRect> {
    let title = (placement.x + 40.0, placement.y + 20.0);
    if let Some(screen) = screens.iter().find(|screen| {
        title.0 >= screen.x
            && title.0 <= screen.x + screen.width
            && title.1 >= screen.y
            && title.1 <= screen.y + screen.height
    }) {
        return Some(*screen);
    }
    let overlap = |screen: &ScreenRect| {
        let width = (placement.x + placement.width).min(screen.x + screen.width)
            - placement.x.max(screen.x);
        let height = (placement.y + placement.height).min(screen.y + screen.height)
            - placement.y.max(screen.y);
        width.max(0.0) * height.max(0.0)
    };
    screens
        .iter()
        .copied()
        .max_by(|left, right| overlap(left).total_cmp(&overlap(right)))
        .or_else(|| screens.first().copied())
}

/// The sizes of the resizable panes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Panes {
    /// Width of the left column.
    pub left: f32,
    /// Width of the right column.
    pub right: f32,
    /// Height of the commit list.
    pub commits: f32,
    /// Height of the source below the graph.
    pub code: f32,
}

impl Default for Panes {
    fn default() -> Self {
        Panes {
            left: 320.0,
            right: 344.0,
            commits: 138.0,
            code: 220.0,
        }
    }
}

impl Panes {
    /// The sizes with anything unusable replaced by a sensible value.
    pub fn sane(self) -> Panes {
        let defaults = Panes::default();
        let clamp = |value: f32, low: f32, high: f32, default: f32| {
            if value.is_finite() {
                value.clamp(low, high)
            } else {
                default
            }
        };
        Panes {
            left: clamp(self.left, MIN_COLUMN, MAX_COLUMN, defaults.left),
            right: clamp(self.right, MIN_COLUMN, MAX_COLUMN, defaults.right),
            commits: clamp(self.commits, 60.0, 600.0, defaults.commits),
            code: clamp(self.code, 160.0, 900.0, defaults.code),
        }
    }
}

/// A repository opened before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recent {
    /// The root of its working tree.
    pub path: PathBuf,
    /// The branch it was on when it was last open.
    #[serde(default)]
    pub branch: Option<String>,
    /// When it was last opened, in seconds since the Unix epoch.
    #[serde(default)]
    pub opened: i64,
}

/// The name a repository is shown under: the last component of its path.
pub fn repository_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The directory the user's configuration lives in.
///
/// Empty when it cannot be determined, which leaves the settings file in the
/// current directory; the window then starts from the defaults.
pub fn config_dir() -> PathBuf {
    blastr_model::paths()
        .map(|paths| paths.config_dir)
        .unwrap_or_default()
}

/// Which editor "open in editor" starts, and the command when it is the
/// user's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    /// The editor.
    pub editor: Editor,
    /// The command line of [`Editor::Custom`], with `{file}`, `{line}` and
    /// `{column}` placeholders.
    pub custom: String,
}

impl EditorSettings {
    /// Whether the custom command needs the user's attention.
    ///
    /// A command that cannot be used is not saved as a new requirement: the
    /// panel says what is missing and the previous editor keeps working.
    pub fn custom_problem(&self) -> Option<String> {
        if self.editor != Editor::Custom {
            return None;
        }
        crate::editor::custom_launch(&self.custom, Path::new("{file}"), 1, 1).err()
    }
}

/// The defaults every comparison runs with, unless a repository says
/// otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisSettings {
    /// How far the impact of a change is followed.
    pub depth: u32,
    /// The lowest confidence that is listed.
    pub min_confidence: Confidence,
    /// Whether a comparison of the working tree recomputes when a file is
    /// saved.
    pub refresh_on_save: bool,
}

impl Default for AnalysisSettings {
    fn default() -> Self {
        let defaults = AnalysisParams::default();
        AnalysisSettings {
            depth: defaults.depth,
            min_confidence: defaults.min_confidence,
            refresh_on_save: true,
        }
    }
}

impl AnalysisSettings {
    /// The settings with a depth outside the range brought back into it.
    pub fn sane(self) -> AnalysisSettings {
        AnalysisSettings {
            depth: self.depth.clamp(MIN_DEPTH, MAX_DEPTH),
            ..self
        }
    }

    /// The settings of a file written before version 3, read as version 3.
    ///
    /// Version 2 wrote the depth it ran with even when the user had never
    /// touched the setting, so the old default is the one value that cannot be
    /// told apart from "unset": it takes the tuned default. A depth the user
    /// chose is any other value, and is kept.
    fn tuned(self) -> AnalysisSettings {
        if self.depth == OLD_DEFAULT_DEPTH {
            AnalysisSettings {
                depth: AnalysisParams::default().depth,
                ..self
            }
        } else {
            self
        }
    }
}

/// What is analysed in one repository, and how far it may go.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepositorySettings {
    /// The root of the working tree.
    pub path: PathBuf,
    /// The Cargo features to enable; empty means the default features.
    pub features: Vec<String>,
    /// Whether every feature is enabled instead.
    pub all_features: bool,
    /// The target triple, when the host is not the one to analyse.
    pub target: Option<String>,
    /// Glob patterns of files whose items are never listed.
    pub ignore: Vec<String>,
    /// Whether build scripts and procedural macros are kept from running.
    pub restricted: bool,
}

impl RepositorySettings {
    /// The settings of a repository that was never opened before: every
    /// default, and restricted until the user says otherwise.
    pub fn new(path: &Path) -> RepositorySettings {
        RepositorySettings {
            path: path.to_path_buf(),
            ..RepositorySettings::default()
        }
    }

    /// What opening a file in this project runs with.
    pub fn options(&self, refresh_on_save: bool) -> AnalysisOptions {
        AnalysisOptions {
            features: self.features.clone(),
            all_features: self.all_features,
            target: self.target.clone(),
            exec: self.exec_mode(),
            manifest_dir: None,
            watch: refresh_on_save,
        }
    }

    /// How build scripts and procedural macros are treated here.
    pub fn exec_mode(&self) -> ExecMode {
        if self.restricted {
            ExecMode::Restricted
        } else {
            ExecMode::Full
        }
    }

    /// Whether the ignore patterns are usable, and what is wrong with them
    /// when they are not.
    pub fn ignore_problem(&self) -> Option<String> {
        IgnoreSet::new(&self.ignore)
            .err()
            .map(|error| error.to_string())
    }
}

/// The answers to the notice shown the first time a repository is opened, as
/// version 1 wrote them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyTrust {
    /// The root of the working tree.
    path: PathBuf,
    /// True when build scripts and procedural macros must not run.
    restricted: bool,
}

/// Everything the window remembers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The version of the file this was read from, and written back as.
    pub version: u32,
    /// Where the window was.
    pub window: Option<WindowPlacement>,
    /// The pane sizes.
    pub panes: Panes,
    /// Repositories opened before, most recent first.
    pub recent: Vec<Recent>,
    /// The repository that was open when the window closed.
    pub last_repository: Option<PathBuf>,
    /// Which editor opens a file.
    pub editor: EditorSettings,
    /// The analysis defaults.
    pub analysis: AnalysisSettings,
    /// What is analysed in each repository, by its root.
    pub repositories: Vec<RepositorySettings>,
    /// How each repository was answered when it was first opened, as version
    /// 1 stored it. Read once, then folded into `repositories`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    trust: Vec<LegacyTrust>,
    /// Whether the file this came from could not be read.
    ///
    /// A damaged file is kept aside before the next save, so that whatever it
    /// held is not lost to the defaults.
    #[serde(skip)]
    pub damaged: bool,
    /// Whether this was read from the file version 1 wrote, so that opening a
    /// repository, or the next save, writes the settings file in its place.
    #[serde(skip)]
    pub migrated: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: VERSION,
            window: None,
            panes: Panes::default(),
            recent: Vec::new(),
            last_repository: None,
            editor: EditorSettings::default(),
            analysis: AnalysisSettings::default(),
            repositories: Vec::new(),
            trust: Vec::new(),
            damaged: false,
            migrated: false,
        }
    }
}

impl Settings {
    /// The path of the settings file inside `config_dir`.
    pub fn path_in(config_dir: &Path) -> PathBuf {
        config_dir.join(FILE_NAME)
    }

    /// The file version 1 wrote, which sits beside this one.
    fn legacy_path(path: &Path) -> PathBuf {
        path.with_file_name(LEGACY_FILE_NAME)
    }

    /// Reads the settings; anything unreadable gives the defaults.
    ///
    /// A missing settings file is read from the `window.json` of version 1, so
    /// that the window opens where it closed and the repositories opened
    /// before are still listed. A file an older version wrote keeps the depth
    /// the user chose and reads the depth default of its day as the current
    /// one. The result says whether either happened, and whether the file was
    /// damaged, so that the next save can write the current shape and keep a
    /// copy of a damaged file as `<file>.bad`.
    pub fn load(path: &Path) -> Settings {
        let (text, legacy) = match std::fs::read_to_string(path) {
            Ok(text) => (text, false),
            Err(_) => match std::fs::read_to_string(Settings::legacy_path(path)) {
                Ok(text) => (text, true),
                Err(_) => return Settings::default(),
            },
        };
        let mut settings: Settings = match serde_json::from_str(&text) {
            Ok(settings) => settings,
            Err(_) => Settings {
                damaged: true,
                ..Settings::default()
            },
        };
        let written = settings.version;
        settings.version = VERSION;
        settings.panes = settings.panes.sane();
        settings.analysis = settings.analysis.sane();
        if written < VERSION {
            settings.analysis = settings.analysis.tuned();
        }
        let folded = settings.fold_trust();
        settings.migrated = legacy || folded || written < VERSION;
        settings
    }

    /// Folds the version-1 answers into the per-repository settings.
    fn fold_trust(&mut self) -> bool {
        if self.trust.is_empty() {
            return false;
        }
        let answers = std::mem::take(&mut self.trust);
        for answer in answers {
            let repository = self.repository_mut(&answer.path);
            repository.restricted = answer.restricted;
        }
        true
    }

    /// Writes the settings, creating the directory when needed.
    ///
    /// A file that could not be read is first copied aside, so that a damaged
    /// file is never silently replaced by the defaults.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if self.damaged {
            let backup = path.with_extension("json.bad");
            let _ = std::fs::copy(path, backup);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// Records that a repository was opened: it becomes the most recent one
    /// and the one to restore.
    pub fn opened(&mut self, path: &Path, branch: Option<String>, now: i64) {
        self.recent.retain(|recent| recent.path != path);
        self.recent.insert(
            0,
            Recent {
                path: path.to_path_buf(),
                branch,
                opened: now,
            },
        );
        self.recent.truncate(MAX_RECENT);
        self.last_repository = Some(path.to_path_buf());
    }

    /// What was analysed in the repository at `path`, when it was opened
    /// before.
    pub fn repository(&self, path: &Path) -> Option<&RepositorySettings> {
        self.repositories
            .iter()
            .find(|settings| settings.path == path)
    }

    /// What is analysed in the repository at `path`, adding an entry with the
    /// defaults when there is none.
    pub fn repository_mut(&mut self, path: &Path) -> &mut RepositorySettings {
        if let Some(index) = self
            .repositories
            .iter()
            .position(|settings| settings.path == path)
        {
            return &mut self.repositories[index];
        }
        self.repositories.push(RepositorySettings::new(path));
        self.repositories
            .last_mut()
            .expect("an entry was just added")
    }

    /// How the repository at `path` may be loaded, when the user was asked
    /// before.
    pub fn exec_mode(&self, path: &Path) -> Option<ExecMode> {
        self.repository(path).map(RepositorySettings::exec_mode)
    }

    /// Records how the repository at `path` may be loaded.
    pub fn set_exec_mode(&mut self, path: &Path, mode: ExecMode) {
        self.repository_mut(path).restricted = mode == ExecMode::Restricted;
    }

    /// The settings a comparison of the repository at `root` runs with.
    ///
    /// The analysis defaults are the user's, and the repository's own settings
    /// overrule them where it has any.
    pub fn analysis_for(&self, root: Option<&Path>) -> (AnalysisOptions, AnalysisParams) {
        let analysis = self.analysis.sane();
        let options = match root.and_then(|root| self.repository(root)) {
            Some(repository) => repository.options(analysis.refresh_on_save),
            None => AnalysisOptions {
                watch: analysis.refresh_on_save,
                ..AnalysisOptions::default()
            },
        };
        let params = AnalysisParams {
            ranking: RankingParams::default(),
            depth: analysis.depth,
            min_confidence: analysis.min_confidence,
            ignore: root
                .and_then(|root| self.repository(root))
                .map(|repository| repository.ignore.clone())
                .unwrap_or_default(),
        };
        (options, params)
    }
}

/// The features of a text field, split on commas and spaces.
///
/// `serde json, toml` and `serde,json,toml` are the same two features; an
/// empty field means the default features.
pub fn parse_features(text: &str) -> Vec<String> {
    let mut features: Vec<String> = Vec::new();
    for word in text.split([',', ' ', '\t', '\n', '\r']) {
        let word = word.trim();
        if word.is_empty() || features.iter().any(|feature| feature == word) {
            continue;
        }
        features.push(word.to_owned());
    }
    features
}

/// A feature list as the text field shows it.
pub fn format_features(features: &[String]) -> String {
    features.join(", ")
}

/// The glob patterns of a text area, one per line or separated by commas.
pub fn parse_globs(text: &str) -> Vec<String> {
    let mut patterns: Vec<String> = Vec::new();
    for line in text.split([',', '\n', '\r']) {
        let line = line.trim();
        if line.is_empty() || patterns.iter().any(|pattern| pattern == line) {
            continue;
        }
        patterns.push(line.to_owned());
    }
    patterns
}

/// Glob patterns as the text area shows them, one per line.
pub fn format_globs(patterns: &[String]) -> String {
    patterns.join("\n")
}

/// A depth typed into a text field, when it is one the analysis accepts.
pub fn parse_depth(text: &str) -> Option<u32> {
    let depth: u32 = text.trim().parse().ok()?;
    (MIN_DEPTH..=MAX_DEPTH).contains(&depth).then_some(depth)
}

/// A target triple typed into a text field; an empty field is the host.
pub fn parse_target(text: &str) -> Option<String> {
    let target = text.trim();
    (!target.is_empty()).then(|| target.to_owned())
}

/// A target triple as the text field shows it.
pub fn format_target(target: Option<&str>) -> String {
    target.unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_damaged_file_gives_the_defaults() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = Settings::path_in(directory.path());
        let missing = Settings::load(&path);
        assert_eq!(missing, Settings::default());
        assert!(!missing.damaged, "a missing file is not a damaged one");
        assert!(!missing.migrated);

        std::fs::write(&path, "{ not json").expect("the file to be written");
        let damaged = Settings::load(&path);
        assert!(damaged.damaged, "the caller must know it was replaced");
        assert_eq!(damaged.panes, Panes::default());
        assert!(damaged.recent.is_empty());
        assert_eq!(damaged.window, None);
        assert_eq!(
            damaged,
            Settings {
                damaged: true,
                ..Settings::default()
            }
        );
    }

    #[test]
    fn a_damaged_file_is_kept_before_the_defaults_are_saved() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = Settings::path_in(directory.path());
        std::fs::write(&path, "{ not json").expect("the file to be written");

        let settings = Settings::load(&path);
        settings.save(&path).expect("the settings to be saved");

        let backup = path.with_extension("json.bad");
        assert_eq!(
            std::fs::read_to_string(&backup).expect("the copy to be kept"),
            "{ not json"
        );
        let saved = Settings::load(&path);
        assert!(!saved.damaged);
        assert_eq!(saved, Settings::default());
    }

    #[test]
    fn a_save_over_a_good_file_keeps_no_copy() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = Settings::path_in(directory.path());

        Settings::default()
            .save(&path)
            .expect("the settings to be saved");

        assert!(!path.with_extension("json.bad").exists());
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = Settings::path_in(&directory.path().join("nested"));
        let mut settings = Settings {
            window: Some(WindowPlacement {
                x: 10.0,
                y: 20.0,
                width: 1440.0,
                height: 900.0,
                maximized: false,
            }),
            ..Settings::default()
        };
        settings.panes.left = 300.0;
        settings.opened(Path::new("/code/invoicing"), Some("main".to_owned()), 42);
        settings.set_exec_mode(Path::new("/code/invoicing"), ExecMode::Restricted);
        settings.analysis = AnalysisSettings {
            depth: 9,
            min_confidence: Confidence::Definite,
            refresh_on_save: false,
        };
        settings.editor = EditorSettings {
            editor: Editor::Custom,
            custom: "code -g {file}:{line}:{column}".to_owned(),
        };
        let repository = settings.repository_mut(Path::new("/code/invoicing"));
        repository.features = vec!["serde".to_owned()];
        repository.target = Some("x86_64-pc-windows-msvc".to_owned());
        repository.ignore = vec!["vendor/**".to_owned()];

        settings.save(&path).expect("the settings to be saved");

        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn the_file_is_written_with_its_version() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = Settings::path_in(directory.path());
        Settings::default()
            .save(&path)
            .expect("the settings to be saved");
        let text = std::fs::read_to_string(&path).expect("the file");
        assert!(text.contains("\"version\": 3"), "{text}");
    }

    #[test]
    fn the_depth_default_of_the_previous_version_becomes_the_tuned_one() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = Settings::path_in(directory.path());
        std::fs::write(
            &path,
            r#"{"version":2,"analysis":{"depth":6,"refresh_on_save":true}}"#,
        )
        .expect("the file to be written");

        let settings = Settings::load(&path);

        assert_eq!(settings.analysis.depth, AnalysisParams::default().depth);
        assert!(settings.migrated, "the caller must write the new file");

        settings.save(&path).expect("the settings to be saved");
        let saved = Settings::load(&path);
        assert!(!saved.migrated);
        assert_eq!(saved.analysis.depth, AnalysisParams::default().depth);
    }

    #[test]
    fn a_depth_the_user_chose_survives_the_tuning() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = Settings::path_in(directory.path());
        std::fs::write(&path, r#"{"version":2,"analysis":{"depth":5}}"#)
            .expect("the file to be written");
        assert_eq!(Settings::load(&path).analysis.depth, 5);

        // The old default chosen on purpose in this build is kept too: only a
        // file written before version 3 is migrated.
        let mut current = Settings::default();
        current.analysis.depth = OLD_DEFAULT_DEPTH;
        current.save(&path).expect("the settings to be saved");
        let saved = Settings::load(&path);
        assert_eq!(saved.analysis.depth, OLD_DEFAULT_DEPTH);
        assert!(!saved.migrated);
    }

    #[test]
    fn unknown_and_missing_keys_are_tolerated() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = Settings::path_in(directory.path());
        std::fs::write(
            &path,
            r#"{"panes":{"left":280.0},"later":true,"analysis":{"depth":7}}"#,
        )
        .expect("the file to be written");

        let settings = Settings::load(&path);

        assert!((settings.panes.left - 280.0).abs() < f32::EPSILON);
        assert!((settings.panes.right - Panes::default().right).abs() < f32::EPSILON);
        assert_eq!(settings.analysis.depth, 7);
        assert!(settings.analysis.refresh_on_save, "unset means on");
        assert_eq!(settings.version, VERSION);
    }

    #[test]
    fn an_unusable_depth_is_brought_back_into_range() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = Settings::path_in(directory.path());
        std::fs::write(&path, r#"{"analysis":{"depth":0},"version":2}"#)
            .expect("the file to be written");
        assert_eq!(Settings::load(&path).analysis.depth, MIN_DEPTH);

        std::fs::write(&path, r#"{"analysis":{"depth":900},"version":2}"#)
            .expect("the file to be written");
        assert_eq!(Settings::load(&path).analysis.depth, MAX_DEPTH);
    }

    #[test]
    fn the_window_state_of_the_old_file_is_taken_over() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let legacy = directory.path().join(LEGACY_FILE_NAME);
        std::fs::write(
            &legacy,
            r#"{
                "panes": {"left": 300.0},
                "recent": [{"path": "/code/app", "branch": "main", "opened": 7}],
                "last_repository": "/code/app",
                "trust": [{"path": "/code/app", "restricted": true}]
            }"#,
        )
        .expect("the old file to be written");

        let path = Settings::path_in(directory.path());
        let settings = Settings::load(&path);

        assert!(settings.migrated, "the caller must write the new file");
        assert!((settings.panes.left - 300.0).abs() < f32::EPSILON);
        assert_eq!(settings.recent.len(), 1);
        assert_eq!(
            settings.last_repository.as_deref(),
            Some(Path::new("/code/app"))
        );
        assert_eq!(
            settings.exec_mode(Path::new("/code/app")),
            Some(ExecMode::Restricted),
            "the answer to the notice survives"
        );

        settings.save(&path).expect("the settings to be saved");
        let saved = Settings::load(&path);
        assert!(!saved.migrated);
        assert_eq!(
            saved.exec_mode(Path::new("/code/app")),
            Some(ExecMode::Restricted)
        );
        assert_eq!(saved.recent.len(), 1);
    }

    #[test]
    fn unusable_pane_sizes_are_replaced() {
        let panes = Panes {
            left: 5.0,
            right: 9000.0,
            commits: f32::NAN,
            code: -3.0,
        }
        .sane();
        assert!((panes.left - MIN_COLUMN).abs() < f32::EPSILON);
        assert!((panes.right - MAX_COLUMN).abs() < f32::EPSILON);
        assert!((panes.commits - Panes::default().commits).abs() < f32::EPSILON);
        assert!((panes.code - 160.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_most_recent_repository_comes_first_without_duplicates() {
        let mut settings = Settings::default();
        settings.opened(Path::new("/a"), None, 1);
        settings.opened(Path::new("/b"), None, 2);
        settings.opened(Path::new("/a"), Some("dev".to_owned()), 3);

        let paths: Vec<&Path> = settings.recent.iter().map(|r| r.path.as_path()).collect();
        assert_eq!(paths, vec![Path::new("/a"), Path::new("/b")]);
        assert_eq!(settings.recent[0].branch.as_deref(), Some("dev"));
        assert_eq!(settings.last_repository.as_deref(), Some(Path::new("/a")));

        for index in 0..MAX_RECENT + 5 {
            settings.opened(Path::new(&format!("/r{index}")), None, 10);
        }
        assert_eq!(settings.recent.len(), MAX_RECENT);
    }

    #[test]
    fn the_trust_answer_is_remembered_per_repository() {
        let mut settings = Settings::default();
        assert_eq!(settings.exec_mode(Path::new("/a")), None);

        settings.set_exec_mode(Path::new("/a"), ExecMode::Full);
        settings.set_exec_mode(Path::new("/b"), ExecMode::Restricted);
        settings.set_exec_mode(Path::new("/a"), ExecMode::Restricted);

        assert_eq!(
            settings.exec_mode(Path::new("/a")),
            Some(ExecMode::Restricted)
        );
        assert_eq!(
            settings.exec_mode(Path::new("/b")),
            Some(ExecMode::Restricted)
        );
        assert_eq!(settings.repositories.len(), 2);
    }

    #[test]
    fn a_repository_that_is_not_configured_runs_with_the_defaults() {
        let mut settings = Settings::default();
        settings.analysis.depth = 8;
        settings.analysis.min_confidence = Confidence::Likely;

        let (options, params) = settings.analysis_for(Some(Path::new("/code/app")));
        assert_eq!(options, AnalysisOptions::default());
        assert_eq!(params.depth, 8);
        assert_eq!(params.min_confidence, Confidence::Likely);
        assert!(params.ignore.is_empty());
        assert_eq!(params.ranking, RankingParams::default());
        assert!(settings.repositories.is_empty(), "nothing was written");
    }

    #[test]
    fn a_repository_configures_its_own_analysis() {
        let mut settings = Settings::default();
        settings.analysis.refresh_on_save = false;
        let repository = settings.repository_mut(Path::new("/code/app"));
        repository.features = vec!["serde".to_owned()];
        repository.all_features = false;
        repository.target = Some("aarch64-apple-darwin".to_owned());
        repository.ignore = vec!["vendor/**".to_owned()];
        repository.restricted = true;

        let (options, params) = settings.analysis_for(Some(Path::new("/code/app")));
        assert_eq!(options.features, vec!["serde".to_owned()]);
        assert_eq!(options.target.as_deref(), Some("aarch64-apple-darwin"));
        assert_eq!(options.exec, ExecMode::Restricted);
        assert!(!options.watch, "refresh on save is off");
        assert_eq!(params.ignore, vec!["vendor/**".to_owned()]);
    }

    #[test]
    fn a_bad_ignore_pattern_is_reported_to_the_panel() {
        let mut repository = RepositorySettings::new(Path::new("/code/app"));
        assert_eq!(repository.ignore_problem(), None);
        repository.ignore = vec!["vendor/**".to_owned()];
        assert_eq!(repository.ignore_problem(), None);
        repository.ignore = vec!["a\\b".to_owned()];
        let problem = repository.ignore_problem().expect("a bad pattern");
        assert!(problem.contains("a\\b"), "{problem}");
    }

    #[test]
    fn a_feature_list_is_read_from_commas_and_spaces() {
        assert_eq!(
            parse_features("serde, json  toml\nserde"),
            vec!["serde".to_owned(), "json".to_owned(), "toml".to_owned()]
        );
        assert!(parse_features("   ").is_empty());
        assert_eq!(
            format_features(&["serde".to_owned(), "toml".to_owned()]),
            "serde, toml"
        );
    }

    #[test]
    fn glob_patterns_are_read_one_per_line() {
        assert_eq!(
            parse_globs("vendor/**\n*.gen.rs, target/**\n\nvendor/**"),
            vec![
                "vendor/**".to_owned(),
                "*.gen.rs".to_owned(),
                "target/**".to_owned()
            ]
        );
        assert!(parse_globs("").is_empty());
        assert_eq!(
            format_globs(&["vendor/**".to_owned(), "*.gen.rs".to_owned()]),
            "vendor/**\n*.gen.rs"
        );
    }

    #[test]
    fn a_depth_is_only_read_when_the_analysis_accepts_it() {
        assert_eq!(parse_depth("6"), Some(6));
        assert_eq!(parse_depth(" 1 "), Some(1));
        assert_eq!(parse_depth("0"), None);
        assert_eq!(parse_depth("65"), None);
        assert_eq!(parse_depth("six"), None);
        assert_eq!(parse_depth(""), None);
    }

    #[test]
    fn an_empty_target_means_the_host() {
        assert_eq!(parse_target("  "), None);
        assert_eq!(
            parse_target("x86_64-unknown-linux-gnu").as_deref(),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(format_target(None), "");
        assert_eq!(
            format_target(Some("wasm32-unknown-unknown")),
            "wasm32-unknown-unknown"
        );
    }

    #[test]
    fn a_custom_command_is_checked_before_it_is_used() {
        let mut editor = EditorSettings::default();
        assert_eq!(
            editor.custom_problem(),
            None,
            "the automatic editor is fine"
        );
        editor.editor = Editor::Custom;
        assert!(editor.custom_problem().is_some(), "an empty command");
        editor.custom = "code -g {file}:{line}".to_owned();
        assert_eq!(editor.custom_problem(), None);
        editor.custom = "code --new-window".to_owned();
        let problem = editor.custom_problem().expect("a command without a file");
        assert!(problem.contains("{file}"), "{problem}");
    }

    fn screen(x: f32, y: f32, width: f32, height: f32) -> ScreenRect {
        ScreenRect {
            x,
            y,
            width,
            height,
        }
    }

    fn placement(x: f32, y: f32, width: f32, height: f32) -> WindowPlacement {
        WindowPlacement {
            x,
            y,
            width,
            height,
            maximized: false,
        }
    }

    #[test]
    fn a_window_that_fits_is_left_where_it_was() {
        let fitted =
            placement(40.0, 40.0, 1440.0, 860.0).fitted(&[screen(0.0, 0.0, 1920.0, 1080.0)]);
        assert_eq!(fitted, placement(40.0, 40.0, 1440.0, 860.0));
    }

    #[test]
    fn a_window_taller_than_the_screen_is_shortened_and_moved_up() {
        let fitted =
            placement(48.0, 40.0, 1424.0, 1932.0).fitted(&[screen(0.0, 0.0, 1920.0, 1040.0)]);
        assert_eq!(fitted.height, 1040.0);
        assert_eq!(fitted.y, 0.0);
        assert_eq!(fitted.width, 1424.0);
        assert_eq!(fitted.x, 48.0);
    }

    #[test]
    fn a_window_off_the_desktop_is_pulled_onto_it() {
        let screens = [screen(0.0, 0.0, 1920.0, 1080.0)];
        let right = placement(3000.0, 200.0, 1200.0, 800.0).fitted(&screens);
        assert_eq!(right.x, 720.0);
        assert_eq!(right.y, 200.0);
        let above = placement(100.0, -400.0, 1200.0, 800.0).fitted(&screens);
        assert_eq!(above.y, 0.0);
    }

    #[test]
    fn the_screen_under_the_title_bar_wins() {
        let screens = [
            screen(0.0, 0.0, 1920.0, 1080.0),
            screen(1920.0, 0.0, 1920.0, 1080.0),
        ];
        let fitted = placement(2500.0, 100.0, 1000.0, 700.0).fitted(&screens);
        assert_eq!(fitted.x, 2500.0);
    }

    #[test]
    fn a_window_smaller_than_the_minimum_is_raised() {
        let fitted = placement(10.0, 10.0, 100.0, 80.0).fitted(&[screen(0.0, 0.0, 1920.0, 1080.0)]);
        assert_eq!((fitted.width, fitted.height), MIN_WINDOW);
    }

    #[test]
    fn without_a_screen_only_the_size_is_made_sane() {
        let fitted = placement(40.0, 40.0, 0.0, f32::NAN).fitted(&[]);
        assert_eq!(fitted.x, 40.0);
        assert_eq!(fitted.y, 40.0);
        assert_eq!(fitted.width, DEFAULT_WINDOW.0);
        assert_eq!(fitted.height, DEFAULT_WINDOW.1);
    }
}
