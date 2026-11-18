//! The `tap` stage: counts records per level without changing them.

use std::collections::BTreeMap;

use pipeline_core::{Level, PipelineError, Record, Stage, StageKind, StageReport};

/// Passes every record through and tallies the severity.
///
/// The stage is a cheap observability hook: it never filters, it only keeps
/// counts that the report can show. Its [`Stage::describe`] override includes
/// the running total, so the description changes as records flow.
#[derive(Clone, Debug, Default)]
pub struct Tap {
    counts: BTreeMap<Level, usize>,
}

impl Tap {
    /// A fresh tap.
    pub fn new() -> Self {
        Self::default()
    }

    /// The counts per severity.
    pub fn counts(&self) -> &BTreeMap<Level, usize> {
        &self.counts
    }

    /// How many records were seen.
    pub fn total(&self) -> usize {
        self.counts.values().sum()
    }

    /// How many records carried `level`.
    pub fn count_of(&self, level: Level) -> usize {
        self.counts.get(&level).copied().unwrap_or(0)
    }
}

impl Stage for Tap {
    fn name(&self) -> &str {
        "tap"
    }

    fn kind(&self) -> StageKind {
        StageKind::Annotate
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        *self.counts.entry(record.level()).or_insert(0) += 1;
        Ok(vec![record])
    }

    fn summarise(&self, report: &StageReport) -> String {
        let mut parts: Vec<String> = Vec::new();
        for level in Level::ALL {
            let count = self.count_of(level);
            if count > 0 {
                parts.push(format!("{}={count}", level.as_str()));
            }
        }
        format!(
            "tap saw {} record(s) of {}: {}",
            self.total(),
            report.input,
            parts.join(",")
        )
    }

    fn describe(&self) -> String {
        format!("tap({} records seen)", self.total())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(level: Level) -> Record {
        Record::new("api", level, "event")
    }

    #[test]
    fn counts_records_but_passes_them_through() {
        let mut tap = Tap::new();
        let records = vec![
            record(Level::Info),
            record(Level::Error),
            record(Level::Info),
            record(Level::Error),
            record(Level::Info),
        ];
        let output = tap.apply_all(records).unwrap();
        assert_eq!(output.len(), 5);
        assert_eq!(tap.total(), 5);
        assert_eq!(tap.count_of(Level::Info), 3);
        assert_eq!(tap.count_of(Level::Error), 2);
        assert_eq!(tap.count_of(Level::Warn), 0);
        assert_eq!(
            tap.counts().keys().copied().collect::<Vec<_>>(),
            vec![Level::Info, Level::Error]
        );
    }

    #[test]
    fn description_and_summary_follow_the_counts() {
        let mut tap = Tap::new();
        assert_eq!(tap.describe(), "tap(0 records seen)");
        tap.apply_all(vec![record(Level::Warn), record(Level::Warn)])
            .unwrap();
        assert_eq!(tap.describe(), "tap(2 records seen)");
        assert_eq!(
            tap.summarise(&StageReport::new(2, 2)),
            "tap saw 2 record(s) of 2: warn=2"
        );
        assert_eq!(tap.kind(), StageKind::Annotate);
    }

    #[test]
    fn empty_input_is_harmless() {
        let mut tap = Tap::new();
        assert!(tap.apply_all(Vec::new()).unwrap().is_empty());
        assert_eq!(tap.total(), 0);
        assert_eq!(
            tap.summarise(&StageReport::new(0, 0)),
            "tap saw 0 record(s) of 0: "
        );
    }
}
