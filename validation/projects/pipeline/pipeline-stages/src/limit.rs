//! The `limit` stage: keeps at most `max` records.

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

/// Keeps the first `max` records and drops the rest.
///
/// The counter is cumulative: a pipeline that runs the same stage twice keeps
/// counting, so the second batch may already be empty.
#[derive(Clone, Debug)]
pub struct Limit {
    max: usize,
    kept: usize,
}

impl Limit {
    /// A limiter for `max` records.
    pub fn new(max: usize) -> Self {
        Self { max, kept: 0 }
    }

    /// The configured bound.
    pub fn max(&self) -> usize {
        self.max
    }

    /// How many records were kept so far.
    pub fn kept(&self) -> usize {
        self.kept
    }

    /// How many records are still allowed.
    pub fn remaining(&self) -> usize {
        self.max.saturating_sub(self.kept)
    }
}

impl Stage for Limit {
    fn name(&self) -> &str {
        "limit"
    }

    fn kind(&self) -> StageKind {
        StageKind::Filter
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        if self.kept < self.max {
            self.kept += 1;
            Ok(vec![record])
        } else {
            Ok(Vec::new())
        }
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!(
            "limit {} kept {} of {} record(s), {} left",
            self.max,
            self.kept,
            report.input,
            self.remaining()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    fn records(count: usize) -> Vec<Record> {
        (0..count)
            .map(|index| Record::new("api", Level::Info, format!("event {index}")))
            .collect()
    }

    #[test]
    fn keeps_only_the_first_records() {
        let mut limit = Limit::new(2);
        let output = limit.apply_all(records(5)).unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[1].message(), "event 1");
        assert_eq!(limit.kept(), 2);
        assert_eq!(limit.remaining(), 0);
        assert_eq!(limit.max(), 2);
    }

    #[test]
    fn the_bound_is_cumulative_across_batches() {
        let mut limit = Limit::new(3);
        assert_eq!(limit.apply_all(records(2)).unwrap().len(), 2);
        assert_eq!(limit.apply_all(records(2)).unwrap().len(), 1);
        assert!(limit.apply_all(records(1)).unwrap().is_empty());
        assert_eq!(limit.kept(), 3);
        assert_eq!(
            limit.summarise(&StageReport::new(2, 1)),
            "limit 3 kept 3 of 2 record(s), 0 left"
        );
    }

    #[test]
    fn a_zero_limit_drops_everything() {
        let mut limit = Limit::new(0);
        assert!(limit.apply_all(records(3)).unwrap().is_empty());
        assert_eq!(limit.remaining(), 0);
        assert_eq!(limit.apply(records(1).remove(0)).unwrap().len(), 0);
    }
}
