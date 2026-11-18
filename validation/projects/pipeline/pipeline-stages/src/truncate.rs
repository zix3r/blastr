//! The `truncate` stage: shortens long messages.

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

/// Shortens messages longer than `width` characters.
///
/// Truncated records get a `truncated` field holding the original length and,
/// when enabled, the message is finished with `...`. The stage overrides
/// [`Stage::describe`] so the configuration shows in reports.
#[derive(Clone, Debug)]
pub struct Truncate {
    width: usize,
    ellipsis: bool,
    truncated: usize,
}

impl Truncate {
    /// The smallest accepted width.
    pub const MIN_WIDTH: usize = 4;

    /// A truncator with `width` characters and the ellipsis enabled.
    pub fn new(width: usize) -> Self {
        Self {
            width: width.max(Self::MIN_WIDTH),
            ellipsis: true,
            truncated: 0,
        }
    }

    /// Turns the trailing `...` on or off.
    pub fn with_ellipsis(mut self, ellipsis: bool) -> Self {
        self.ellipsis = ellipsis;
        self
    }

    /// The configured width.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Whether the ellipsis is enabled.
    pub fn ellipsis(&self) -> bool {
        self.ellipsis
    }

    /// How many messages were shortened so far.
    pub fn truncated(&self) -> usize {
        self.truncated
    }
}

impl Default for Truncate {
    fn default() -> Self {
        Self::new(80)
    }
}

impl Stage for Truncate {
    fn name(&self) -> &str {
        "truncate"
    }

    fn kind(&self) -> StageKind {
        StageKind::Reduce
    }

    fn apply(&mut self, mut record: Record) -> Result<Vec<Record>, PipelineError> {
        let message = record.message().to_string();
        let length = message.chars().count();
        if length <= self.width {
            return Ok(vec![record]);
        }
        let mut shortened: String = message.chars().take(self.width).collect();
        if self.ellipsis {
            shortened.push_str("...");
        }
        record.set_field("truncated", length.to_string());
        record.set_message(shortened);
        self.truncated += 1;
        Ok(vec![record])
    }

    fn summarise(&self, report: &StageReport) -> String {
        let note = format!(
            "{} of {} shortened to {} chars",
            self.truncated, report.input, self.width
        );
        format!("{report} [{note}]")
    }

    fn describe(&self) -> String {
        format!("truncate({} chars, ellipsis={})", self.width, self.ellipsis)
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
    fn short_messages_are_untouched() {
        let mut truncate = Truncate::new(10);
        let original = record("short");
        let output = truncate.apply(original.clone()).unwrap();
        assert_eq!(output, vec![original]);
        assert_eq!(truncate.truncated(), 0);
    }

    #[test]
    fn long_messages_are_cut_with_an_ellipsis() {
        let mut truncate = Truncate::new(5);
        let output = truncate.apply(record("abcdefghij")).unwrap();
        assert_eq!(output[0].message(), "abcde...");
        assert_eq!(output[0].get("truncated"), Some("10"));
        assert_eq!(truncate.truncated(), 1);
        assert_eq!(output[0].source(), "api");
    }

    #[test]
    fn the_ellipsis_can_be_disabled() {
        let mut truncate = Truncate::new(3).with_ellipsis(false);
        assert_eq!(truncate.width(), 4);
        assert!(!truncate.ellipsis());
        let output = truncate.apply(record("abcdef")).unwrap();
        assert_eq!(output[0].message(), "abcd");
        assert_eq!(output[0].get("truncated"), Some("6"));
    }

    #[test]
    fn unicode_messages_count_characters_not_bytes() {
        let mut truncate = Truncate::new(6).with_ellipsis(false);
        let output = truncate.apply(record("héllo wörld")).unwrap();
        assert_eq!(output[0].message(), "héllo ");
        assert_eq!(output[0].get("truncated"), Some("11"));
    }

    #[test]
    fn description_mentions_the_configuration() {
        let truncate = Truncate::default();
        assert_eq!(truncate.width(), 80);
        assert_eq!(truncate.describe(), "truncate(80 chars, ellipsis=true)");
        let summary = truncate.summarise(&StageReport::new(2, 2));
        assert_eq!(summary.to_string(), "2 -> 2 [0 of 2 shortened to 80 chars]");
        assert_eq!(truncate.kind(), StageKind::Reduce);
    }
}
