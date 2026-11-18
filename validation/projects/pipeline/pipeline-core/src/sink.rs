//! Outputs that consume the records leaving a pipeline.

use std::collections::BTreeMap;

use crate::error::PipelineError;
use crate::level::Level;
use crate::record::Record;

/// A destination for records.
///
/// Sinks are almost always used through `&mut dyn Sink`, so the pipeline can
/// write into a collector, a counter or a formatted text buffer without being
/// generic over the destination.
pub trait Sink {
    /// A name used in reports.
    fn label(&self) -> &str;

    /// How many records the sink accepted so far.
    fn written(&self) -> usize;

    /// Accepts one record.
    fn emit(&mut self, record: &Record) -> Result<(), PipelineError>;

    /// Finalises buffered output, returning how many records were pending.
    ///
    /// The default is a no-op for sinks that write through immediately.
    fn flush(&mut self) -> Result<usize, PipelineError> {
        Ok(0)
    }

    /// The maximum number of records the sink can hold.
    ///
    /// The default means "unbounded".
    fn capacity(&self) -> usize {
        usize::MAX
    }
}

/// Renders a sink's label and progress from behind a shared trait object.
pub fn describe_sink(sink: &dyn Sink) -> String {
    format!("{}: {} record(s)", sink.label(), sink.written())
}

/// A sink that keeps every record it receives.
#[derive(Clone, Debug)]
pub struct CollectSink {
    label: String,
    records: Vec<Record>,
    pending: usize,
}

impl CollectSink {
    /// A collector with the given label.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            records: Vec::new(),
            pending: 0,
        }
    }

    /// The records collected so far.
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// The messages of the collected records.
    pub fn messages(&self) -> Vec<&str> {
        self.records.iter().map(Record::message).collect()
    }

    /// Consumes the sink and returns the records.
    pub fn into_records(self) -> Vec<Record> {
        self.records
    }
}

impl Default for CollectSink {
    fn default() -> Self {
        Self::new("collect")
    }
}

impl Sink for CollectSink {
    fn label(&self) -> &str {
        &self.label
    }

    fn written(&self) -> usize {
        self.records.len()
    }

    fn emit(&mut self, record: &Record) -> Result<(), PipelineError> {
        self.records.push(record.clone());
        self.pending += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<usize, PipelineError> {
        let pending = self.pending;
        self.pending = 0;
        Ok(pending)
    }
}

/// A sink that only counts records; it uses the default `flush`.
#[derive(Clone, Debug, Default)]
pub struct CountingSink {
    count: usize,
}

impl CountingSink {
    /// An empty counter.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many records were emitted.
    pub fn count(&self) -> usize {
        self.count
    }
}

impl Sink for CountingSink {
    fn label(&self) -> &str {
        "counting"
    }

    fn written(&self) -> usize {
        self.count
    }

    fn emit(&mut self, _record: &Record) -> Result<(), PipelineError> {
        self.count += 1;
        Ok(())
    }
}

/// A sink that counts records per severity.
#[derive(Clone, Debug, Default)]
pub struct TallySink {
    counts: BTreeMap<Level, usize>,
    total: usize,
}

impl TallySink {
    /// An empty tally.
    pub fn new() -> Self {
        Self::default()
    }

    /// The counts per severity, most severe levels last.
    pub fn counts(&self) -> &BTreeMap<Level, usize> {
        &self.counts
    }

    /// How many records carry `level`.
    pub fn count_of(&self, level: Level) -> usize {
        self.counts.get(&level).copied().unwrap_or(0)
    }
}

impl Sink for TallySink {
    fn label(&self) -> &str {
        "tally"
    }

    fn written(&self) -> usize {
        self.total
    }

    fn emit(&mut self, record: &Record) -> Result<(), PipelineError> {
        *self.counts.entry(record.level()).or_insert(0) += 1;
        self.total += 1;
        Ok(())
    }

    fn capacity(&self) -> usize {
        1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_trait_objects_expose_progress() {
        let mut sink = CountingSink::new();
        sink.emit(&Record::new("api", Level::Info, "x")).unwrap();
        let shared: &dyn Sink = &sink;
        assert_eq!(describe_sink(shared), "counting: 1 record(s)");
        assert_eq!(
            describe_sink(&CollectSink::default()),
            "collect: 0 record(s)"
        );
    }

    fn sample() -> Vec<Record> {
        vec![
            Record::new("api", Level::Debug, "cache hit"),
            Record::new("api", Level::Error, "upstream down"),
            Record::new("worker", Level::Error, "job failed"),
        ]
    }

    #[test]
    fn collect_sink_keeps_records_and_reports_flushes() {
        let mut sink = CollectSink::new("unit");
        assert_eq!(sink.label(), "unit");
        assert_eq!(sink.capacity(), usize::MAX);
        for record in sample() {
            sink.emit(&record).unwrap();
        }
        assert_eq!(sink.written(), 3);
        assert_eq!(sink.flush().unwrap(), 3);
        assert_eq!(sink.flush().unwrap(), 0);
        assert_eq!(
            sink.messages(),
            vec!["cache hit", "upstream down", "job failed"]
        );
        assert_eq!(sink.into_records().len(), 3);
        assert_eq!(CollectSink::default().label(), "collect");
    }

    #[test]
    fn counting_sink_uses_the_default_flush() {
        let mut sink = CountingSink::new();
        for record in sample() {
            sink.emit(&record).unwrap();
        }
        assert_eq!(sink.count(), 3);
        assert_eq!(sink.written(), 3);
        assert_eq!(sink.flush().unwrap(), 0);
        assert_eq!(sink.label(), "counting");
    }

    #[test]
    fn tally_sink_groups_by_level() {
        let mut sink = TallySink::new();
        for record in sample() {
            sink.emit(&record).unwrap();
        }
        assert_eq!(sink.count_of(Level::Error), 2);
        assert_eq!(sink.count_of(Level::Debug), 1);
        assert_eq!(sink.count_of(Level::Info), 0);
        assert_eq!(sink.written(), 3);
        assert_eq!(sink.capacity(), 1024);
        assert_eq!(sink.counts().len(), 2);
    }
}
