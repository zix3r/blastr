//! An in-memory store: catalogue, stock, customers and orders, plus the line
//! based snapshot format used to move a shop between processes.

pub mod error;
pub mod store;
pub mod text;

pub use error::StorageError;
pub use store::{FIRST_ORDER_ID, RegionRevenue, Reservation, Store};
pub use text::{decode, encode};
