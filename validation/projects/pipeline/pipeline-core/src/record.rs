//! The record type that flows through a pipeline.

use std::collections::BTreeMap;
use std::collections::btree_map;
use std::fmt;

use crate::error::PipelineError;
use crate::level::Level;

/// A single log event travelling through the pipeline.
///
/// Records carry a source, a severity, a human-readable message and a sorted
/// map of fields. Fields are kept sorted so that rendering and deduplication
/// are deterministic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    source: String,
    level: Level,
    message: String,
    fields: BTreeMap<String, String>,
}

impl Record {
    /// Builds a record with no fields.
    pub fn new(source: impl Into<String>, level: Level, message: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            level,
            message: message.into(),
            fields: BTreeMap::new(),
        }
    }

    /// Starts a [`RecordBuilder`].
    pub fn builder() -> RecordBuilder {
        RecordBuilder::new()
    }

    /// The service or file the record came from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The severity of the record.
    pub fn level(&self) -> Level {
        self.level
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Iterates over the fields as `(key, value)` pairs.
    pub fn fields_iter(&self) -> Fields<'_> {
        Fields::new(&self.fields)
    }

    /// Iterates over the words of the message with their position.
    pub fn words(&self) -> Words<'_> {
        Words::new(&self.message)
    }

    /// The value of `key`, if the record carries that field.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    /// Whether the record carries `key`.
    pub fn has_field(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }

    /// How many fields the record carries.
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }

    /// Sets `key` to `value`, replacing any previous value.
    pub fn set_field(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.fields.insert(key.into(), value.into());
    }

    /// Removes `key`, returning the previous value.
    pub fn remove_field(&mut self, key: &str) -> Option<String> {
        self.fields.remove(key)
    }

    /// Replaces the severity.
    pub fn set_level(&mut self, level: Level) {
        self.level = level;
    }

    /// Replaces the message.
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    /// Raises the severity to at least `floor`, returning whether it changed.
    pub fn raise_level(&mut self, floor: Level) -> bool {
        if self.level < floor {
            self.level = floor;
            true
        } else {
            false
        }
    }

    /// Builder-style [`Record::set_field`].
    pub fn with_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.set_field(key, value);
        self
    }

    /// Builder-style [`Record::set_level`].
    pub fn with_level(mut self, level: Level) -> Self {
        self.set_level(level);
        self
    }

    /// How many words the message contains.
    pub fn word_count(&self) -> usize {
        self.words().count()
    }

    /// A stable identity used by deduplication stages.
    ///
    /// Two records with the same source, level, message and fields share a
    /// signature; the order of fields does not matter because the map is sorted.
    pub fn signature(&self) -> String {
        let mut signature = String::with_capacity(48);
        signature.push_str(&self.source);
        signature.push('|');
        signature.push_str(self.level.as_str());
        signature.push('|');
        signature.push_str(&self.message);
        for (key, value) in self.fields_iter() {
            signature.push('|');
            signature.push_str(key);
            signature.push('=');
            signature.push_str(value);
        }
        signature
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} {}",
            self.level.badge(),
            self.source,
            self.message
        )?;
        for (key, value) in self.fields_iter() {
            write!(f, " {key}={value}")?;
        }
        Ok(())
    }
}

/// Iterator over the fields of a [`Record`], yielding `(key, value)` pairs.
#[derive(Clone, Debug)]
pub struct Fields<'a> {
    inner: btree_map::Iter<'a, String, String>,
}

impl<'a> Fields<'a> {
    fn new(fields: &'a BTreeMap<String, String>) -> Self {
        Self {
            inner: fields.iter(),
        }
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for Fields<'_> {}

/// Iterator over the words of a message, yielding `(position, word)` pairs.
#[derive(Clone, Debug)]
pub struct Words<'a> {
    rest: &'a str,
    index: usize,
}

impl<'a> Words<'a> {
    fn new(message: &'a str) -> Self {
        Self {
            rest: message,
            index: 0,
        }
    }
}

impl<'a> Iterator for Words<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.rest.as_bytes();
        let mut start = 0;
        while start < bytes.len() && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        if start >= bytes.len() {
            self.rest = "";
            return None;
        }
        let mut end = start;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        let word = &self.rest[start..end];
        self.rest = &self.rest[end..];
        let index = self.index;
        self.index += 1;
        Some((index, word))
    }
}

/// Builder for [`Record`]s, used where a record is assembled from parts.
#[derive(Clone, Debug, Default)]
pub struct RecordBuilder {
    source: Option<String>,
    level: Option<Level>,
    message: Option<String>,
    fields: BTreeMap<String, String>,
}

impl RecordBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the source of the record.
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Sets the severity of the record.
    pub fn level(mut self, level: Level) -> Self {
        self.level = Some(level);
        self
    }

    /// Sets the severity from a label such as `warn` or `40`.
    pub fn level_label(self, label: &str) -> Result<Self, PipelineError> {
        let level = Level::from_label(label)?;
        Ok(self.level(level))
    }

    /// Sets the message of the record.
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Adds one field.
    pub fn field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.insert(key.into(), value.into());
        self
    }

    /// Adds many fields.
    pub fn fields<I, K, V>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (key, value) in fields {
            self.fields.insert(key.into(), value.into());
        }
        self
    }

    /// Finishes the record, failing when a mandatory part is missing.
    pub fn build(self) -> Result<Record, PipelineError> {
        let source = self
            .source
            .ok_or_else(|| PipelineError::missing("record-builder", "source"))?;
        let level = self
            .level
            .ok_or_else(|| PipelineError::missing("record-builder", "level"))?;
        let message = self
            .message
            .ok_or_else(|| PipelineError::missing("record-builder", "message"))?;
        Ok(Record {
            source,
            level,
            message,
            fields: self.fields,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Record {
        Record::new("api", Level::Warn, "slow request handled")
            .with_field("status", "504")
            .with_field("path", "/orders")
    }

    #[test]
    fn accessors_expose_the_record() {
        let record = sample();
        assert_eq!(record.source(), "api");
        assert_eq!(record.level(), Level::Warn);
        assert_eq!(record.message(), "slow request handled");
        assert_eq!(record.get("status"), Some("504"));
        assert_eq!(record.get("missing"), None);
        assert!(record.has_field("path"));
        assert_eq!(record.field_count(), 2);
    }

    #[test]
    fn fields_are_sorted_alphabetically() {
        let record = Record::new("api", Level::Info, "x")
            .with_field("zulu", "1")
            .with_field("alpha", "2");
        let pairs: Vec<(&str, &str)> = record.fields_iter().collect();
        assert_eq!(pairs, vec![("alpha", "2"), ("zulu", "1")]);
        assert_eq!(record.fields_iter().len(), 2);
    }

    #[test]
    fn mutations_and_removal() {
        let mut record = sample();
        record.set_field("status", "500");
        assert_eq!(record.get("status"), Some("500"));
        assert_eq!(record.remove_field("path"), Some("/orders".to_string()));
        assert_eq!(record.remove_field("path"), None);
        record.set_level(Level::Fatal);
        record.set_message("boom");
        assert_eq!(record.to_string(), "[FATAL] api boom status=500");
    }

    #[test]
    fn raising_the_level_only_moves_upwards() {
        let mut record = Record::new("worker", Level::Warn, "retrying");
        assert!(record.raise_level(Level::Error));
        assert_eq!(record.level(), Level::Error);
        assert!(!record.raise_level(Level::Debug));
        assert_eq!(record.level(), Level::Error);
    }

    #[test]
    fn words_carry_positions() {
        let record = Record::new("api", Level::Info, "  alpha beta   gamma ");
        let words: Vec<(usize, &str)> = record.words().collect();
        assert_eq!(words, vec![(0, "alpha"), (1, "beta"), (2, "gamma")]);
        assert_eq!(record.word_count(), 3);
        let empty = Record::new("api", Level::Info, "   ");
        assert_eq!(empty.word_count(), 0);
    }

    #[test]
    fn signature_is_order_independent() {
        let first = Record::new("api", Level::Info, "ok")
            .with_field("b", "2")
            .with_field("a", "1");
        let second = Record::new("api", Level::Info, "ok")
            .with_field("a", "1")
            .with_field("b", "2");
        assert_eq!(first.signature(), second.signature());
        assert_eq!(first.signature(), "api|info|ok|a=1|b=2");
        let different = second.with_level(Level::Warn);
        assert_ne!(first.signature(), different.signature());
    }

    #[test]
    fn display_lists_fields() {
        assert_eq!(
            sample().to_string(),
            "[WARN] api slow request handled path=/orders status=504"
        );
    }

    #[test]
    fn builder_assembles_and_validates() {
        let built = Record::builder()
            .source("billing")
            .level_label("40")
            .unwrap()
            .message("invoice paid")
            .fields([("invoice", "7"), ("amount", "12")])
            .field("currency", "EUR")
            .build()
            .unwrap();
        assert_eq!(
            built.to_string(),
            "[WARN] billing invoice paid amount=12 currency=EUR invoice=7"
        );

        let missing_source = Record::builder().build().unwrap_err();
        assert_eq!(
            missing_source,
            PipelineError::missing("record-builder", "source")
        );
        assert_eq!(
            missing_source.to_string(),
            "stage `record-builder` requires field `source`"
        );

        let missing_level = Record::builder().source("api").build().unwrap_err();
        assert_eq!(
            missing_level,
            PipelineError::missing("record-builder", "level")
        );

        let bad_level = Record::builder().level_label("shout").unwrap_err();
        assert_eq!(bad_level, PipelineError::parse("level", "shout"));
    }
}
