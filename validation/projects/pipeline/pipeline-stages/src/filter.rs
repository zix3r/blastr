//! The `filter` stage: keeps records that satisfy a predicate.

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

/// A boxed record predicate, used by [`Filter`].
pub type Predicate = Box<dyn Fn(&Record) -> bool>;

/// Drops records for which the predicate returns `false`.
pub struct Filter {
    label: String,
    keep: Predicate,
    rejected: usize,
}

impl Filter {
    /// Builds a stage from any `Fn(&Record) -> bool`.
    pub fn new(label: impl Into<String>, keep: impl Fn(&Record) -> bool + 'static) -> Self {
        Self {
            label: label.into(),
            keep: Box::new(keep),
            rejected: 0,
        }
    }

    /// Builds a stage from a plain function pointer such as `is_error`.
    pub fn from_fn(label: impl Into<String>, keep: fn(&Record) -> bool) -> Self {
        Self::new(label, keep)
    }

    /// How many records were dropped so far.
    pub fn rejected(&self) -> usize {
        self.rejected
    }
}

impl Stage for Filter {
    fn name(&self) -> &str {
        &self.label
    }

    fn kind(&self) -> StageKind {
        StageKind::Filter
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        if (self.keep)(&record) {
            Ok(vec![record])
        } else {
            self.rejected += 1;
            Ok(Vec::new())
        }
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!(
            "{} rejected {} of {} record(s)",
            self.label, self.rejected, report.input
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    fn records() -> Vec<Record> {
        vec![
            Record::new("api", Level::Info, "started"),
            Record::new("api", Level::Error, "boom"),
            Record::new("worker", Level::Fatal, "dead"),
        ]
    }

    #[test]
    fn keeps_matching_records() {
        let mut filter = Filter::new("errors", |record: &Record| record.level().is_failure());
        let output = filter.apply_all(records()).unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].message(), "boom");
        assert_eq!(filter.rejected(), 1);
        assert_eq!(filter.name(), "errors");
        assert_eq!(filter.describe(), "errors(filter)");
        assert_eq!(
            filter.summarise(&StageReport::new(3, 2)),
            "errors rejected 1 of 3 record(s)"
        );
    }

    #[test]
    fn rejecting_everything_yields_an_empty_batch() {
        let mut filter = Filter::new("none", |_record: &Record| false);
        assert!(filter.apply_all(records()).unwrap().is_empty());
        assert_eq!(filter.rejected(), 3);
    }

    #[test]
    fn predicates_can_inspect_fields() {
        let mut filter = Filter::new("status", |record: &Record| {
            record.get("status") == Some("500")
        });
        let records = vec![
            Record::new("api", Level::Error, "boom").with_field("status", "500"),
            Record::new("api", Level::Error, "boom").with_field("status", "503"),
        ];
        let output = filter.apply_all(records).unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].get("status"), Some("500"));
    }
}
