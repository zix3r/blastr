//! The `enrich` stage: derives fields from the source and the message.

use std::collections::BTreeMap;

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

/// Adds service names and message statistics.
///
/// The stage maps a record's source to a service name (falling back to
/// `unknown`), counts the words of the message and flags empty messages. It
/// uses the custom [`Record::words`] iterator, so punctuation is respected as
/// part of the word as written.
#[derive(Clone, Debug, Default)]
pub struct Enrich {
    services: BTreeMap<String, String>,
}

impl Enrich {
    /// An enricher without a service map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `source -> service` pairs.
    pub fn with_services<I, K, V>(mut self, services: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (source, service) in services {
            self.services.insert(source.into(), service.into());
        }
        self
    }

    /// The service a source belongs to, or `unknown`.
    pub fn service_for(&self, source: &str) -> &str {
        self.services
            .get(source)
            .map(String::as_str)
            .unwrap_or("unknown")
    }
}

impl Stage for Enrich {
    fn name(&self) -> &str {
        "enrich"
    }

    fn kind(&self) -> StageKind {
        StageKind::Annotate
    }

    fn apply(&mut self, mut record: Record) -> Result<Vec<Record>, PipelineError> {
        let service = self.service_for(record.source()).to_string();
        let words = record.word_count();
        let empty = words == 0;
        record.set_field("service", service);
        record.set_field("words", words.to_string());
        if empty {
            record.set_field("empty", "true");
        }
        Ok(vec![record])
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!(
            "enriched {} record(s) with {} known service(s)",
            report.output,
            self.services.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    #[test]
    fn derives_service_and_word_count() {
        let mut enrich = Enrich::new().with_services([("api", "gateway"), ("worker", "jobs")]);
        let output = enrich
            .apply(Record::new("api", Level::Info, "two words"))
            .unwrap();
        assert_eq!(output[0].get("service"), Some("gateway"));
        assert_eq!(output[0].get("words"), Some("2"));
        assert_eq!(output[0].get("empty"), None);
        assert_eq!(enrich.service_for("api"), "gateway");
        assert_eq!(enrich.service_for("cron"), "unknown");
    }

    #[test]
    fn empty_messages_get_a_flag() {
        let mut enrich = Enrich::new();
        let output = enrich
            .apply(Record::new("mystery", Level::Warn, "   "))
            .unwrap();
        assert_eq!(output[0].get("empty"), Some("true"));
        assert_eq!(output[0].get("words"), Some("0"));
        assert_eq!(
            enrich.summarise(&StageReport::new(1, 1)),
            "enriched 1 record(s) with 0 known service(s)"
        );
    }

    #[test]
    fn enrichment_is_stable_across_runs() {
        let mut enrich = Enrich::new().with_services([("api", "gateway")]);
        let first = enrich.apply(Record::new("api", Level::Info, "a")).unwrap();
        let second = enrich.apply(Record::new("api", Level::Info, "a")).unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].field_count(), 2);
        assert_eq!(first[0].get("service"), Some("gateway"));
    }
}
