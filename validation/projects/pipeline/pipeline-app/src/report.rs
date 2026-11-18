//! Turns a configured spec and a log document into a rendered report.

use std::fmt;

use pipeline_core::{PipelineError, RunStats, read_records};
use pipeline_stages::TextSink;

use crate::config::build_pipeline;

/// The outcome of running a spec over a log document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The pipeline label.
    pub label: String,
    /// Counts for the run.
    pub stats: RunStats,
    /// One summary line per stage.
    pub summaries: Vec<String>,
    /// The rendered output records.
    pub lines: Vec<String>,
}

impl Report {
    /// Bundles the parts of a report.
    pub fn new(
        label: impl Into<String>,
        stats: RunStats,
        summaries: Vec<String>,
        lines: Vec<String>,
    ) -> Self {
        Self {
            label: label.into(),
            stats,
            summaries,
            lines,
        }
    }

    /// How many records survived the pipeline.
    pub fn record_count(&self) -> usize {
        self.lines.len()
    }

    /// The full textual report.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("pipeline {}\n", self.label));
        out.push_str(&format!("{}\n", self.stats.summary()));
        for detail in self.stats.detail() {
            out.push_str(&format!("  {detail}\n"));
        }
        for summary in &self.summaries {
            out.push_str(&format!("  * {summary}\n"));
        }
        out.push_str("---\n");
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

/// Builds the pipeline described by `spec`, reads `text` and runs it.
pub fn run_spec(spec: &str, text: &str) -> Result<Report, PipelineError> {
    let mut pipeline = build_pipeline(spec)?;
    let records = read_records(text)?;
    let mut sink = TextSink::new("report");
    let stats = pipeline.run_into(records, &mut sink)?;
    let summaries = pipeline.summaries(&stats);
    Ok(Report::new(
        pipeline.label().to_string(),
        stats,
        summaries,
        sink.into_lines(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Describe;

    const LOG: &str = "\
# two errors, only one should survive dedup
ERROR api upstream timeout status=504 path=/orders token=abc123
ERROR api upstream timeout status=504 path=/orders token=abc123
INFO api request finished status=200 path=/orders
";

    #[test]
    fn a_report_renders_its_sections_in_order() {
        let report = run_spec("dedup|redact|truncate:12", LOG).unwrap();
        assert_eq!(report.label, "dedup>redact>truncate");
        assert_eq!(report.record_count(), 2);
        assert_eq!(
            report.render(),
            "pipeline dedup>redact>truncate\n\
             3 in -> 2 out across 3 stage(s)\n  \
             dedup: 3 -> 2 (1 dropped)\n  \
             redact: 2 -> 2\n  \
             truncate: 2 -> 2\n  \
             * dedup dropped 1 of 3 record(s), 2 distinct\n  \
             * 1 secret(s) masked across 2 record(s)\n  \
             * 2 -> 2 [2 of 2 shortened to 12 chars]\n\
             ---\n\
             api ERROR upstream tim... path=/orders status=504 token=*** truncated=16\n\
             api INFO request fini... path=/orders status=200 truncated=16\n"
        );
        assert_eq!(report.summaries.len(), 3);
    }

    #[test]
    fn reports_describe_themselves() {
        let report = run_spec("dedup", LOG).unwrap();
        assert_eq!(report.headline(), "pipeline dedup");
        assert!(report.mentions("3 in -> 2 out"));
        assert_eq!(report.to_string(), report.render());
    }

    #[test]
    fn reading_errors_abort_before_the_run() {
        let error = run_spec("dedup", "not-a-record\n").unwrap_err();
        assert_eq!(error, PipelineError::parse("level", "not-a-record"));
    }

    #[test]
    fn stage_errors_abort_the_run() {
        let error = run_spec("validate:request_id", LOG).unwrap_err();
        assert_eq!(error, PipelineError::missing("validate", "request_id"));
    }
}
