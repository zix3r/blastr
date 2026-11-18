//! A [`Sink`] that renders records as text lines.

use pipeline_core::{PipelineError, Record, Sink};

/// Collects records as formatted lines.
///
/// The sink lives outside `pipeline-core`, which keeps the core free of any
/// presentation decisions; the pipeline only sees it as `&mut dyn Sink`.
#[derive(Clone, Debug)]
pub struct TextSink {
    label: String,
    lines: Vec<String>,
    pending: usize,
}

impl TextSink {
    /// The largest number of lines the sink accepts before it reports
    /// [`Sink::capacity`] being reached.
    pub const CAPACITY: usize = 4096;

    /// A sink with the given label.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            lines: Vec::new(),
            pending: 0,
        }
    }

    /// The lines written so far.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Consumes the sink and returns the lines.
    pub fn into_lines(self) -> Vec<String> {
        self.lines
    }

    /// The lines joined with newlines.
    pub fn joined(&self) -> String {
        self.lines.join("\n")
    }
}

impl Default for TextSink {
    fn default() -> Self {
        Self::new("text")
    }
}

/// Renders one record as `source BADGE message key=value ...`.
pub fn render_record(record: &Record) -> String {
    let mut line = String::with_capacity(record.message().len() + 24);
    line.push_str(record.source());
    line.push(' ');
    line.push_str(record.level().badge());
    line.push(' ');
    line.push_str(record.message());
    for (key, value) in record.fields_iter() {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(value);
    }
    line
}

impl Sink for TextSink {
    fn label(&self) -> &str {
        &self.label
    }

    fn written(&self) -> usize {
        self.lines.len()
    }

    fn emit(&mut self, record: &Record) -> Result<(), PipelineError> {
        if self.lines.len() >= Self::CAPACITY {
            return Err(PipelineError::limit(Self::CAPACITY, self.lines.len() + 1));
        }
        self.lines.push(render_record(record));
        self.pending += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<usize, PipelineError> {
        let pending = self.pending;
        self.pending = 0;
        Ok(pending)
    }

    fn capacity(&self) -> usize {
        Self::CAPACITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    fn record() -> Record {
        Record::new("api", Level::Warn, "slow request")
            .with_field("status", "504")
            .with_field("path", "/orders")
    }

    #[test]
    fn renders_source_badge_message_and_fields() {
        assert_eq!(
            render_record(&record()),
            "api WARN slow request path=/orders status=504"
        );
        let plain = Record::new("worker", Level::Fatal, "dead");
        assert_eq!(render_record(&plain), "worker FATAL dead");
    }

    #[test]
    fn sink_buffers_lines_and_reports_flushes() {
        let mut sink = TextSink::new("unit");
        assert_eq!(sink.label(), "unit");
        assert_eq!(sink.capacity(), 4096);
        sink.emit(&record()).unwrap();
        sink.emit(&Record::new("api", Level::Info, "ok")).unwrap();
        assert_eq!(sink.written(), 2);
        assert_eq!(sink.flush().unwrap(), 2);
        assert_eq!(sink.flush().unwrap(), 0);
        assert_eq!(
            sink.joined(),
            "api WARN slow request path=/orders status=504\napi INFO ok"
        );
        assert_eq!(sink.into_lines().len(), 2);
        assert_eq!(TextSink::default().label(), "text");
    }

    #[test]
    fn capacity_is_enforced() {
        let mut sink = TextSink::new("small");
        let record = Record::new("api", Level::Info, "event");
        for _ in 0..TextSink::CAPACITY {
            sink.emit(&record).unwrap();
        }
        assert_eq!(
            sink.emit(&record).unwrap_err(),
            PipelineError::limit(TextSink::CAPACITY, TextSink::CAPACITY + 1)
        );
    }
}
