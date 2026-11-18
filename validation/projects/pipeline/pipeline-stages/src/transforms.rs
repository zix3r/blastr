//! Named record transformations that can be plugged into a [`crate::Map`].
//!
//! As with the predicates, these are plain functions: callers hand them to a
//! stage as values instead of calling them directly.

use pipeline_core::Record;

/// Upper-cases the message.
pub fn uppercase_message(mut record: Record) -> Record {
    record.set_message(record.message().to_uppercase());
    record
}

/// Lower-cases the message.
pub fn lowercase_message(mut record: Record) -> Record {
    record.set_message(record.message().to_lowercase());
    record
}

/// Trims surrounding whitespace from the message.
pub fn trim_message(mut record: Record) -> Record {
    let message = record.message().trim().to_string();
    record.set_message(message);
    record
}

/// Records the message length in a `message_len` field.
pub fn tag_message_length(mut record: Record) -> Record {
    let length = record.message().chars().count();
    record.set_field("message_len", length.to_string());
    record
}

/// Records the word count in a `words` field.
pub fn tag_word_count(mut record: Record) -> Record {
    let words = record.word_count();
    record.set_field("words", words.to_string());
    record
}

/// Copies the level into a `severity` field as a lower-case label.
pub fn tag_severity(mut record: Record) -> Record {
    let severity = record.level().as_str().to_string();
    record.set_field("severity", severity);
    record
}

/// Every transformation that can be named in a configuration spec.
pub const NAMES: [&str; 6] = ["upper", "lower", "trim", "length", "words", "severity"];

/// Resolves a configuration name to the transformation it names.
pub fn transform_by_name(name: &str) -> Option<fn(Record) -> Record> {
    let transform: fn(Record) -> Record = match name {
        "upper" => uppercase_message,
        "lower" => lowercase_message,
        "trim" => trim_message,
        "length" => tag_message_length,
        "words" => tag_word_count,
        "severity" => tag_severity,
        _ => return None,
    };
    Some(transform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    fn record(message: &str) -> Record {
        Record::new("api", Level::Info, message)
    }

    #[test]
    fn case_folding() {
        assert_eq!(
            uppercase_message(record("Slow Request")).message(),
            "SLOW REQUEST"
        );
        assert_eq!(
            lowercase_message(record("Slow Request")).message(),
            "slow request"
        );
    }

    #[test]
    fn trimming_keeps_inner_spacing() {
        let trimmed = trim_message(record("  slow   request  "));
        assert_eq!(trimmed.message(), "slow   request");
        assert_eq!(trimmed.word_count(), 2);
    }

    #[test]
    fn length_and_word_tags() {
        let tagged = tag_message_length(record("slow request"));
        assert_eq!(tagged.get("message_len"), Some("12"));
        let tagged = tag_word_count(record("slow request today"));
        assert_eq!(tagged.get("words"), Some("3"));
        let empty = tag_message_length(record(""));
        assert_eq!(empty.get("message_len"), Some("0"));
    }

    #[test]
    fn severity_tag_uses_the_canonical_label() {
        let tagged = tag_severity(record("x").with_level(Level::Fatal));
        assert_eq!(tagged.get("severity"), Some("fatal"));
    }

    #[test]
    fn names_resolve_to_functions() {
        for name in NAMES {
            assert!(transform_by_name(name).is_some(), "{name} should resolve");
        }
        assert_eq!(
            transform_by_name("upper").unwrap()(record("x")).message(),
            "X"
        );
        assert_eq!(
            transform_by_name("length").unwrap()(record("abc")).get("message_len"),
            Some("3")
        );
        assert!(transform_by_name("reverse").is_none());
    }
}
