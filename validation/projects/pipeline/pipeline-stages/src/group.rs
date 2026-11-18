//! The `group` stage: counts records per partition without changing them.

use std::collections::BTreeMap;

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

use crate::partition::Partition;

/// Counts records per bucket, passing every record through untouched.
///
/// The stage is generic over the [`Partition`], and the bucket map is keyed by
/// the partition's associated key type, so the compiler knows exactly what a
/// bucket is.
#[derive(Clone, Debug)]
pub struct Group<P: Partition> {
    partition: P,
    counts: BTreeMap<P::Key, usize>,
}

impl<P: Partition> Group<P> {
    /// Counts records by `partition`.
    pub fn new(partition: P) -> Self {
        Self {
            partition,
            counts: BTreeMap::new(),
        }
    }

    /// The counts per bucket.
    pub fn counts(&self) -> &BTreeMap<P::Key, usize> {
        &self.counts
    }

    /// How many distinct buckets were seen.
    pub fn buckets(&self) -> usize {
        self.counts.len()
    }
}

impl<P: Partition> Stage for Group<P> {
    fn name(&self) -> &str {
        "group"
    }

    fn kind(&self) -> StageKind {
        StageKind::Annotate
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        let key = self.partition.key(&record);
        *self.counts.entry(key).or_insert(0) += 1;
        Ok(vec![record])
    }

    fn summarise(&self, report: &StageReport) -> String {
        let buckets: Vec<String> = self
            .counts
            .iter()
            .map(|(key, count)| format!("{key}={count}"))
            .collect();
        format!(
            "group by {} found {} bucket(s) in {} record(s): {}",
            P::LABEL,
            buckets.len(),
            report.input,
            buckets.join(",")
        )
    }

    fn describe(&self) -> String {
        format!("group({})", P::LABEL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::partition::{ByField, ByLevel, BySource};
    use pipeline_core::Level;

    fn records() -> Vec<Record> {
        vec![
            Record::new("api", Level::Info, "one"),
            Record::new("worker", Level::Error, "two"),
            Record::new("api", Level::Info, "three"),
            Record::new("api", Level::Warn, "four"),
        ]
    }

    #[test]
    fn counts_by_source() {
        let mut group = Group::new(BySource);
        let output = group.apply_all(records()).unwrap();
        assert_eq!(output.len(), 4);
        assert_eq!(group.buckets(), 2);
        assert_eq!(group.counts().get("api"), Some(&3));
        assert_eq!(group.counts().get("worker"), Some(&1));
        assert_eq!(group.describe(), "group(source)");
        assert_eq!(
            group.summarise(&StageReport::new(4, 4)),
            "group by source found 2 bucket(s) in 4 record(s): api=3,worker=1"
        );
    }

    #[test]
    fn counts_by_level_in_severity_order() {
        let mut group = Group::new(ByLevel);
        group.apply_all(records()).unwrap();
        let keys: Vec<Level> = group.counts().keys().copied().collect();
        assert_eq!(keys, vec![Level::Info, Level::Warn, Level::Error]);
        assert_eq!(group.counts().get(&Level::Info), Some(&2));
        assert_eq!(
            group.summarise(&StageReport::new(4, 4)),
            "group by level found 3 bucket(s) in 4 record(s): info=2,warn=1,error=1"
        );
    }

    #[test]
    fn counts_by_field_with_fallback() {
        let mut group = Group::new(ByField::new("service"));
        let tagged = vec![
            Record::new("api", Level::Info, "one").with_field("service", "gateway"),
            Record::new("api", Level::Info, "two"),
        ];
        group.apply_all(tagged).unwrap();
        assert_eq!(group.counts().get("gateway"), Some(&1));
        assert_eq!(group.counts().get("unknown"), Some(&1));
        assert_eq!(group.buckets(), 2);
    }
}
