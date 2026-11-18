//! The `redact` stage: masks secrets in fields and in the message.

use std::collections::BTreeSet;

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

/// Replaces secret values with a mask.
///
/// The stage masks the values of a configurable set of fields and scrubs
/// `key=value` pairs embedded in the message. Every replacement is counted so
/// the run report can show how much was hidden.
#[derive(Clone, Debug)]
pub struct Redact {
    keys: BTreeSet<String>,
    mask: String,
    hits: usize,
}

impl Redact {
    /// The field names masked when no custom set is configured.
    pub const DEFAULT_KEYS: [&'static str; 3] = ["password", "token", "secret"];

    /// A redactor with the default keys and the `***` mask.
    pub fn new() -> Self {
        Self {
            keys: Self::DEFAULT_KEYS
                .iter()
                .map(|key| (*key).to_string())
                .collect(),
            mask: String::from("***"),
            hits: 0,
        }
    }

    /// Replaces the mask.
    pub fn with_mask(mut self, mask: impl Into<String>) -> Self {
        self.mask = mask.into();
        self
    }

    /// Replaces the set of masked keys.
    pub fn with_keys<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.keys = keys.into_iter().map(Into::into).collect();
        self
    }

    /// How many values were masked so far.
    pub fn hits(&self) -> usize {
        self.hits
    }

    fn scrub_message(&self, message: &str) -> (String, usize) {
        let mut parts: Vec<String> = Vec::new();
        let mut hits = 0;
        for token in message.split(' ') {
            match token.split_once('=') {
                Some((key, _value)) if self.keys.contains(key) => {
                    parts.push(format!("{key}={}", self.mask));
                    hits += 1;
                }
                _ => parts.push(token.to_string()),
            }
        }
        (parts.join(" "), hits)
    }
}

impl Default for Redact {
    fn default() -> Self {
        Self::new()
    }
}

impl Stage for Redact {
    fn name(&self) -> &str {
        "redact"
    }

    fn kind(&self) -> StageKind {
        StageKind::Map
    }

    fn apply(&mut self, mut record: Record) -> Result<Vec<Record>, PipelineError> {
        let mut masked = 0;
        for key in &self.keys {
            if record.has_field(key) {
                record.set_field(key.as_str(), self.mask.clone());
                masked += 1;
            }
        }
        let message = record.message().to_string();
        let (scrubbed, message_hits) = self.scrub_message(&message);
        if message_hits > 0 {
            record.set_message(scrubbed);
        }
        self.hits += masked + message_hits;
        Ok(vec![record])
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!(
            "{} secret(s) masked across {} record(s)",
            self.hits, report.input
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    #[test]
    fn masks_fields_and_counts_hits() {
        let mut redact = Redact::new();
        let record = Record::new("api", Level::Info, "login accepted")
            .with_field("password", "hunter2")
            .with_field("token", "abc")
            .with_field("user", "ada");
        let output = redact.apply(record).unwrap();
        assert_eq!(output[0].get("password"), Some("***"));
        assert_eq!(output[0].get("token"), Some("***"));
        assert_eq!(output[0].get("user"), Some("ada"));
        assert_eq!(redact.hits(), 2);
    }

    #[test]
    fn masks_secrets_inside_the_message() {
        let mut redact = Redact::new();
        let record = Record::new(
            "api",
            Level::Warn,
            "retry with token=abc123 and password=swordfish",
        );
        let output = redact.apply(record).unwrap();
        assert_eq!(output[0].message(), "retry with token=*** and password=***");
        assert_eq!(redact.hits(), 2);
        assert_eq!(
            redact.summarise(&StageReport::new(1, 1)),
            "2 secret(s) masked across 1 record(s)"
        );
    }

    #[test]
    fn custom_keys_and_masks() {
        let mut redact = Redact::new()
            .with_keys(["cookie", "authorization"])
            .with_mask("[redacted]");
        let record = Record::new("edge", Level::Info, "request cookie=xyz")
            .with_field("cookie", "xyz")
            .with_field("authorization", "Bearer abc")
            .with_field("password", "kept");
        let output = redact.apply(record).unwrap();
        assert_eq!(output[0].get("cookie"), Some("[redacted]"));
        assert_eq!(output[0].get("authorization"), Some("[redacted]"));
        assert_eq!(output[0].get("password"), Some("kept"));
        assert_eq!(output[0].message(), "request cookie=[redacted]");
        assert_eq!(redact.hits(), 3);
    }

    #[test]
    fn plain_records_are_untouched() {
        let mut redact = Redact::default();
        let record = Record::new("api", Level::Info, "nothing to hide").with_field("user", "ada");
        let output = redact.apply_all(vec![record.clone()]).unwrap();
        assert_eq!(output, vec![record]);
        assert_eq!(redact.hits(), 0);
    }
}
