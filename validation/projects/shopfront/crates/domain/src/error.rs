//! One error type for every domain level failure.

use std::fmt;

use crate::money::Currency;

/// Everything that can go wrong while building or validating domain values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomainError {
    /// A required text field was blank.
    EmptyField(&'static str),
    /// A SKU did not match the catalogue grammar.
    InvalidSku { raw: String, reason: String },
    /// A field carried a value the domain does not know.
    InvalidField { field: &'static str, value: String },
    /// A currency code was not recognised.
    UnknownCurrency(String),
    /// A country code was not recognised.
    UnknownCountry(String),
    /// A region name was not recognised.
    UnknownRegion(String),
    /// A discount specification could not be parsed.
    InvalidDiscount(String),
    /// A quantity outside the accepted range.
    InvalidQuantity(u32),
    /// Two amounts of different currencies were combined.
    CurrencyMismatch { left: Currency, right: Currency },
    /// Integer arithmetic left the representable range.
    Overflow,
}

impl DomainError {
    /// A full sentence describing the failure, used by every front end.
    pub fn message(&self) -> String {
        match self {
            DomainError::EmptyField(field) => format!("{field} must not be empty"),
            DomainError::InvalidSku { raw, reason } => format!("invalid sku '{raw}': {reason}"),
            DomainError::InvalidField { field, value } => format!("invalid {field} '{value}'"),
            DomainError::UnknownCurrency(code) => format!("unknown currency '{code}'"),
            DomainError::UnknownCountry(code) => format!("unknown country '{code}'"),
            DomainError::UnknownRegion(name) => format!("unknown region '{name}'"),
            DomainError::InvalidDiscount(spec) => format!("invalid discount '{spec}'"),
            DomainError::InvalidQuantity(quantity) => {
                format!("invalid quantity {quantity}")
            }
            DomainError::CurrencyMismatch { left, right } => {
                format!("currency mismatch: {} vs {}", left.code(), right.code())
            }
            DomainError::Overflow => "amount overflow".to_string(),
        }
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DomainError {}
