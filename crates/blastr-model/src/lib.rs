//! Shared data types for Blastr.
//!
//! Every other crate depends on this one and on nothing below it: the
//! vocabulary enums, the graph and change types, the progress and
//! cancellation types passed to long-running operations, the messages the
//! window and the engine exchange in [`protocol`], and the words the window
//! shows in [`wording`]. It holds no analysis, so the window can depend on it
//! without the analysis pipeline.

#![forbid(unsafe_code)]

mod analysis;
pub mod bridge;
mod cancel;
mod hashing;
mod ignore;
mod impls;
mod index;
mod paths;
mod progress;
pub mod protocol;
mod types;
mod vocab;
pub mod wording;

/// The version of this build, as `Cargo.toml` states it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use analysis::*;
pub use cancel::{CancelToken, Cancelled};
pub use hashing::{hash_tokens, sketch_similarity, sketch_tokens};
pub use ignore::{IgnoreSet, PatternError};
pub use index::GraphIndex;
pub use paths::{Paths, paths};
pub use progress::{NoProgress, Progress};
pub use types::*;
pub use vocab::*;
