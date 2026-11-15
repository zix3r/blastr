//! Shared data types for Blastr.
//!
//! Every other crate depends on this one and on nothing below it: the
//! vocabulary enums, the graph and change types, and the progress and
//! cancellation types passed to long-running operations.

#![forbid(unsafe_code)]

mod cancel;
mod hashing;
mod impls;
mod index;
mod progress;
mod types;
mod vocab;

pub use cancel::{CancelToken, Cancelled};
pub use hashing::{hash_tokens, sketch_similarity, sketch_tokens};
pub use index::GraphIndex;
pub use progress::{NoProgress, Progress};
pub use types::*;
pub use vocab::*;
