//! An app stage: rejects records that miss mandatory fields.

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

/// Fails the pipeline when a record lacks a mandatory field.
///
/// The stage is defined here, outside `pipeline-stages`, to show that stages
/// compose across crates: the configuration layer can put it in a pipeline
/// like any other stage.
#[derive(Clone, Debug)]
pub struct Validate {
    required: Vec<String>,
}

impl Validate {
    /// A stage requiring the given field names.
    pub fn new<I, S>(required: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            required: required.into_iter().map(Into::into).collect(),
        }
    }

    /// The field names that must be present.
    pub fn keys(&self) -> Vec<&str> {
        self.required.iter().map(String::as_str).collect()
    }
}

impl Stage for Validate {
    fn name(&self) -> &str {
        "validate"
    }

    fn kind(&self) -> StageKind {
        StageKind::Filter
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        for key in &self.required {
            if !record.has_field(key) {
                return Err(PipelineError::missing("validate", key.as_str()));
            }
        }
        Ok(vec![record])
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!(
            "validated {} record(s) against {} field(s)",
            report.output,
            self.required.len()
        )
    }

    fn describe(&self) -> String {
        format!("validate({})", self.required.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    #[test]
    fn records_with_every_field_pass() {
        let mut validate = Validate::new(["request_id", "status"]);
        let record = Record::new("api", Level::Info, "paid")
            .with_field("request_id", "abc")
            .with_field("status", "200");
        let output = validate.apply(record.clone()).unwrap();
        assert_eq!(output, vec![record]);
        assert_eq!(validate.keys(), vec!["request_id", "status"]);
        assert_eq!(validate.describe(), "validate(request_id,status)");
    }

    #[test]
    fn a_missing_field_is_an_error() {
        let mut validate = Validate::new(["request_id"]);
        let error = validate
            .apply(Record::new("api", Level::Info, "paid"))
            .unwrap_err();
        assert_eq!(error, PipelineError::missing("validate", "request_id"));
        assert_eq!(
            error.to_string(),
            "stage `validate` requires field `request_id`"
        );
    }

    #[test]
    fn summarise_counts_the_batch() {
        let validate = Validate::new(["status"]);
        assert_eq!(
            validate.summarise(&StageReport::new(4, 3)),
            "validated 3 record(s) against 1 field(s)"
        );
        assert_eq!(validate.kind(), StageKind::Filter);
    }
}
