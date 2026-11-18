//! The error type shared by every crate in the workspace.

use std::fmt;
use std::num::ParseIntError;
use std::str::ParseBoolError;

/// Everything that can go wrong while configuring or running a pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineError {
    /// The textual configuration could not be understood.
    Config(String),
    /// A value could not be parsed into the shape the pipeline expects.
    Parse {
        context: &'static str,
        value: String,
    },
    /// A stage required a field that the record does not carry.
    Missing { stage: &'static str, field: String },
    /// A stage refused a record for a domain-specific reason.
    Stage { stage: &'static str, detail: String },
    /// A bounded stage saw more records than it is allowed to keep.
    Limit { limit: usize, actual: usize },
}

impl PipelineError {
    /// Builds a configuration error.
    pub fn config(detail: impl Into<String>) -> Self {
        Self::Config(detail.into())
    }

    /// Builds a parse error for `context`, remembering the offending value.
    pub fn parse(context: &'static str, value: impl Into<String>) -> Self {
        Self::Parse {
            context,
            value: value.into(),
        }
    }

    /// Builds the error a stage reports when it needs a field that is absent.
    pub fn missing(stage: &'static str, field: impl Into<String>) -> Self {
        Self::Missing {
            stage,
            field: field.into(),
        }
    }

    /// Builds the error a stage reports when it refuses a record.
    pub fn stage(stage: &'static str, detail: impl Into<String>) -> Self {
        Self::Stage {
            stage,
            detail: detail.into(),
        }
    }

    /// Builds the error a bounded stage reports when it overflows.
    pub fn limit(limit: usize, actual: usize) -> Self {
        Self::Limit { limit, actual }
    }

    /// Whether the error happened while reading the configuration.
    pub fn is_config(&self) -> bool {
        matches!(self, Self::Config(_))
    }

    /// The stage that raised the error, when the error is stage-specific.
    pub fn stage_name(&self) -> Option<&'static str> {
        match self {
            Self::Missing { stage, .. } | Self::Stage { stage, .. } => Some(stage),
            Self::Config(_) | Self::Parse { .. } | Self::Limit { .. } => None,
        }
    }

    /// A short tag used by the report renderer.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Config(_) => "config",
            Self::Parse { .. } => "parse",
            Self::Missing { .. } => "missing",
            Self::Stage { .. } => "stage",
            Self::Limit { .. } => "limit",
        }
    }
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(detail) => write!(f, "configuration: {detail}"),
            Self::Parse { context, value } => write!(f, "cannot parse {context} from `{value}`"),
            Self::Missing { stage, field } => {
                write!(f, "stage `{stage}` requires field `{field}`")
            }
            Self::Stage { stage, detail } => write!(f, "stage `{stage}`: {detail}"),
            Self::Limit { limit, actual } => {
                write!(f, "limit {limit} exceeded: {actual} record(s) seen")
            }
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<ParseIntError> for PipelineError {
    fn from(error: ParseIntError) -> Self {
        Self::Config(format!("expected a positive number: {error}"))
    }
}

impl From<ParseBoolError> for PipelineError {
    fn from(error: ParseBoolError) -> Self {
        Self::Config(format!("expected `true` or `false`: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_stable() {
        assert_eq!(
            PipelineError::config("unknown stage `nope`").to_string(),
            "configuration: unknown stage `nope`"
        );
        assert_eq!(
            PipelineError::parse("level", "nope").to_string(),
            "cannot parse level from `nope`"
        );
        assert_eq!(
            PipelineError::missing("validate", "request_id").to_string(),
            "stage `validate` requires field `request_id`"
        );
        assert_eq!(
            PipelineError::stage("filter", "predicate panicked").to_string(),
            "stage `filter`: predicate panicked"
        );
        assert_eq!(
            PipelineError::limit(2, 5).to_string(),
            "limit 2 exceeded: 5 record(s) seen"
        );
    }

    #[test]
    fn classifier_helpers() {
        let config = PipelineError::config("bad");
        assert!(config.is_config());
        assert_eq!(config.stage_name(), None);
        assert_eq!(config.kind(), "config");

        let missing = PipelineError::missing("validate", "status");
        assert!(!missing.is_config());
        assert_eq!(missing.stage_name(), Some("validate"));
        assert_eq!(missing.kind(), "missing");

        assert_eq!(PipelineError::parse("level", "x").kind(), "parse");
        assert_eq!(PipelineError::limit(1, 2).kind(), "limit");
        assert_eq!(PipelineError::stage("tap", "x").kind(), "stage");
    }

    #[test]
    fn numeric_errors_convert_to_configuration_errors() {
        let error: PipelineError = "twelve".parse::<usize>().unwrap_err().into();
        assert_eq!(
            error,
            PipelineError::Config(
                "expected a positive number: invalid digit found in string".into()
            )
        );
        assert!(error.is_config());

        let error: PipelineError = "yes".parse::<bool>().unwrap_err().into();
        assert!(
            error
                .to_string()
                .starts_with("configuration: expected `true` or `false`:")
        );
    }

    #[test]
    fn errors_are_usable_as_std_errors() {
        let error: Box<dyn std::error::Error> = Box::new(PipelineError::parse("field", "a==b"));
        assert_eq!(error.to_string(), "cannot parse field from `a==b`");
    }
}
