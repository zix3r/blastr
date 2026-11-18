//! The `dedup` stage: keeps one record per signature.
//!
//! Unlike most stages this one looks at the whole batch, so it overrides
//! [`Stage::apply_all`] instead of relying on the default record-by-record
//! implementation.

use std::collections::BTreeMap;

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

/// Drops repeated records.
///
/// Inside one batch the *last* occurrence of a signature is kept, so that the
/// most recent version of an event wins while its position stays stable.
/// Signatures seen in earlier batches are dropped as well, which makes the
/// stage stateful across runs of the same pipeline.
#[derive(Clone, Debug, Default)]
pub struct Dedup {
    seen: std::collections::BTreeSet<String>,
    dropped: usize,
}

impl Dedup {
    /// A deduplicator with an empty memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many records were dropped so far.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// How many distinct signatures have been seen.
    pub fn distinct(&self) -> usize {
        self.seen.len()
    }
}

impl Stage for Dedup {
    fn name(&self) -> &str {
        "dedup"
    }

    fn kind(&self) -> StageKind {
        StageKind::Reduce
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        if self.seen.insert(record.signature()) {
            Ok(vec![record])
        } else {
            self.dropped += 1;
            Ok(Vec::new())
        }
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!(
            "dedup dropped {} of {} record(s), {} distinct",
            self.dropped,
            report.input,
            self.seen.len()
        )
    }

    fn apply_all(&mut self, records: Vec<Record>) -> Result<Vec<Record>, PipelineError> {
        let mut last_position: BTreeMap<String, usize> = BTreeMap::new();
        for (position, record) in records.iter().enumerate() {
            last_position.insert(record.signature(), position);
        }

        let mut kept = Vec::with_capacity(records.len());
        for (position, record) in records.into_iter().enumerate() {
            let signature = record.signature();
            let is_last = last_position.get(&signature).copied() == Some(position);
            let is_new = !self.seen.contains(&signature);
            if is_last && is_new {
                self.seen.insert(signature);
                kept.push(record);
            } else {
                self.dropped += 1;
            }
        }
        Ok(kept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    fn record(message: &str) -> Record {
        Record::new("api", Level::Info, message)
    }

    #[test]
    fn keeps_the_last_occurrence_of_a_batch() {
        let mut dedup = Dedup::new();
        let batch = vec![
            record("first").with_field("attempt", "1"),
            record("other"),
            record("first").with_field("attempt", "2"),
            record("first").with_field("attempt", "2"),
        ];
        let kept = dedup.apply_all(batch).unwrap();
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0].get("attempt"), Some("1"));
        assert_eq!(kept[1].message(), "other");
        assert_eq!(kept[2].get("attempt"), Some("2"));
        assert_eq!(dedup.dropped(), 1);
        assert_eq!(dedup.distinct(), 3);
    }

    #[test]
    fn remembers_signatures_across_batches() {
        let mut dedup = Dedup::new();
        assert_eq!(dedup.apply_all(vec![record("once")]).unwrap().len(), 1);
        assert!(dedup.apply_all(vec![record("once")]).unwrap().is_empty());
        assert_eq!(dedup.dropped(), 1);
        assert_eq!(dedup.distinct(), 1);
    }

    #[test]
    fn single_record_path_drops_duplicates() {
        let mut dedup = Dedup::new();
        assert_eq!(dedup.apply(record("once")).unwrap().len(), 1);
        assert!(dedup.apply(record("once")).unwrap().is_empty());
        assert_eq!(
            dedup.summarise(&StageReport::new(3, 1)),
            "dedup dropped 1 of 3 record(s), 1 distinct"
        );
    }

    #[test]
    fn distinct_fields_make_distinct_records() {
        let mut dedup = Dedup::new();
        let batch = vec![
            record("paid").with_field("invoice", "1"),
            record("paid").with_field("invoice", "2"),
        ];
        assert_eq!(dedup.apply_all(batch).unwrap().len(), 2);
        assert_eq!(dedup.dropped(), 0);
    }
}
