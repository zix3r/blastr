//! The `normalize-level` stage: folds the `level` field back into the record.

use pipeline_core::{Level, PipelineError, Record, Stage, StageKind, StageReport};

/// Moves a textual `level` field onto the record's severity.
///
/// Upstream systems disagree about severity spellings, so many pipelines carry
/// the original label in a field. This stage parses it, removes the field,
/// keeps the raw label for auditing and counts how often the record was
/// promoted.
#[derive(Clone, Debug, Default)]
pub struct NormalizeLevel {
    normalised: usize,
    promoted: usize,
}

impl NormalizeLevel {
    /// A fresh stage.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many records carried a level field.
    pub fn normalised(&self) -> usize {
        self.normalised
    }

    /// How many records were raised to a more severe level.
    pub fn promoted(&self) -> usize {
        self.promoted
    }
}

impl Stage for NormalizeLevel {
    fn name(&self) -> &str {
        "normalize-level"
    }

    fn kind(&self) -> StageKind {
        StageKind::Annotate
    }

    fn apply(&mut self, mut record: Record) -> Result<Vec<Record>, PipelineError> {
        let Some(raw) = record.get("level").map(str::to_owned) else {
            return Ok(vec![record]);
        };
        let level = Level::from_label(&raw)?;
        record.remove_field("level");
        record.set_field("level_source", raw);
        let previous = record.level();
        if level > previous {
            record.raise_level(level);
            self.promoted += 1;
        } else {
            record.set_level(level);
        }
        self.normalised += 1;
        Ok(vec![record])
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!(
            "normalised {} record(s), promoted {}, kept {}",
            self.normalised, self.promoted, report.output
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(level: Level, raw: &str) -> Record {
        Record::new("api", level, "event").with_field("level", raw)
    }

    #[test]
    fn parses_and_removes_the_field() {
        let mut stage = NormalizeLevel::new();
        let output = stage.apply(record(Level::Info, "warning")).unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].level(), Level::Warn);
        assert_eq!(output[0].get("level"), None);
        assert_eq!(output[0].get("level_source"), Some("warning"));
        assert_eq!(stage.normalised(), 1);
        assert_eq!(stage.promoted(), 1);
    }

    #[test]
    fn promotion_is_counted() {
        let mut stage = NormalizeLevel::new();
        let output = stage.apply(record(Level::Info, "50")).unwrap();
        assert_eq!(output[0].level(), Level::Error);
        assert_eq!(stage.promoted(), 1);

        let demoted = stage.apply(record(Level::Error, "debug")).unwrap();
        assert_eq!(demoted[0].level(), Level::Debug);
        assert_eq!(stage.promoted(), 1);
        assert_eq!(stage.normalised(), 2);
    }

    #[test]
    fn records_without_a_level_pass_through() {
        let mut stage = NormalizeLevel::new();
        let plain = Record::new("api", Level::Warn, "event");
        let output = stage.apply(plain.clone()).unwrap();
        assert_eq!(output, vec![plain]);
        assert_eq!(stage.normalised(), 0);
    }

    #[test]
    fn unparseable_levels_are_errors() {
        let mut stage = NormalizeLevel::new();
        let error = stage.apply(record(Level::Info, "shout")).unwrap_err();
        assert_eq!(error, PipelineError::parse("level", "shout"));
        assert_eq!(
            stage.summarise(&StageReport::new(2, 2)),
            "normalised 0 record(s), promoted 0, kept 2"
        );
    }
}
