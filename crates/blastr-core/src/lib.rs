//! Analysis pipeline for Blastr.
//!
//! This crate holds the pieces that every front end shares: the error type,
//! the progress and cancellation types passed to long-running operations, and
//! the [`Engine`] that owns the cache.

#![forbid(unsafe_code)]

mod cancel;
mod engine;
mod error;
mod progress;

pub use cancel::CancelToken;
pub use engine::Engine;
pub use error::{Error, Result};
pub use progress::{NoProgress, Progress};
