//! A `Display` implementation invoked by a format string.

use std::fmt;

/// A receipt line.
pub struct Receipt {
    /// What was bought.
    pub item: &'static str,
}

impl fmt::Display for Receipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invoice: {}", self.item)
    }
}

/// A label with a `Display` of its own.
pub struct Label;

impl fmt::Display for Label {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "label")
    }
}

/// Prints a receipt through the formatting trait.
pub fn describe(receipt: &Receipt) -> String {
    format!("{receipt}")
}

/// Prints a label through the formatting trait.
pub fn title() -> String {
    format!("{}", Label)
}

/// Formats a number, which no implementation of the workspace prints.
pub fn count(value: u32) -> String {
    format!("{value}")
}

/// Prints a receipt through `to_string`.
pub fn short(receipt: &Receipt) -> String {
    receipt.to_string()
}
