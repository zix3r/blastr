//! Failures the store can hand back.

use std::fmt;

use domain::{Cents, DomainError};

/// Everything that can go wrong while reading or writing the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageError {
    /// The SKU is already in the catalogue.
    DuplicateSku(String),
    /// The customer id is already registered.
    DuplicateCustomer(u64),
    /// No product with that SKU.
    UnknownSku(String),
    /// No customer with that id.
    UnknownCustomer(u64),
    /// No order with that id.
    UnknownOrder(u64),
    /// Not enough unreserved stock.
    InsufficientStock {
        sku: String,
        requested: u32,
        available: u32,
    },
    /// An order needs at least one line.
    EmptyOrder,
    /// The payment covered less than the grand total.
    Underpaid { due: Cents, paid: Cents },
    /// A text field cannot be stored in the snapshot format.
    InvalidText(&'static str),
    /// A snapshot line could not be parsed.
    Parse { line: usize, message: String },
    /// A domain level failure surfaced while storing.
    Domain(DomainError),
}

impl StorageError {
    /// A full sentence describing the failure.
    pub fn message(&self) -> String {
        match self {
            StorageError::DuplicateSku(sku) => format!("product '{sku}' already exists"),
            StorageError::DuplicateCustomer(id) => format!("customer {id} already exists"),
            StorageError::UnknownSku(sku) => format!("unknown product '{sku}'"),
            StorageError::UnknownCustomer(id) => format!("unknown customer {id}"),
            StorageError::UnknownOrder(id) => format!("unknown order #{id}"),
            StorageError::InsufficientStock {
                sku,
                requested,
                available,
            } => format!("only {available} of '{sku}' available, {requested} requested"),
            StorageError::EmptyOrder => "an order needs at least one line".to_string(),
            StorageError::Underpaid { due, paid } => {
                format!("payment of {paid} cents does not cover {due} cents")
            }
            StorageError::InvalidText(field) => {
                format!("{field} must not contain '|' or ';'")
            }
            StorageError::Parse { line, message } => {
                format!("snapshot line {line}: {message}")
            }
            StorageError::Domain(error) => error.message(),
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for StorageError {}

impl From<DomainError> for StorageError {
    fn from(error: DomainError) -> StorageError {
        StorageError::Domain(error)
    }
}
