//! `macrokit-station` — an application built on `macrokit`.
//!
//! The station crate shows what a consumer of `macrokit` looks like: it merges
//! its own settings into the generated baseline schema, registers extra
//! commands with `#[command]`, derives `Describe` on its own types and reuses
//! `macrokit`'s exported `macro_rules!` helpers.

pub mod profile;
pub mod station;

pub use profile::ThermalProfile;
pub use station::{Estimate, Station, StationError, StationResult};
