//! The textual log format read by the pipeline.
//!
//! Every line looks like
//!
//! ```text
//! LEVEL source message words key=value key=value
//! ```
//!
//! Blank lines and lines starting with `#` are skipped.

use std::str::Lines;

use crate::error::PipelineError;
use crate::level::Level;
use crate::record::Record;

/// Parses one log line into a [`Record`].
///
/// The first token is the severity, the second the source. Message words are
/// collected until the first `key=value` token; everything after that must be a
/// field.
pub fn parse_line(line: &str) -> Result<Record, PipelineError> {
    let mut tokens = line.split_whitespace();
    let level_token = tokens
        .next()
        .ok_or_else(|| PipelineError::parse("level", ""))?;
    let level: Level = level_token.parse()?;
    let source = tokens
        .next()
        .ok_or_else(|| PipelineError::parse("source", line))?;

    let mut message: Vec<&str> = Vec::new();
    let mut fields: Vec<(&str, &str)> = Vec::new();
    for token in tokens {
        match token.split_once('=') {
            Some((key, value)) if !key.is_empty() => fields.push((key, value)),
            Some(_) => return Err(PipelineError::parse("field name", token)),
            None if fields.is_empty() => message.push(token),
            None => return Err(PipelineError::parse("field", token)),
        }
    }

    let mut record = Record::new(source, level, message.join(" "));
    for (key, value) in fields {
        record.set_field(key, value);
    }
    Ok(record)
}

/// Reads records from a multi-line log document.
///
/// The reader is an iterator of `Result`s, so a single malformed line aborts
/// the collection but callers can also drive it one line at a time.
#[derive(Clone, Debug)]
pub struct LineReader<'a> {
    lines: Lines<'a>,
    skipped: usize,
    malformed: usize,
}

impl<'a> LineReader<'a> {
    /// Reads the log document `text`.
    pub fn new(text: &'a str) -> Self {
        Self {
            lines: text.lines(),
            skipped: 0,
            malformed: 0,
        }
    }

    /// How many blank or comment lines were skipped so far.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// How many lines failed to parse so far.
    pub fn malformed(&self) -> usize {
        self.malformed
    }

    /// Reads every record, failing on the first malformed line.
    pub fn parse_all(self) -> Result<Vec<Record>, PipelineError> {
        self.collect()
    }
}

impl Iterator for LineReader<'_> {
    type Item = Result<Record, PipelineError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let line = self.lines.next()?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                self.skipped += 1;
                continue;
            }
            let parsed = parse_line(trimmed);
            if parsed.is_err() {
                self.malformed += 1;
            }
            return Some(parsed);
        }
    }
}

/// Reads every record of `text`.
pub fn read_records(text: &str) -> Result<Vec<Record>, PipelineError> {
    LineReader::new(text).collect::<Result<Vec<Record>, PipelineError>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_line() {
        let record = parse_line("WARN api slow request handled status=504 path=/orders").unwrap();
        assert_eq!(record.source(), "api");
        assert_eq!(record.level(), Level::Warn);
        assert_eq!(record.message(), "slow request handled");
        assert_eq!(record.get("status"), Some("504"));
        assert_eq!(record.get("path"), Some("/orders"));
        assert_eq!(
            record.to_string(),
            "[WARN] api slow request handled path=/orders status=504"
        );
    }

    #[test]
    fn parses_numeric_levels_and_empty_messages() {
        let record = parse_line("40 billing").unwrap();
        assert_eq!(record.level(), Level::Warn);
        assert_eq!(record.source(), "billing");
        assert_eq!(record.message(), "");
        assert_eq!(record.word_count(), 0);
    }

    #[test]
    fn rejects_broken_lines() {
        assert_eq!(
            parse_line("loud api boom"),
            Err(PipelineError::parse("level", "loud"))
        );
        assert_eq!(
            parse_line("INFO"),
            Err(PipelineError::parse("source", "INFO"))
        );
        assert_eq!(
            parse_line("INFO api =oops"),
            Err(PipelineError::parse("field name", "=oops"))
        );
        assert_eq!(
            parse_line("INFO api boom status=500 trailing"),
            Err(PipelineError::parse("field", "trailing"))
        );
    }

    #[test]
    fn reader_skips_comments_and_blanks() {
        let text = "\n# a comment\n\nINFO api started\n  \nERROR worker job failed code=17\n";
        let mut reader = LineReader::new(text);
        let first = reader.next().unwrap().unwrap();
        assert_eq!(first.message(), "started");
        let second = reader.next().unwrap().unwrap();
        assert_eq!(second.get("code"), Some("17"));
        assert!(reader.next().is_none());
        assert_eq!(reader.skipped(), 4);
        assert_eq!(reader.malformed(), 0);
    }

    #[test]
    fn reader_reports_malformed_lines_but_keeps_going() {
        let text = "INFO api fine\nnope api broken\nDEBUG api also fine";
        let mut reader = LineReader::new(text);
        assert!(reader.next().unwrap().is_ok());
        let broken = reader.next().unwrap();
        assert_eq!(broken, Err(PipelineError::parse("level", "nope")));
        assert!(reader.next().unwrap().is_ok());
        assert_eq!(reader.malformed(), 1);
        assert_eq!(reader.skipped(), 0);
    }

    #[test]
    fn read_records_collects_or_fails() {
        let records = read_records("INFO api one\nERROR api two\n").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].level(), Level::Error);
        assert_eq!(
            read_records("INFO api one\nbroken\n").unwrap_err(),
            PipelineError::parse("level", "broken")
        );
        assert!(read_records("").unwrap().is_empty());
        assert_eq!(
            LineReader::new("INFO api one").parse_all().unwrap().len(),
            1
        );
    }
}
