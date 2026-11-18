//! The ignore set: the compiled glob patterns that keep files out of an
//! analysis.
//!
//! The patterns come from the window, which holds them in the user's
//! configuration and sends them with every request. Nothing here reads a
//! file: this module only compiles the patterns and answers whether a path
//! matches one of them.

use blastr_model::RelPath;

/// What can go wrong while compiling an ignore pattern.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PatternError {
    /// An ignore pattern is not a usable glob.
    #[error("bad ignore pattern `{pattern}`: {message}")]
    BadPattern {
        /// The pattern as the user wrote it.
        pattern: String,
        /// What is wrong with it.
        message: String,
    },
}

impl PatternError {
    /// One line telling the user what to do next, when there is something useful to say.
    pub fn hint(&self) -> Option<String> {
        match self {
            PatternError::BadPattern { .. } => Some(
                "a pattern matches repository paths: `*` and `?` stay within one segment, `**` covers whole segments"
                    .to_owned(),
            ),
        }
    }
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
    pub fn new(patterns: &[String]) -> Result<IgnoreSet, PatternError> {
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
    fn compile(pattern: &str) -> Result<Pattern, PatternError> {
        if pattern.is_empty() {
            return Err(PatternError::BadPattern {
                pattern: pattern.to_owned(),
                message: "the pattern is empty".to_owned(),
            });
        }
        if pattern.contains('\\') {
            return Err(PatternError::BadPattern {
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
                return Err(PatternError::BadPattern {
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
    use super::*;

    /// Whether `pattern` ignores `path`.
    fn ignored(pattern: &str, path: &str) -> bool {
        let set = IgnoreSet::new(&[pattern.to_owned()]).expect("the pattern must compile");
        set.is_ignored(&RelPath::new(path))
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
                Err(PatternError::BadPattern {
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
    fn a_bad_pattern_tells_the_user_what_a_pattern_matches() {
        let error = PatternError::BadPattern {
            pattern: "a\\b".to_owned(),
            message: "use `/` to separate path segments".to_owned(),
        };
        assert!(error.to_string().contains("a\\b"));
        let hint = error.hint().expect("a bad pattern has a hint");
        assert!(hint.contains("**"), "hint was {hint}");
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
}
