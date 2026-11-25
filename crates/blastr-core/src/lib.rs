//! Analysis pipeline for Blastr.
//!
//! This crate holds the error type, the [`Engine`] that owns the cache and
//! runs comparisons, the structural comparison of two revision graphs in
//! `diff`, the impact analysis in `impact`, and the engine's side of the
//! engine process in [`service`]. The types it shares with the window live in
//! `blastr-model`.

#![forbid(unsafe_code)]

mod cache;
mod codec;
mod diff;
mod engine;
mod error;
mod hunks;
mod impact;
mod pipeline;
pub mod service;
mod session;

/// The version of the rust-analyzer libraries this build uses.
pub const RUST_ANALYZER_VERSION: &str = blastr_ra::RUST_ANALYZER_VERSION;

pub use blastr_model::{CancelToken, NoProgress, Progress, VERSION};
pub use engine::{AnalysisRequest, Engine, build_id};
pub use error::{Error, Result};
pub use session::Session;
