//! Analysis pipeline for Blastr.
//!
//! This crate holds what every front end shares: the error type, the
//! [`Engine`] that owns the cache and runs comparisons, the structural
//! comparison of two revision graphs in [`diff`], the words every front end
//! shows in [`wording`], and the messages of
//! the engine process in [`service`].

#![forbid(unsafe_code)]

mod base;
mod cache;
mod codec;
pub mod config;
pub mod diff;
mod engine;
mod error;
mod hunks;
pub mod impact;
pub mod service;
mod session;
pub mod wording;

/// The version of this build, as `Cargo.toml` states it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version of the rust-analyzer libraries this build uses.
pub const RUST_ANALYZER_VERSION: &str = blastr_ra::RUST_ANALYZER_VERSION;

pub use base::{FileEdit, ProjectBase, Variant};
pub use blastr_model::{CancelToken, NoProgress, Progress};
pub use engine::{CacheStatus, DiffOutput, Engine, build_id};
pub use error::{Error, Result};
pub use session::{Refresh, Session, SessionRequest};
