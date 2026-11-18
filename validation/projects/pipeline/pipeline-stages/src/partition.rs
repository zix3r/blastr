//! Grouping rules with associated keys.
//!
//! A [`Partition`] maps a record to a key. The trait carries compile-time
//! metadata (a label) and an associated key type, which lets the `group` stage
//! keep a `BTreeMap<Self::Key, _>` without boxing or erasing the key type.

use std::fmt;

use pipeline_core::{Level, Record};

/// A rule that decides which bucket a record belongs to.
pub trait Partition {
    /// The label used in reports, for example `source`.
    const LABEL: &'static str;

    /// The key records are grouped by. It must be orderable so counts render
    /// deterministically.
    type Key: Ord + Clone + fmt::Display;

    /// The key of `record`.
    fn key(&self, record: &Record) -> Self::Key;

    /// Whether two records fall into the same bucket.
    ///
    /// The default compares the keys, which is right for every partition whose
    /// key is cheap to compute.
    fn same_bucket(&self, left: &Record, right: &Record) -> bool {
        self.key(left) == self.key(right)
    }

    /// A one-line description of the rule.
    fn describe(&self) -> String {
        format!("by {}", Self::LABEL)
    }
}

/// Groups records by the service that emitted them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BySource;

impl Partition for BySource {
    const LABEL: &'static str = "source";
    type Key = String;

    fn key(&self, record: &Record) -> String {
        record.source().to_string()
    }
}

/// Groups records by severity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByLevel;

impl Partition for ByLevel {
    const LABEL: &'static str = "level";
    type Key = Level;

    fn key(&self, record: &Record) -> Level {
        record.level()
    }
}

/// Groups records by the value of one field, with an `unknown` fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ByField {
    field: String,
}

impl ByField {
    /// Groups by `field`.
    pub fn new(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
        }
    }

    /// The field that is read.
    pub fn field(&self) -> &str {
        &self.field
    }
}

impl Partition for ByField {
    const LABEL: &'static str = "field";
    type Key = String;

    fn key(&self, record: &Record) -> String {
        record.get(&self.field).unwrap_or("unknown").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(level: Level) -> Record {
        Record::new("api", level, "event")
    }

    #[test]
    fn sources_are_string_keys() {
        let partition = BySource;
        assert_eq!(BySource::LABEL, "source");
        assert_eq!(partition.key(&record(Level::Info)), "api");
        assert!(partition.same_bucket(&record(Level::Info), &record(Level::Error)));
        let other = Record::new("worker", Level::Info, "event");
        assert!(!partition.same_bucket(&record(Level::Info), &other));
        assert_eq!(partition.describe(), "by source");
    }

    #[test]
    fn levels_are_ordered_keys() {
        let partition = ByLevel;
        assert_eq!(ByLevel::LABEL, "level");
        assert_eq!(partition.key(&record(Level::Warn)), Level::Warn);
        assert!(!partition.same_bucket(&record(Level::Warn), &record(Level::Info)));
        assert_eq!(partition.describe(), "by level");
    }

    #[test]
    fn fields_fall_back_to_unknown() {
        let partition = ByField::new("service");
        assert_eq!(ByField::LABEL, "field");
        assert_eq!(partition.field(), "service");
        assert_eq!(partition.key(&record(Level::Info)), "unknown");
        let tagged = record(Level::Info).with_field("service", "gateway");
        assert_eq!(partition.key(&tagged), "gateway");
        assert_eq!(partition.describe(), "by field");
    }
}
