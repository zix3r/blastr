//! A `?` that converts one error into another through `From`.

use std::fmt;

/// The failure of the low-level step.
pub struct BadInput;

impl fmt::Display for BadInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid input")
    }
}

/// The failure the public function reports.
pub struct Failure {
    reason: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl From<BadInput> for Failure {
    fn from(error: BadInput) -> Failure {
        Failure {
            reason: error.to_string(),
        }
    }
}

/// A step that can fail on its own.
pub fn parse(text: &str) -> Result<u32, BadInput> {
    text.parse::<u32>().map_err(|_| BadInput)
}

/// The public entry point, which converts the error with `?`.
pub fn attempt(text: &str) -> Result<u32, Failure> {
    let value = parse(text)?;
    Ok(value + 1)
}

/// A `?` that converts nothing: it errors with `Failure` already.
pub fn checked(text: &str) -> Result<u32, Failure> {
    let value = text.parse::<u32>().map_err(|_| Failure {
        reason: String::from("not a number"),
    })?;
    Ok(value)
}
