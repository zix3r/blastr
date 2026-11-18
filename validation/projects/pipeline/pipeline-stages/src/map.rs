//! The `map` stage: rewrites a record with a closure or function pointer.

use pipeline_core::{PipelineError, Record, Stage, StageKind, StageReport};

/// A boxed record-to-record function, used by [`Map`].
///
/// The closure may carry mutable state (a counter, a cache), hence `FnMut`.
pub type Transform = Box<dyn FnMut(Record) -> Record>;

/// Applies a transformation to every record.
///
/// The transformation is stored as a trait object, so both closures with
/// captured state and plain `fn` pointers can be plugged in.
pub struct Map {
    label: String,
    transform: Transform,
}

impl Map {
    /// Builds a stage from any `Fn(Record) -> Record`.
    pub fn new(
        label: impl Into<String>,
        transform: impl FnMut(Record) -> Record + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            transform: Box::new(transform),
        }
    }

    /// Builds a stage from a plain function pointer.
    pub fn from_fn(label: impl Into<String>, transform: fn(Record) -> Record) -> Self {
        Self::new(label, transform)
    }
}

impl Stage for Map {
    fn name(&self) -> &str {
        &self.label
    }

    fn kind(&self) -> StageKind {
        StageKind::Map
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        Ok(vec![(self.transform)(record)])
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!("{} transformed {} record(s)", self.label, report.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline_core::Level;

    fn record() -> Record {
        Record::new("api", Level::Info, "Hello World")
    }

    #[test]
    fn closure_transforms_every_record() {
        let mut map = Map::new("shout", |mut record: Record| {
            record.set_message(record.message().to_uppercase());
            record
        });
        let output = map.apply(record()).unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].message(), "HELLO WORLD");
        assert_eq!(map.name(), "shout");
        assert_eq!(map.describe(), "shout(map)");
    }

    #[test]
    fn closures_may_capture_state() {
        let mut seen = 0;
        let mut map = Map::new("count", move |mut record: Record| {
            seen += 1;
            record.set_field("seen", seen.to_string());
            record
        });
        let output = map.apply_all(vec![record(), record()]).unwrap();
        assert_eq!(output[0].get("seen"), Some("1"));
        assert_eq!(output[1].get("seen"), Some("2"));
    }

    #[test]
    fn function_pointers_are_stages_too() {
        fn append_bang(mut record: Record) -> Record {
            let message = format!("{}!", record.message());
            record.set_message(message);
            record
        }

        let mut map = Map::from_fn("bang", append_bang);
        let output = map.apply_all(vec![record()]).unwrap();
        assert_eq!(output[0].message(), "Hello World!");
        assert_eq!(
            map.summarise(&StageReport::new(1, 1)),
            "bang transformed 1 record(s)"
        );
    }

    #[test]
    fn the_default_kind_is_map() {
        assert_eq!(Map::from_fn("noop", |record| record).kind(), StageKind::Map);
    }
}
