//! Core types for the shopfront workspace.
//!
//! Everything the other crates talk about lives here: money, the catalogue,
//! customers, orders and the errors they produce. No I/O and no formatting
//! policy beyond the small `Display` implementations.

pub mod customer;
pub mod discount;
pub mod error;
pub mod money;
pub mod order;
pub mod product;

pub use customer::{Address, Country, Customer, CustomerId, Region};
pub use discount::Discount;
pub use error::DomainError;
pub use money::{CENTS_PER_UNIT, Cents, Currency, Money, portion, sum_by};
pub use order::{CancelReason, Carrier, Order, OrderId, OrderLine, OrderStatus};
pub use product::{Product, Sku};
