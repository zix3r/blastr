//! Core building blocks of the `pipeline` sample project.
//!
//! The crate defines the data that flows through a pipeline ([`Record`]), the
//! [`Stage`] and [`Sink`] traits, the [`Pipeline`] runner, and the error type
//! shared by every crate in the workspace.

pub mod describe;
pub mod error;
pub mod level;
pub mod pipeline;
pub mod reader;
pub mod record;
pub mod sink;
pub mod stage;

pub use describe::Describe;
pub use error::PipelineError;
pub use level::Level;
pub use pipeline::{Pipeline, RunOutput, RunStats, StageStats, drain, run_pair, run_stage};
pub use reader::{LineReader, parse_line, read_records};
pub use record::{Fields, Record, RecordBuilder, Words};
pub use sink::{CollectSink, CountingSink, Sink, TallySink, describe_sink};
pub use stage::{Chain, Stage, StageExt, StageKind, StageReport};
