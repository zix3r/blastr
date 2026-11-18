//! Concrete stages and sinks for the `pipeline` workspace.
//!
//! `pipeline-core` knows how to run stages; this crate knows what stages an
//! operator actually configures: filtering, mapping, redaction, deduplication,
//! truncation, tagging, enrichment, tapping and limiting.

pub mod dedup;
pub mod enrich;
pub mod filter;
pub mod group;
pub mod limit;
pub mod map;
pub mod normalize;
pub mod partition;
pub mod predicates;
pub mod redact;
pub mod tag;
pub mod tap;
pub mod text_sink;
pub mod transforms;
pub mod truncate;

pub use dedup::Dedup;
pub use enrich::Enrich;
pub use filter::{Filter, Predicate};
pub use group::Group;
pub use limit::Limit;
pub use map::{Map, Transform};
pub use normalize::NormalizeLevel;
pub use partition::{ByField, ByLevel, BySource, Partition};
pub use predicates::{
    has_request_id, is_above_debug, is_error, is_health_check, is_warn_or_higher, predicate_by_name,
};
pub use redact::Redact;
pub use tag::Tag;
pub use tap::Tap;
pub use text_sink::{TextSink, render_record};
pub use transforms::{
    lowercase_message, tag_message_length, tag_severity, tag_word_count, transform_by_name,
    trim_message, uppercase_message,
};
pub use truncate::Truncate;
