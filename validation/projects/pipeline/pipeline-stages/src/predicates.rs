//! Named predicates that can be plugged into a [`crate::Filter`].
//!
//! The functions are deliberately plain `fn(&Record) -> bool` items: the
//! configuration layer passes them around as values, without calling them.

use pipeline_core::{Level, Record};

/// Keeps errors and fatals.
pub fn is_error(record: &Record) -> bool {
    record.level().is_failure()
}

/// Keeps anything at least as severe as a warning.
pub fn is_warn_or_higher(record: &Record) -> bool {
    record.level().at_least(Level::Warn)
}

/// Keeps anything louder than debug (`info` and above).
pub fn is_above_debug(record: &Record) -> bool {
    record.level() > Level::Debug
}

/// Keeps health-check noise, matched on the message or the `path` field.
pub fn is_health_check(record: &Record) -> bool {
    if record.message().contains("health") {
        return true;
    }
    record
        .get("path")
        .is_some_and(|path| path.starts_with("/health"))
}

/// Keeps records that carry a request id.
pub fn has_request_id(record: &Record) -> bool {
    record.get("request_id").is_some_and(|id| !id.is_empty())
}

/// Every predicate that can be named in a configuration spec.
pub const NAMES: [&str; 5] = ["error", "warn", "no-debug", "health", "request"];

/// Resolves a configuration name to the predicate it names.
pub fn predicate_by_name(name: &str) -> Option<fn(&Record) -> bool> {
    let predicate: fn(&Record) -> bool = match name {
        "error" => is_error,
        "warn" => is_warn_or_higher,
        "no-debug" => is_above_debug,
        "health" => is_health_check,
        "request" => has_request_id,
        _ => return None,
    };
    Some(predicate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(level: Level, message: &str) -> Record {
        Record::new("api", level, message)
    }

    #[test]
    fn severity_predicates() {
        assert!(is_error(&record(Level::Error, "boom")));
        assert!(is_error(&record(Level::Fatal, "dead")));
        assert!(!is_error(&record(Level::Warn, "hmm")));
        assert!(is_warn_or_higher(&record(Level::Warn, "hmm")));
        assert!(!is_warn_or_higher(&record(Level::Info, "ok")));
        assert!(is_above_debug(&record(Level::Info, "ok")));
        assert!(!is_above_debug(&record(Level::Debug, "detail")));
    }

    #[test]
    fn health_checks_match_message_or_path() {
        assert!(is_health_check(&record(Level::Info, "health probe ok")));
        let probed = record(Level::Info, "probe").with_field("path", "/health/ready");
        assert!(is_health_check(&probed));
        assert!(!is_health_check(&record(Level::Info, "payments settled")));
    }

    #[test]
    fn request_ids_must_be_present_and_non_empty() {
        let tagged = record(Level::Info, "paid").with_field("request_id", "abc123");
        assert!(has_request_id(&tagged));
        let empty = record(Level::Info, "paid").with_field("request_id", "");
        assert!(!has_request_id(&empty));
        assert!(!has_request_id(&record(Level::Info, "paid")));
    }

    #[test]
    fn names_resolve_to_functions() {
        for name in NAMES {
            assert!(predicate_by_name(name).is_some(), "{name} should resolve");
        }
        assert!(predicate_by_name("error").unwrap()(&record(
            Level::Error,
            "boom"
        )));
        assert!(!predicate_by_name("warn").unwrap()(&record(
            Level::Info,
            "ok"
        )));
        assert!(predicate_by_name("health").unwrap()(&record(
            Level::Info,
            "health"
        )));
        assert!(predicate_by_name("request").unwrap()(
            &record(Level::Info, "x").with_field("request_id", "1")
        ));
        assert!(predicate_by_name("no-debug").unwrap()(&record(
            Level::Warn,
            "hmm"
        )));
        assert!(predicate_by_name("louder").is_none());
    }
}
