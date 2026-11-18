//! Error types shared by the configuration engine.
//!
//! There is one error enum per layer: [`UnitError`] for the unit registry,
//! [`ValueError`] for a single value check, [`SchemaError`] for schema
//! construction and [`ConfigError`] for everything the engine reports. The
//! outer layers convert the inner ones, which keeps the messages precise.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// Result alias used by the configuration engine and its commands.
pub type ConfigResult<T> = Result<T, ConfigError>;
/// Result alias used by the unit registry and quantity parsing.
pub type UnitResult<T> = Result<T, UnitError>;
/// Result alias used while building a [`Schema`](crate::schema::Schema).
pub type SchemaResult<T> = Result<T, SchemaError>;

/// A problem with a unit symbol, a dimension or a conversion.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UnitError {
    /// The symbol is not in the registry.
    UnknownUnit {
        /// The symbol that could not be resolved.
        symbol: String,
        /// Closest known symbol, if one is close enough.
        suggestion: Option<String>,
    },
    /// A quantity was written without a unit.
    MissingUnit {
        /// The offending input.
        input: String,
    },
    /// A dimension specification could not be parsed.
    MalformedDimension {
        /// The offending text.
        text: String,
    },
    /// Two units belong to different dimensions.
    DimensionMismatch {
        /// Dimension of the left hand side.
        expected: String,
        /// Dimension of the right hand side.
        found: String,
    },
    /// A dimension exponent would leave the `i8` range.
    DimensionOverflow {
        /// The exponent that overflowed.
        exponent: i32,
    },
    /// An affine unit (one with an offset) cannot be rescaled.
    OffsetUnit {
        /// Symbol of the offending unit.
        symbol: String,
    },
    /// A number could not be parsed.
    MalformedNumber {
        /// The offending input.
        input: String,
        /// Why the input was rejected.
        reason: String,
    },
    /// A value was infinite or `NaN`.
    NonFinite {
        /// The offending input.
        input: String,
    },
    /// The divisor of a ratio was zero.
    DivisionByZero,
}

impl Display for UnitError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownUnit { symbol, suggestion } => {
                write!(formatter, "unknown unit `{symbol}`")?;
                match suggestion {
                    Some(candidate) => write!(formatter, ", did you mean `{candidate}`?"),
                    None => Ok(()),
                }
            }
            Self::MissingUnit { input } => write!(formatter, "`{input}` has no unit"),
            Self::MalformedDimension { text } => write!(formatter, "malformed dimension `{text}`"),
            Self::DimensionMismatch { expected, found } => {
                write!(
                    formatter,
                    "dimension mismatch: expected `{expected}`, found `{found}`"
                )
            }
            Self::DimensionOverflow { exponent } => {
                write!(formatter, "dimension exponent {exponent} is out of range")
            }
            Self::OffsetUnit { symbol } => {
                write!(
                    formatter,
                    "unit `{symbol}` has an offset and cannot be rescaled"
                )
            }
            Self::MalformedNumber { input, reason } => {
                write!(formatter, "malformed number in `{input}`: {reason}")
            }
            Self::NonFinite { input } => write!(formatter, "`{input}` is not a finite number"),
            Self::DivisionByZero => write!(formatter, "division by zero"),
        }
    }
}

impl Error for UnitError {}

/// A problem with a single value, before it is attributed to a setting key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ValueError {
    /// The text is not a boolean word.
    NotABool {
        /// The offending text.
        text: String,
    },
    /// The text is not an integer.
    NotAnInteger {
        /// The offending text.
        text: String,
    },
    /// The text is not a finite decimal number.
    NotANumber {
        /// The offending text.
        text: String,
    },
    /// The value is empty but the kind requires content.
    Empty,
    /// The unit of the value does not match the kind.
    DimensionMismatch {
        /// Dimension required by the kind.
        expected: String,
        /// Dimension of the parsed value.
        found: String,
    },
    /// The value is outside the bounds of the kind.
    OutOfRange {
        /// The rendered value.
        text: String,
        /// The rendered bounds.
        bounds: String,
    },
    /// The text is longer than the kind allows.
    TextTooLong {
        /// Maximum number of characters.
        limit: usize,
        /// Number of characters supplied.
        actual: usize,
    },
    /// The value has the wrong variant for the kind.
    TypeMismatch {
        /// Variant the kind expects.
        expected: &'static str,
        /// Variant that was supplied.
        found: &'static str,
    },
    /// The unit layer rejected the value.
    Unit(UnitError),
}

impl Display for ValueError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotABool { text } => write!(formatter, "`{text}` is not a boolean"),
            Self::NotAnInteger { text } => write!(formatter, "`{text}` is not an integer"),
            Self::NotANumber { text } => write!(formatter, "`{text}` is not a finite number"),
            Self::Empty => write!(formatter, "a value is required"),
            Self::DimensionMismatch { expected, found } => {
                write!(
                    formatter,
                    "dimension mismatch: expected `{expected}`, found `{found}`"
                )
            }
            Self::OutOfRange { text, bounds } => {
                write!(formatter, "`{text}` is outside {bounds}")
            }
            Self::TextTooLong { limit, actual } => {
                write!(
                    formatter,
                    "text has {actual} characters, the limit is {limit}"
                )
            }
            Self::TypeMismatch { expected, found } => {
                write!(
                    formatter,
                    "expected a {expected} value, found a {found} value"
                )
            }
            Self::Unit(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ValueError {}

impl From<UnitError> for ValueError {
    fn from(error: UnitError) -> Self {
        Self::Unit(error)
    }
}

/// A problem while assembling a schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SchemaError {
    /// A key or alias is not a valid setting name.
    InvalidKey {
        /// The offending key.
        key: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The key is already present in the schema.
    DuplicateKey {
        /// The duplicated key.
        key: String,
    },
    /// An alias is already owned by another entry.
    AliasConflict {
        /// The conflicting alias.
        alias: String,
        /// Key that already owns it.
        owner: String,
    },
    /// The declared default does not satisfy the kind.
    InvalidDefault {
        /// The affected key.
        key: String,
        /// Why the default was rejected.
        reason: String,
    },
    /// A kind specification could not be parsed.
    InvalidKind {
        /// The offending specification.
        text: String,
        /// Why it was rejected.
        reason: String,
    },
    /// A rule refers to a key that is not part of the schema.
    UnknownKey {
        /// The offending key.
        key: String,
    },
    /// Merging two schemas produced a collision.
    MergeConflict {
        /// The colliding key.
        key: String,
    },
}

impl Display for SchemaError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey { key, reason } => write!(formatter, "invalid key `{key}`: {reason}"),
            Self::DuplicateKey { key } => write!(formatter, "setting `{key}` is defined twice"),
            Self::AliasConflict { alias, owner } => {
                write!(formatter, "alias `{alias}` is already used by `{owner}`")
            }
            Self::InvalidDefault { key, reason } => {
                write!(formatter, "default of `{key}` is invalid: {reason}")
            }
            Self::InvalidKind { text, reason } => {
                write!(formatter, "invalid kind `{text}`: {reason}")
            }
            Self::UnknownKey { key } => write!(formatter, "unknown key `{key}`"),
            Self::MergeConflict { key } => {
                write!(formatter, "merging schemas would overwrite `{key}`")
            }
        }
    }
}

impl Error for SchemaError {}

/// Everything the engine reports to its callers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConfigError {
    /// No setting (and no alias) matches the key.
    UnknownKey {
        /// The key that was looked up.
        key: String,
        /// Closest known key, if one is close enough.
        suggestion: Option<String>,
    },
    /// The setting exists but has no value.
    MissingValue {
        /// The key without a value.
        key: String,
    },
    /// The text could not be turned into a value for this setting.
    InvalidValue {
        /// The affected key.
        key: String,
        /// Why the text was rejected.
        reason: String,
    },
    /// An assignment line could not be parsed.
    Syntax {
        /// Where the offending text came from.
        location: String,
        /// The offending text.
        text: String,
        /// What was expected.
        reason: String,
    },
    /// Validation produced at least one error level issue.
    Validation {
        /// Number of error level issues.
        errors: usize,
        /// Message of the first one.
        first: String,
    },
    /// The unit layer rejected a value.
    Unit(UnitError),
    /// The schema layer rejected an operation.
    Schema(SchemaError),
}

impl Display for ConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey { key, suggestion } => {
                write!(formatter, "unknown setting `{key}`")?;
                match suggestion {
                    Some(candidate) => write!(formatter, ", did you mean `{candidate}`?"),
                    None => Ok(()),
                }
            }
            Self::MissingValue { key } => write!(formatter, "setting `{key}` has no value"),
            Self::InvalidValue { key, reason } => {
                write!(formatter, "invalid value for `{key}`: {reason}")
            }
            Self::Syntax {
                location,
                text,
                reason,
            } => write!(formatter, "cannot parse {location} (`{text}`): {reason}"),
            Self::Validation { errors, first } => {
                write!(formatter, "{errors} validation error(s), first: {first}")
            }
            Self::Unit(error) => Display::fmt(error, formatter),
            Self::Schema(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ConfigError {}

impl From<UnitError> for ConfigError {
    fn from(error: UnitError) -> Self {
        Self::Unit(error)
    }
}

impl From<SchemaError> for ConfigError {
    fn from(error: SchemaError) -> Self {
        Self::Schema(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigError, SchemaError, UnitError, ValueError};

    #[test]
    fn unit_errors_explain_the_failure() {
        let error = UnitError::UnknownUnit {
            symbol: "kPaa".to_string(),
            suggestion: Some("kPa".to_string()),
        };
        assert_eq!(
            error.to_string(),
            "unknown unit `kPaa`, did you mean `kPa`?"
        );
        assert_eq!(
            UnitError::DimensionMismatch {
                expected: "TIME1".to_string(),
                found: "LEN1".to_string(),
            }
            .to_string(),
            "dimension mismatch: expected `TIME1`, found `LEN1`"
        );
    }

    #[test]
    fn value_errors_render_bounds() {
        let error = ValueError::OutOfRange {
            text: "19".to_string(),
            bounds: "1..64".to_string(),
        };
        assert_eq!(error.to_string(), "`19` is outside 1..64");
    }

    #[test]
    fn schema_errors_are_distinct() {
        assert_eq!(
            SchemaError::DuplicateKey {
                key: "loop.gain".to_string()
            }
            .to_string(),
            "setting `loop.gain` is defined twice"
        );
    }

    #[test]
    fn config_errors_wrap_inner_layers() {
        let wrapped = ConfigError::from(UnitError::DivisionByZero);
        assert_eq!(wrapped.to_string(), "division by zero");
        let wrapped = ConfigError::from(SchemaError::UnknownKey {
            key: "loop.gone".to_string(),
        });
        assert_eq!(wrapped.to_string(), "unknown key `loop.gone`");
        assert_eq!(
            ConfigError::UnknownKey {
                key: "loop.gian".to_string(),
                suggestion: Some("loop.gain".to_string()),
            }
            .to_string(),
            "unknown setting `loop.gian`, did you mean `loop.gain`?"
        );
    }
}
