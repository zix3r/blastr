//! Analysis pipeline for Blastr.
//!
//! This crate holds what every front end shares: the error type, the
//! [`Engine`] that owns the cache and runs comparisons, and the structural
//! comparison of two revision graphs in [`diff`].

#![forbid(unsafe_code)]

mod codec;
pub mod config;
pub mod diff;
mod engine;
mod error;
mod hunks;
pub mod impact;
mod session;

pub use blastr_model::{CancelToken, NoProgress, Progress};
pub use codec::{decode_graph, encode_graph};
pub use engine::{CacheStatus, DiffOutput, Engine, IndexReport};
pub use error::{Error, Result};
pub use session::{Refresh, Session};
