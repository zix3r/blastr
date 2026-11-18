//! The `tag` stage: adds a constant field to every record.

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

/// Sets one field to a constant value.
///
/// By default an existing field is overwritten; [`Tag::keep_existing`] makes
/// the stage a no-op for records that already carry the field.
#[derive(Clone, Debug)]
pub struct Tag {
    key: String,
    value: String,
    overwrite: bool,
    applied: usize,
}

impl Tag {
    /// A tag that overwrites the field.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            overwrite: true,
            applied: 0,
        }
    }

    /// Keeps values that are already present.
    pub fn keep_existing(mut self) -> Self {
        self.overwrite = false;
        self
    }

    /// The field that is written.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The value that is written.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// How many records were changed so far.
    pub fn applied(&self) -> usize {
        self.applied
    }
}

impl Stage for Tag {
    fn name(&self) -> &str {
        "tag"
    }

    fn kind(&self) -> StageKind {
        StageKind::Annotate
    }

    fn apply(&mut self, mut record: Record) -> Result<Vec<Record>, PipelineError> {
        let present = record.has_field(&self.key);
        if self.overwrite || !present {
            record.set_field(self.key.as_str(), self.value.clone());
            self.applied += 1;
        }
        Ok(vec![record])
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!(
            "tagged {} record(s) with {}={} ({} passed)",
            self.applied, self.key, self.value, report.output
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    fn record() -> Record {
        Record::new("api", Level::Info, "event")
    }

    #[test]
    fn tags_every_record() {
        let mut tag = Tag::new("env", "prod");
        let output = tag.apply_all(vec![record(), record()]).unwrap();
        assert_eq!(output[0].get("env"), Some("prod"));
        assert_eq!(output[1].get("env"), Some("prod"));
        assert_eq!(tag.applied(), 2);
        assert_eq!(tag.key(), "env");
        assert_eq!(tag.value(), "prod");
        assert_eq!(tag.describe(), "tag(annotate)");
    }

    #[test]
    fn overwriting_is_the_default() {
        let mut tag = Tag::new("env", "prod");
        let output = tag.apply(record().with_field("env", "staging")).unwrap();
        assert_eq!(output[0].get("env"), Some("prod"));
    }

    #[test]
    fn keep_existing_preserves_earlier_values() {
        let mut tag = Tag::new("env", "prod").keep_existing();
        let output = tag
            .apply_all(vec![record().with_field("env", "staging"), record()])
            .unwrap();
        assert_eq!(output[0].get("env"), Some("staging"));
        assert_eq!(output[1].get("env"), Some("prod"));
        assert_eq!(tag.applied(), 1);
        assert_eq!(
            tag.summarise(&StageReport::new(2, 2)),
            "tagged 1 record(s) with env=prod (2 passed)"
        );
    }
}
