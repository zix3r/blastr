//! Severity levels attached to every record.

use std::fmt;
use std::str::FromStr;

use crate::error::PipelineError;

/// Severity of a single record, ordered from least to most severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    /// Every level, ordered from least to most severe.
    pub const ALL: [Level; 6] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
        Level::Fatal,
    ];

    /// The canonical lower-case spelling used in configuration and fields.
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }

    /// The upper-case spelling used when a record is rendered.
    pub const fn badge(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        }
    }

    /// The numeric severity, matching the syslog ranges used by the log format.
    pub const fn severity(self) -> u8 {
        match self {
            Level::Trace => 10,
            Level::Debug => 20,
            Level::Info => 30,
            Level::Warn => 40,
            Level::Error => 50,
            Level::Fatal => 60,
        }
    }

    /// Whether this level is at least as severe as `floor`.
    pub fn at_least(self, floor: Level) -> bool {
        self.severity() >= floor.severity()
    }

    /// Whether the level signals a failure that operators must look at.
    pub fn is_failure(self) -> bool {
        matches!(self, Level::Error | Level::Fatal)
    }

    /// Parses a level label, accepting the spellings operators type by hand.
    pub fn from_label(label: &str) -> Result<Level, PipelineError> {
        label.parse()
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Level {
    type Err = PipelineError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let lowered = text.trim().to_ascii_lowercase();
        match lowered.as_str() {
            "trace" | "trc" | "10" => Ok(Level::Trace),
            "debug" | "dbg" | "20" => Ok(Level::Debug),
            "info" | "information" | "30" => Ok(Level::Info),
            "warn" | "warning" | "40" => Ok(Level::Warn),
            "error" | "err" | "50" => Ok(Level::Error),
            "fatal" | "critical" | "crit" | "60" => Ok(Level::Fatal),
            _ => Err(PipelineError::parse("level", text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip() {
        for level in Level::ALL {
            assert_eq!(level.to_string(), level.as_str());
            assert_eq!(level.badge(), level.badge().to_ascii_uppercase());
            assert_eq!(level.as_str().parse::<Level>(), Ok(level));
        }
    }

    #[test]
    fn hand_written_spellings_are_accepted() {
        assert_eq!(Level::from_label("  WARNING "), Ok(Level::Warn));
        assert_eq!(Level::from_label("crit"), Ok(Level::Fatal));
        assert_eq!(Level::from_label("Trc"), Ok(Level::Trace));
        assert_eq!(Level::from_label("information"), Ok(Level::Info));
        assert_eq!(Level::from_label("50"), Ok(Level::Error));
        assert_eq!(
            Level::from_label("loud"),
            Err(PipelineError::parse("level", "loud"))
        );
    }

    #[test]
    fn ordering_follows_severity() {
        assert!(Level::Error > Level::Warn);
        assert!(Level::Info.at_least(Level::Info));
        assert!(!Level::Debug.at_least(Level::Warn));
        assert!(Level::Fatal.is_failure());
        assert!(!Level::Warn.is_failure());
    }

    #[test]
    fn descriptions_match_badges() {
        let mut badges: Vec<&str> = Level::ALL.iter().map(|level| level.badge()).collect();
        badges.sort_unstable();
        assert_eq!(
            badges,
            vec!["DEBUG", "ERROR", "FATAL", "INFO", "TRACE", "WARN"]
        );
    }
}
