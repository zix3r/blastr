//! Configuration files and the ignore set.
//!
//! A configuration is read from `<repo_root>/blastr.toml` on top of
//! `<config_dir>/config.toml`. Parsing walks the TOML table by hand so that a
//! mistyped key is reported with the key name, a suggestion and the file it
//! came from. The same file holds [`IgnoreSet`], the compiled glob patterns
//! that keep ignored files out of an analysis.

use std::fs;
use std::path::{Path, PathBuf};

use blastr_model::{AnalysisParams, Confidence, RankingParams, RelPath};

/// The keys a configuration file may contain at the top level.
static TOP_LEVEL_KEYS: [&str; 6] = [
    "features",
    "depth",
    "min_confidence",
    "ignore",
    "editor",
    "ranking",
];

/// The keys the `ranking` table may contain.
static RANKING_KEYS: [&str; 5] = [
    "decay",
    "candidate_weight",
    "adaptation_weight",
    "behaviour_weight",
    "score_floor",
];

/// Everything that can go wrong while reading a configuration file.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read `{}`: {message}", path.display())]
    Read {
        /// The file that was being read.
        path: PathBuf,
        /// The underlying I/O message.
        message: String,
    },
    /// The file is not valid TOML.
    #[error("syntax error in `{}`: {message}", path.display())]
    Syntax {
        /// The file that was being parsed.
        path: PathBuf,
        /// The parser's message, with its line and column.
        message: String,
    },
    /// The file contains a key Blastr does not know.
    #[error("unknown key `{key}` in `{}`", path.display())]
    UnknownKey {
        /// The file the key was found in.
        path: PathBuf,
        /// The key, dotted for keys inside a table (`ranking.decy`).
        key: String,
        /// The closest valid key at the same level, when it is close enough.
        suggestion: Option<String>,
    },
    /// A key holds a value that is not acceptable.
    #[error("invalid value for `{key}` in `{}`: {message}", path.display())]
    InvalidValue {
        /// The file the value was found in.
        path: PathBuf,
        /// The key, dotted for keys inside a table (`ranking.decay`).
        key: String,
        /// What is wrong with the value.
        message: String,
    },
    /// An ignore pattern is not a usable glob.
    #[error("bad ignore pattern `{pattern}`: {message}")]
    BadPattern {
        /// The pattern as written in the file.
        pattern: String,
        /// What is wrong with it.
        message: String,
    },
}

impl ConfigError {
    /// One line telling the user what to do next, when there is something useful to say.
    pub fn hint(&self) -> Option<String> {
        match self {
            ConfigError::UnknownKey {
                key, suggestion, ..
            } => Some(match suggestion {
                Some(suggestion) => format!("did you mean `{suggestion}`?"),
                None => format!("valid keys: {}", valid_keys_for(key)?.join(", ")),
            }),
            ConfigError::InvalidValue { key, .. } => expected_value(key),
            ConfigError::Syntax { .. } => {
                Some("check the TOML syntax; the message gives the line and column".to_owned())
            }
            ConfigError::Read { .. } => {
                Some("check that the file exists and that Blastr may read it".to_owned())
            }
            ConfigError::BadPattern { .. } => Some(
                "a pattern matches repository paths: `*` and `?` stay within one segment, `**` covers whole segments"
                    .to_owned(),
            ),
        }
    }
}

/// The keys that are valid at the level an unknown key was found at.
fn valid_keys_for(key: &str) -> Option<&'static [&'static str]> {
    match key.rsplit_once('.') {
        None => Some(TOP_LEVEL_KEYS.as_slice()),
        Some(("ranking", _)) => Some(RANKING_KEYS.as_slice()),
        Some(_) => None,
    }
}

/// What a key accepts, for the hint printed after an invalid value.
fn expected_value(key: &str) -> Option<String> {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    let text = match leaf {
        "features" | "ignore" => "an array of strings".to_owned(),
        "depth" => "a whole number from 1 to 64".to_owned(),
        "min_confidence" => format!("one of {}", confidence_names()),
        "editor" => "a string".to_owned(),
        "ranking" => "a table, written as `[ranking]`".to_owned(),
        "decay" | "candidate_weight" => "a number above 0 and at most 1".to_owned(),
        "adaptation_weight" | "behaviour_weight" | "score_floor" => {
            "a number from 0 to 1".to_owned()
        }
        _ => return None,
    };
    Some(text)
}

/// The accepted confidence names, quoted, in a list.
fn confidence_names() -> String {
    Confidence::ALL
        .iter()
        .map(|confidence| format!("`{}`", confidence.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A configuration file's contents. Every key is optional, so that a file
/// only states what it wants to change.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    /// Cargo features to enable while loading the project.
    pub features: Option<Vec<String>>,
    /// Longest evidence path to follow.
    pub depth: Option<u32>,
    /// Lowest confidence to list.
    pub min_confidence: Option<Confidence>,
    /// Glob patterns of files whose items are never listed.
    pub ignore: Option<Vec<String>>,
    /// The command that opens a file at a line, with `{file}` and `{line}`.
    pub editor: Option<String>,
    /// Scoring weights set by this file.
    pub ranking: RankingOverrides,
}

/// The fields of [`RankingParams`] a configuration file may override.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RankingOverrides {
    /// Multiplier applied for each step after the first.
    pub decay: Option<f32>,
    /// Weight of a candidate step.
    pub candidate_weight: Option<f32>,
    /// Weight of the `Adaptation` class.
    pub adaptation_weight: Option<f32>,
    /// Weight of the `Behaviour` class.
    pub behaviour_weight: Option<f32>,
    /// Items scoring below this are counted, not listed.
    pub score_floor: Option<f32>,
}

impl Config {
    /// Parses a configuration; `origin` is only used to name the file in errors.
    pub fn parse(text: &str, origin: &Path) -> Result<Config, ConfigError> {
        let table = text
            .parse::<toml::Table>()
            .map_err(|error| ConfigError::Syntax {
                path: origin.to_path_buf(),
                message: error.to_string(),
            })?;

        let mut features = None;
        let mut depth_limit = None;
        let mut min_confidence = None;
        let mut ignore = None;
        let mut editor = None;
        let mut ranking_overrides = RankingOverrides::default();

        for (key, value) in &table {
            match key.as_str() {
                "features" => features = Some(string_array(value, origin, "features")?),
                "depth" => depth_limit = Some(depth(value, origin)?),
                "min_confidence" => {
                    min_confidence = Some(confidence(value, origin, "min_confidence")?);
                }
                "ignore" => {
                    let patterns = string_array(value, origin, "ignore")?;
                    IgnoreSet::new(&patterns)?;
                    ignore = Some(patterns);
                }
                "editor" => editor = Some(string(value, origin, "editor")?),
                "ranking" => ranking_overrides = ranking(value, origin)?,
                other => return Err(unknown_key(origin, other, &TOP_LEVEL_KEYS)),
            }
        }
        Ok(Config {
            features,
            depth: depth_limit,
            min_confidence,
            ignore,
            editor,
            ranking: ranking_overrides,
        })
    }

    /// Reads one configuration file. `None` when the file does not exist.
    pub fn load_file(path: &Path) -> Result<Option<Config>, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => Config::parse(&text, path).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                message: error.to_string(),
            }),
        }
    }

    /// Reads `<repo_root>/blastr.toml` over `<config_dir>/config.toml`.
    pub fn load(repo_root: &Path, config_dir: &Path) -> Result<Config, ConfigError> {
        let project = Config::load_file(&repo_root.join("blastr.toml"))?.unwrap_or_default();
        let user = Config::load_file(&config_dir.join("config.toml"))?.unwrap_or_default();
        Ok(project.over(user))
    }

    /// Every value `self` sets, and `lower`'s where it does not.
    pub fn over(self, lower: Config) -> Config {
        Config {
            features: self.features.or(lower.features),
            depth: self.depth.or(lower.depth),
            min_confidence: self.min_confidence.or(lower.min_confidence),
            ignore: self.ignore.or(lower.ignore),
            editor: self.editor.or(lower.editor),
            ranking: RankingOverrides {
                decay: self.ranking.decay.or(lower.ranking.decay),
                candidate_weight: self
                    .ranking
                    .candidate_weight
                    .or(lower.ranking.candidate_weight),
                adaptation_weight: self
                    .ranking
                    .adaptation_weight
                    .or(lower.ranking.adaptation_weight),
                behaviour_weight: self
                    .ranking
                    .behaviour_weight
                    .or(lower.ranking.behaviour_weight),
                score_floor: self.ranking.score_floor.or(lower.ranking.score_floor),
            },
        }
    }

    /// The analysis defaults with this configuration's values applied.
    pub fn params(&self) -> AnalysisParams {
        let defaults = AnalysisParams::default();
        let ranking = RankingParams::default();
        AnalysisParams {
            ranking: RankingParams {
                decay: self.ranking.decay.unwrap_or(ranking.decay),
                candidate_weight: self
                    .ranking
                    .candidate_weight
                    .unwrap_or(ranking.candidate_weight),
                adaptation_weight: self
                    .ranking
                    .adaptation_weight
                    .unwrap_or(ranking.adaptation_weight),
                behaviour_weight: self
                    .ranking
                    .behaviour_weight
                    .unwrap_or(ranking.behaviour_weight),
                score_floor: self.ranking.score_floor.unwrap_or(ranking.score_floor),
            },
            depth: self.depth.unwrap_or(defaults.depth),
            min_confidence: self.min_confidence.unwrap_or(defaults.min_confidence),
            ignore: self.ignore.clone().unwrap_or(defaults.ignore),
        }
    }
}

/// Reads the `ranking` table.
fn ranking(value: &toml::Value, origin: &Path) -> Result<RankingOverrides, ConfigError> {
    let Some(table) = value.as_table() else {
        return Err(invalid_value(
            origin,
            "ranking",
            format!(
                "expected a table, found a value of type {}",
                value.type_str()
            ),
        ));
    };

    let mut decay = None;
    let mut candidate_weight = None;
    let mut adaptation_weight = None;
    let mut behaviour_weight = None;
    let mut score_floor = None;

    for (key, value) in table {
        let dotted = format!("ranking.{key}");
        match key.as_str() {
            "decay" => decay = Some(weight(value, origin, &dotted, true)?),
            "candidate_weight" => {
                candidate_weight = Some(weight(value, origin, &dotted, true)?);
            }
            "adaptation_weight" => {
                adaptation_weight = Some(weight(value, origin, &dotted, false)?);
            }
            "behaviour_weight" => {
                behaviour_weight = Some(weight(value, origin, &dotted, false)?);
            }
            "score_floor" => score_floor = Some(weight(value, origin, &dotted, false)?),
            _ => return Err(unknown_key(origin, &dotted, &RANKING_KEYS)),
        }
    }
    Ok(RankingOverrides {
        decay,
        candidate_weight,
        adaptation_weight,
        behaviour_weight,
        score_floor,
    })
}

/// Reads a string.
fn string(value: &toml::Value, origin: &Path, key: &str) -> Result<String, ConfigError> {
    match value.as_str() {
        Some(text) => Ok(text.to_owned()),
        None => Err(invalid_value(
            origin,
            key,
            format!(
                "expected a string, found a value of type {}",
                value.type_str()
            ),
        )),
    }
}

/// Reads an array of strings.
fn string_array(value: &toml::Value, origin: &Path, key: &str) -> Result<Vec<String>, ConfigError> {
    let Some(items) = value.as_array() else {
        return Err(invalid_value(
            origin,
            key,
            format!(
                "expected an array of strings, found a value of type {}",
                value.type_str()
            ),
        ));
    };

    let mut strings = Vec::with_capacity(items.len());
    for item in items {
        match item.as_str() {
            Some(text) => strings.push(text.to_owned()),
            None => {
                return Err(invalid_value(
                    origin,
                    key,
                    format!(
                        "expected an array of strings, but it contains a value of type {}",
                        item.type_str()
                    ),
                ));
            }
        }
    }
    Ok(strings)
}

/// Reads an integer.
fn integer(value: &toml::Value, origin: &Path, key: &str) -> Result<i64, ConfigError> {
    match value.as_integer() {
        Some(number) => Ok(number),
        None => Err(invalid_value(
            origin,
            key,
            format!(
                "expected an integer, found a value of type {}",
                value.type_str()
            ),
        )),
    }
}

/// Reads the `depth` key, which must allow at least one and at most 64 steps.
fn depth(value: &toml::Value, origin: &Path) -> Result<u32, ConfigError> {
    let depth = integer(value, origin, "depth")?;
    if !(1..=64).contains(&depth) {
        return Err(invalid_value(
            origin,
            "depth",
            format!("must be between 1 and 64, found {depth}"),
        ));
    }
    Ok(depth as u32)
}

/// Reads the `min_confidence` key.
fn confidence(value: &toml::Value, origin: &Path, key: &str) -> Result<Confidence, ConfigError> {
    let Some(text) = value.as_str() else {
        return Err(invalid_value(
            origin,
            key,
            format!(
                "expected a string, found a value of type {}",
                value.type_str()
            ),
        ));
    };

    match Confidence::ALL
        .iter()
        .copied()
        .find(|confidence| confidence.as_str() == text)
    {
        Some(confidence) => Ok(confidence),
        None => Err(invalid_value(
            origin,
            key,
            format!("expected one of {}, found `{text}`", confidence_names()),
        )),
    }
}

/// Reads a weight; `strict` additionally rejects zero.
fn weight(value: &toml::Value, origin: &Path, key: &str, strict: bool) -> Result<f32, ConfigError> {
    let number = match value {
        toml::Value::Integer(integer) => *integer as f64,
        toml::Value::Float(float) => *float,
        _ => {
            return Err(invalid_value(
                origin,
                key,
                format!(
                    "expected a number, found a value of type {}",
                    value.type_str()
                ),
            ));
        }
    };

    let valid = if strict {
        number.is_finite() && number > 0.0 && number <= 1.0
    } else {
        (0.0..=1.0).contains(&number)
    };
    if !valid {
        let accepted = if strict {
            "must be above 0.0 and at most 1.0"
        } else {
            "must be between 0.0 and 1.0"
        };
        return Err(invalid_value(
            origin,
            key,
            format!("{accepted}, found {number}"),
        ));
    }
    Ok(number as f32)
}

/// Builds the error for a key that is not in `valid`.
fn unknown_key(origin: &Path, key: &str, valid: &[&'static str]) -> ConfigError {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    ConfigError::UnknownKey {
        path: origin.to_path_buf(),
        key: key.to_owned(),
        suggestion: closest_key(leaf, valid).map(|suggestion| suggestion.to_owned()),
    }
}

/// Builds the error for a value that is not acceptable.
fn invalid_value(origin: &Path, key: &str, message: String) -> ConfigError {
    ConfigError::InvalidValue {
        path: origin.to_path_buf(),
        key: key.to_owned(),
        message,
    }
}

/// The valid key closest to `word`, when its edit distance is at most three.
fn closest_key(word: &str, valid: &[&'static str]) -> Option<&'static str> {
    let mut best: Option<(usize, &'static str)> = None;
    for &candidate in valid {
        let distance = edit_distance(word, candidate);
        if distance > 3 {
            continue;
        }
        match best {
            Some((best_distance, _)) if best_distance <= distance => {}
            _ => best = Some((distance, candidate)),
        }
    }
    best.map(|(_, candidate)| candidate)
}

/// The number of single-character insertions, deletions and substitutions
/// that turn `a` into `b`.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];

    for (index, left) in a.iter().enumerate() {
        current[0] = index + 1;
        for (other, right) in b.iter().enumerate() {
            let substitution = previous[other] + usize::from(left != right);
            let insertion = current[other] + 1;
            let deletion = previous[other + 1] + 1;
            current[other + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// The compiled glob patterns of files whose items an analysis never lists.
///
/// Patterns match repository-relative paths with `/` separators. `*` matches
/// within one segment, `**` covers any number of segments, `?` is one
/// character, and a pattern without a `/` matches a file name in any directory.
#[derive(Debug, Clone, Default)]
pub struct IgnoreSet {
    patterns: Vec<Pattern>,
}

impl IgnoreSet {
    /// An ignore set that ignores nothing.
    pub fn empty() -> IgnoreSet {
        IgnoreSet::default()
    }

    /// Compiles `patterns`, rejecting the ones that are not usable globs.
    pub fn new(patterns: &[String]) -> Result<IgnoreSet, ConfigError> {
        let mut compiled = Vec::with_capacity(patterns.len());
        for pattern in patterns {
            compiled.push(Pattern::compile(pattern)?);
        }
        Ok(IgnoreSet { patterns: compiled })
    }

    /// Whether `path` matches any of the patterns.
    pub fn is_ignored(&self, path: &RelPath) -> bool {
        self.patterns
            .iter()
            .any(|pattern| pattern.matches(path.as_str()))
    }
}

/// One compiled ignore pattern.
#[derive(Debug, Clone)]
struct Pattern {
    /// The path segments of the pattern, in order.
    segments: Vec<Segment>,
}

/// One segment of a compiled pattern.
#[derive(Debug, Clone)]
enum Segment {
    /// `**`, matching any number of path segments.
    AnySegments,
    /// A segment made of literal characters and single-segment wildcards.
    Tokens(Vec<Token>),
}

/// One element of a segment: a literal character or a wildcard.
#[derive(Debug, Clone)]
enum Token {
    /// A character that must appear as written.
    Char(char),
    /// `*`, matching any run of characters inside one segment.
    Star,
    /// `?`, matching exactly one character.
    Question,
}

impl Pattern {
    /// Compiles one pattern, rejecting empty patterns and misplaced `**`.
    fn compile(pattern: &str) -> Result<Pattern, ConfigError> {
        if pattern.is_empty() {
            return Err(ConfigError::BadPattern {
                pattern: pattern.to_owned(),
                message: "the pattern is empty".to_owned(),
            });
        }
        if pattern.contains('\\') {
            return Err(ConfigError::BadPattern {
                pattern: pattern.to_owned(),
                message: "use `/` to separate path segments".to_owned(),
            });
        }

        let mut segments = Vec::new();
        if !pattern.contains('/') {
            // A bare file name matches in any directory.
            segments.push(Segment::AnySegments);
        }
        for text in pattern.split('/') {
            if text == "**" {
                segments.push(Segment::AnySegments);
            } else if text.contains("**") {
                return Err(ConfigError::BadPattern {
                    pattern: pattern.to_owned(),
                    message: "`**` must be a whole path segment".to_owned(),
                });
            } else {
                segments.push(Segment::Tokens(tokens(text)));
            }
        }
        Ok(Pattern { segments })
    }

    /// Whether the whole `path` matches this pattern.
    fn matches(&self, path: &str) -> bool {
        let segments: Vec<&str> = path.split('/').collect();
        self.matches_from(0, &segments)
    }

    /// Whether the segments from `index` on match the remaining path segments.
    fn matches_from(&self, index: usize, path: &[&str]) -> bool {
        match self.segments.get(index) {
            None => path.is_empty(),
            Some(Segment::AnySegments) => {
                if self.matches_from(index + 1, path) {
                    return true;
                }
                match path.split_first() {
                    Some((_, rest)) => self.matches_from(index, rest),
                    None => false,
                }
            }
            Some(Segment::Tokens(tokens)) => match path.split_first() {
                Some((segment, rest)) => {
                    let characters: Vec<char> = segment.chars().collect();
                    tokens_match(tokens, &characters) && self.matches_from(index + 1, rest)
                }
                None => false,
            },
        }
    }
}

/// Splits a segment into literal characters and wildcards.
fn tokens(text: &str) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(text.chars().count());
    for character in text.chars() {
        match character {
            '*' => {
                if !matches!(tokens.last(), Some(Token::Star)) {
                    tokens.push(Token::Star);
                }
            }
            '?' => tokens.push(Token::Question),
            other => tokens.push(Token::Char(other)),
        }
    }
    tokens
}

/// Whether `text` matches the whole of `tokens`.
fn tokens_match(tokens: &[Token], text: &[char]) -> bool {
    match tokens.split_first() {
        None => text.is_empty(),
        Some((Token::Star, rest)) => {
            if tokens_match(rest, text) {
                return true;
            }
            match text.split_first() {
                Some((_, tail)) => tokens_match(tokens, tail),
                None => false,
            }
        }
        Some((Token::Question, rest)) => match text.split_first() {
            Some((_, tail)) => tokens_match(rest, tail),
            None => false,
        },
        Some((Token::Char(expected), rest)) => match text.split_first() {
            Some((character, tail)) => character == expected && tokens_match(rest, tail),
            None => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// The configuration file the tests parse.
    const FULL: &str = r#"
features = ["default"]
depth = 6
min_confidence = "possible"
ignore = ["vendor/**", "**/generated/**"]
editor = "zed {file}:{line}"

[ranking]
decay = 0.6
candidate_weight = 0.4
adaptation_weight = 1.0
behaviour_weight = 0.8
score_floor = 0.05
"#;

    /// Parses `text` as a configuration file in the current directory.
    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::parse(text, Path::new("blastr.toml"))
    }

    /// Parses `text`, expecting an error.
    fn error(text: &str) -> ConfigError {
        parse(text).expect_err("the text must not parse")
    }

    /// The key an invalid value or unknown key error is about.
    fn key_of(error: &ConfigError) -> &str {
        match error {
            ConfigError::UnknownKey { key, .. } | ConfigError::InvalidValue { key, .. } => {
                key.as_str()
            }
            other => panic!("expected a key error, found {other:?}"),
        }
    }

    /// Whether `pattern` ignores `path`.
    fn ignored(pattern: &str, path: &str) -> bool {
        let set = IgnoreSet::new(&[pattern.to_owned()]).expect("the pattern must compile");
        set.is_ignored(&RelPath::new(path))
    }

    #[test]
    fn every_key_parses() {
        let config = parse(FULL).expect("the example must parse");
        assert_eq!(config.features, Some(vec!["default".to_owned()]));
        assert_eq!(config.depth, Some(6));
        assert_eq!(config.min_confidence, Some(Confidence::Possible));
        assert_eq!(
            config.ignore,
            Some(vec!["vendor/**".to_owned(), "**/generated/**".to_owned()])
        );
        assert_eq!(config.editor, Some("zed {file}:{line}".to_owned()));
        assert_eq!(config.ranking.decay, Some(0.6));
        assert_eq!(config.ranking.candidate_weight, Some(0.4));
        assert_eq!(config.ranking.adaptation_weight, Some(1.0));
        assert_eq!(config.ranking.behaviour_weight, Some(0.8));
        assert_eq!(config.ranking.score_floor, Some(0.05));
    }

    #[test]
    fn an_empty_file_gives_default() {
        assert_eq!(
            parse("").expect("an empty file must parse"),
            Config::default()
        );
        assert_eq!(
            parse("# only a comment\n").expect("a comment must parse"),
            Config::default()
        );
    }

    #[test]
    fn integers_are_accepted_for_weights() {
        let config = parse("[ranking]\ndecay = 1\ncandidate_weight = 1\nscore_floor = 0")
            .expect("whole numbers must parse");
        assert_eq!(config.ranking.decay, Some(1.0));
        assert_eq!(config.ranking.candidate_weight, Some(1.0));
        assert_eq!(config.ranking.score_floor, Some(0.0));
        assert_eq!(config.ranking.adaptation_weight, None);
    }

    #[test]
    fn an_unknown_top_level_key_suggests_the_closest_key() {
        let config_error = error("dept = 3\n");
        assert_eq!(key_of(&config_error), "dept");
        match &config_error {
            ConfigError::UnknownKey { suggestion, .. } => {
                assert_eq!(suggestion.as_deref(), Some("depth"));
            }
            other => panic!("expected an unknown key, found {other:?}"),
        }
        assert_eq!(
            config_error.hint().as_deref(),
            Some("did you mean `depth`?")
        );
        assert!(config_error.to_string().contains("blastr.toml"));
    }

    #[test]
    fn an_unknown_nested_key_is_dotted_and_suggests_the_closest_key() {
        let config_error = error("[ranking]\ndecay = 0.5\ndecy = 0.5\n");
        assert_eq!(key_of(&config_error), "ranking.decy");
        match &config_error {
            ConfigError::UnknownKey { suggestion, .. } => {
                assert_eq!(suggestion.as_deref(), Some("decay"));
            }
            other => panic!("expected an unknown key, found {other:?}"),
        }
        assert_eq!(
            config_error.hint().as_deref(),
            Some("did you mean `decay`?")
        );
    }

    #[test]
    fn a_key_with_no_close_match_has_no_suggestion() {
        let config_error = error("unknown_key = 1\n");
        match &config_error {
            ConfigError::UnknownKey { suggestion, .. } => assert!(suggestion.is_none()),
            other => panic!("expected an unknown key, found {other:?}"),
        }
        let hint = config_error.hint().expect("the hint lists the valid keys");
        assert!(hint.contains("depth"), "hint was {hint}");
        assert!(hint.contains("ranking"), "hint was {hint}");
        assert!(!hint.contains("did you mean"), "hint was {hint}");
    }

    #[test]
    fn an_unknown_nested_key_without_a_close_match_lists_the_ranking_keys() {
        let config_error = error("[ranking]\nzzzzzzzzzz = 1\n");
        let hint = config_error
            .hint()
            .expect("the hint lists the ranking keys");
        assert!(hint.contains("candidate_weight"), "hint was {hint}");
        assert!(!hint.contains("editor"), "hint was {hint}");
    }

    #[test]
    fn a_wrong_type_names_the_key() {
        for (text, key) in [
            ("depth = \"six\"\n", "depth"),
            ("features = \"default\"\n", "features"),
            ("ignore = [1]\n", "ignore"),
            ("editor = 4\n", "editor"),
            ("min_confidence = 3\n", "min_confidence"),
            ("ranking = 3\n", "ranking"),
            ("[ranking]\ndecay = \"much\"\n", "ranking.decay"),
        ] {
            let config_error = error(text);
            assert_eq!(key_of(&config_error), key, "for {text:?}");
            match &config_error {
                ConfigError::InvalidValue { path, .. } => {
                    assert_eq!(path, Path::new("blastr.toml"));
                }
                other => panic!("expected an invalid value for {text:?}, found {other:?}"),
            }
            assert!(config_error.hint().is_some(), "no hint for {text:?}");
        }
    }

    #[test]
    fn an_out_of_range_depth_names_the_key() {
        for text in ["depth = 0\n", "depth = 65\n", "depth = -1\n"] {
            let config_error = error(text);
            assert_eq!(key_of(&config_error), "depth");
            assert!(config_error.to_string().contains("between 1 and 64"));
        }
    }

    #[test]
    fn an_out_of_range_weight_names_the_key() {
        for (text, key) in [
            ("[ranking]\ndecay = 0.0\n", "ranking.decay"),
            ("[ranking]\ndecay = 1.5\n", "ranking.decay"),
            (
                "[ranking]\ncandidate_weight = 0\n",
                "ranking.candidate_weight",
            ),
            (
                "[ranking]\nadaptation_weight = 2.0\n",
                "ranking.adaptation_weight",
            ),
            (
                "[ranking]\nbehaviour_weight = -0.5\n",
                "ranking.behaviour_weight",
            ),
            ("[ranking]\nscore_floor = 1.2\n", "ranking.score_floor"),
        ] {
            let config_error = error(text);
            assert_eq!(key_of(&config_error), key);
            let hint = config_error.hint().expect("a weight error has a hint");
            assert!(hint.contains("number"), "hint was {hint}");
        }
    }

    #[test]
    fn an_unknown_confidence_lists_the_accepted_names() {
        let config_error = error("min_confidence = \"maybe\"\n");
        assert_eq!(key_of(&config_error), "min_confidence");
        let message = config_error.to_string();
        assert!(message.contains("`maybe`"), "message was {message}");
        let hint = config_error.hint().expect("the hint lists the names");
        for confidence in Confidence::ALL {
            assert!(hint.contains(confidence.as_str()), "hint was {hint}");
        }
    }

    #[test]
    fn every_confidence_name_parses() {
        for confidence in Confidence::ALL {
            let text = format!("min_confidence = \"{}\"\n", confidence.as_str());
            assert_eq!(
                parse(&text).expect("the name must parse").min_confidence,
                Some(*confidence)
            );
        }
    }

    #[test]
    fn a_syntax_error_names_the_file() {
        let config_error = Config::parse("depth = ", Path::new("C:/repo/blastr.toml"))
            .expect_err("the text must not parse");
        match &config_error {
            ConfigError::Syntax { path, message } => {
                assert_eq!(path, Path::new("C:/repo/blastr.toml"));
                assert!(!message.is_empty());
            }
            other => panic!("expected a syntax error, found {other:?}"),
        }
        assert!(config_error.hint().is_some());
    }

    #[test]
    fn over_prefers_every_field_of_the_higher_config() {
        let higher = parse(FULL).expect("the example must parse");
        let lower = Config {
            features: Some(vec!["other".to_owned()]),
            depth: Some(2),
            min_confidence: Some(Confidence::Definite),
            ignore: Some(vec!["target/**".to_owned()]),
            editor: Some("vi {file}".to_owned()),
            ranking: RankingOverrides {
                decay: Some(0.9),
                candidate_weight: Some(0.9),
                adaptation_weight: Some(0.9),
                behaviour_weight: Some(0.9),
                score_floor: Some(0.9),
            },
        };
        assert_eq!(higher.clone().over(lower), higher);
    }

    #[test]
    fn over_keeps_the_lower_value_where_the_higher_one_is_unset() {
        let higher = Config {
            features: Some(vec!["higher".to_owned()]),
            depth: None,
            ranking: RankingOverrides {
                decay: Some(0.5),
                score_floor: None,
                ..RankingOverrides::default()
            },
            ..Config::default()
        };
        let lower = parse(FULL).expect("the example must parse");

        let merged = higher.over(lower);
        assert_eq!(merged.features, Some(vec!["higher".to_owned()]));
        assert_eq!(merged.depth, Some(6));
        assert_eq!(merged.min_confidence, Some(Confidence::Possible));
        assert_eq!(merged.editor, Some("zed {file}:{line}".to_owned()));
        assert_eq!(merged.ranking.decay, Some(0.5));
        assert_eq!(merged.ranking.score_floor, Some(0.05));
        assert_eq!(merged.ranking.candidate_weight, Some(0.4));
    }

    #[test]
    fn params_give_the_defaults_for_an_empty_config() {
        assert_eq!(Config::default().params(), AnalysisParams::default());
    }

    #[test]
    fn params_apply_every_configured_value() {
        let params = parse(FULL).expect("the example must parse").params();
        assert_eq!(params.depth, 6);
        assert_eq!(params.min_confidence, Confidence::Possible);
        assert_eq!(
            params.ignore,
            vec!["vendor/**".to_owned(), "**/generated/**".to_owned()]
        );
        assert_eq!(params.ranking.decay, 0.6);
        assert_eq!(params.ranking.candidate_weight, 0.4);
        assert_eq!(params.ranking.adaptation_weight, 1.0);
        assert_eq!(params.ranking.behaviour_weight, 0.8);
        assert_eq!(params.ranking.score_floor, 0.05);

        let partial =
            parse("depth = 3\nmin_confidence = \"definite\"\n[ranking]\nscore_floor = 0.1\n")
                .expect("the text must parse")
                .params();
        assert_eq!(partial.depth, 3);
        assert_eq!(partial.min_confidence, Confidence::Definite);
        assert_eq!(partial.ranking.score_floor, 0.1);
        assert_eq!(partial.ranking.decay, RankingParams::default().decay);
    }

    #[test]
    fn load_returns_the_defaults_when_neither_file_exists() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let config = Config::load(root.path(), &root.path().join("config"))
            .expect("missing files are not an error");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn load_reads_the_project_file() {
        let root = tempfile::tempdir().expect("a temporary directory");
        fs::write(root.path().join("blastr.toml"), "depth = 2\n")
            .expect("the file must be written");

        let config =
            Config::load(root.path(), &root.path().join("config")).expect("the file must load");
        assert_eq!(config.depth, Some(2));
        assert_eq!(config.editor, None);
    }

    #[test]
    fn load_reads_the_user_file() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let config_dir = root.path().join("config");
        fs::create_dir_all(&config_dir).expect("the directory must be created");
        fs::write(
            config_dir.join("config.toml"),
            "depth = 2\neditor = \"vi {file}\"\n",
        )
        .expect("the file must be written");

        let config = Config::load(root.path(), &config_dir).expect("the file must load");
        assert_eq!(config.depth, Some(2));
        assert_eq!(config.editor, Some("vi {file}".to_owned()));
    }

    #[test]
    fn load_prefers_the_project_file_over_the_user_file() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let config_dir = root.path().join("config");
        fs::create_dir_all(&config_dir).expect("the directory must be created");
        fs::write(root.path().join("blastr.toml"), "depth = 2\n")
            .expect("the project file must be written");
        fs::write(
            config_dir.join("config.toml"),
            "depth = 9\neditor = \"vi {file}\"\n",
        )
        .expect("the user file must be written");

        let config = Config::load(root.path(), &config_dir).expect("the files must load");
        assert_eq!(config.depth, Some(2));
        assert_eq!(config.editor, Some("vi {file}".to_owned()));
    }

    #[test]
    fn load_reports_the_project_file_by_name() {
        let root = tempfile::tempdir().expect("a temporary directory");
        fs::write(root.path().join("blastr.toml"), "dept = 2\n").expect("the file must be written");

        let config_error = Config::load(root.path(), &root.path().join("config"))
            .expect_err("the key must be rejected");
        assert_eq!(key_of(&config_error), "dept");
        assert!(config_error.to_string().contains("blastr.toml"));
    }

    #[test]
    fn load_file_reads_or_reports_a_missing_file() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let path = root.path().join("blastr.toml");
        assert!(
            Config::load_file(&path)
                .expect("a missing file is fine")
                .is_none()
        );

        fs::write(&path, "editor = \"zed\"\n").expect("the file must be written");
        let config = Config::load_file(&path)
            .expect("the file must load")
            .expect("the file exists");
        assert_eq!(config.editor, Some("zed".to_owned()));
    }

    #[test]
    fn a_double_star_pattern_matches_everything_under_a_directory() {
        assert!(ignored("vendor/**", "vendor/a.rs"));
        assert!(ignored("vendor/**", "vendor/a/b/c.rs"));
        assert!(!ignored("vendor/**", "src/vendor/a.rs"));
        assert!(!ignored("vendor/**", "vendorx/a.rs"));
    }

    #[test]
    fn a_leading_double_star_matches_any_number_of_directories() {
        assert!(ignored("**/generated/**", "a/b/generated/c.rs"));
        assert!(ignored("**/generated/**", "generated/c.rs"));
        assert!(!ignored("**/generated/**", "a/b/generatedx/c.rs"));
        assert!(!ignored("**/generated/**", "a/b/generated.txt/c.rs"));
    }

    #[test]
    fn a_double_star_inside_a_pattern_covers_whole_segments() {
        assert!(ignored("a/**/b.rs", "a/b.rs"));
        assert!(ignored("a/**/b.rs", "a/x/y/b.rs"));
        assert!(!ignored("a/**/b.rs", "a/x/y/c.rs"));
        let set = IgnoreSet::new(&["a/**/b".to_owned()]).expect("the pattern must compile");
        assert!(!set.is_ignored(&RelPath::new("a/xxb")));
    }

    #[test]
    fn a_pattern_without_a_slash_matches_a_file_name_anywhere() {
        assert!(ignored("*.gen.rs", "a.gen.rs"));
        assert!(ignored("*.gen.rs", "src/a.gen.rs"));
        assert!(ignored("*.gen.rs", "src/deep/a.gen.rs"));
        assert!(!ignored("*.gen.rs", "src/a.rs"));
        assert!(!ignored("*.gen.rs", "src/a.gen.rs/b.rs"));
    }

    #[test]
    fn a_star_stays_inside_one_segment() {
        assert!(ignored("src/*.rs", "src/a.rs"));
        assert!(ignored("src/*.rs", "src/.rs"));
        assert!(!ignored("src/*.rs", "src/a/b.rs"));
        assert!(!ignored("src/*.rs", "other/a.rs"));
    }

    #[test]
    fn a_question_mark_matches_one_character() {
        assert!(ignored("src/?.rs", "src/a.rs"));
        assert!(!ignored("src/?.rs", "src/ab.rs"));
        assert!(!ignored("src/?.rs", "src/.rs"));
        assert!(ignored("?.rs", "src/a.rs"));
    }

    #[test]
    fn a_bad_pattern_is_rejected() {
        for pattern in ["", "a\\b.rs", "src/a**/b.rs", "**x"] {
            match Pattern::compile(pattern) {
                Err(ConfigError::BadPattern {
                    pattern: reported,
                    message,
                }) => {
                    assert_eq!(reported, pattern);
                    assert!(!message.is_empty());
                }
                other => panic!("expected a bad pattern for {pattern:?}, found {other:?}"),
            }
        }
    }

    #[test]
    fn parse_validates_ignore_patterns() {
        let config_error = error("ignore = [\"a\\\\b\"]\n");
        match &config_error {
            ConfigError::BadPattern { pattern, .. } => assert_eq!(pattern, "a\\b"),
            other => panic!("expected a bad pattern, found {other:?}"),
        }
        assert!(config_error.hint().is_some());
    }

    #[test]
    fn an_empty_ignore_set_ignores_nothing() {
        assert!(!IgnoreSet::empty().is_ignored(&RelPath::new("src/a.rs")));
        assert!(
            !IgnoreSet::new(&[])
                .expect("empty is valid")
                .is_ignored(&RelPath::new("a.rs"))
        );
    }

    #[test]
    fn several_patterns_combine() {
        let set = IgnoreSet::new(&["vendor/**".to_owned(), "*.gen.rs".to_owned()])
            .expect("the patterns must compile");
        assert!(set.is_ignored(&RelPath::new("vendor/a.rs")));
        assert!(set.is_ignored(&RelPath::new("src/a.gen.rs")));
        assert!(!set.is_ignored(&RelPath::new("src/a.rs")));
    }

    #[test]
    fn edit_distance_counts_single_edits() {
        assert_eq!(edit_distance("depth", "depth"), 0);
        assert_eq!(edit_distance("dept", "depth"), 1);
        assert_eq!(edit_distance("decy", "decay"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
