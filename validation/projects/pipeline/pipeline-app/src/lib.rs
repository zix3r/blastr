//! Assembles a record pipeline from a textual spec and runs it.
//!
//! The binary is a thin wrapper around [`run_spec`]: it takes an optional spec
//! on the command line, reads the built-in sample log and prints the report.

pub mod config;
pub mod report;
pub mod validate;

pub use config::{StageSpec, build_pipeline, build_stage, parse_spec};
pub use report::{Report, run_spec};
pub use validate::Validate;

/// The log sample bundled with the demo binary.
pub const SAMPLE_LOG: &str = "\
# order-service sample log
INFO api request finished status=200 path=/orders
INFO api rate limited level=warn status=429 path=/orders
WARN api slow request finished status=504 path=/orders
ERROR api upstream timeout status=504 path=/orders token=abc123
ERROR api upstream timeout status=504 path=/orders token=abc123
DEBUG worker cache warmup finished entries=128
INFO api health check status=200 path=/health
WARN worker compaction finished after scanning every segment in the archive
";

/// The spec the binary uses when the caller does not pass one.
pub const DEFAULT_SPEC: &str = "level|filter:no-debug|dedup|redact|group:level|enrich|truncate:28";
