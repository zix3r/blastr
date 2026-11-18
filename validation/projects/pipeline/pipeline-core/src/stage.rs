//! The [`Stage`] trait and the combinators built on top of it.

use std::fmt;

use crate::error::PipelineError;
use crate::record::Record;

/// The broad category a stage belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StageKind {
    /// Drops records without changing them.
    Filter,
    /// Rewrites individual records.
    Map,
    /// Adds derived information to records.
    Annotate,
    /// Collapses several records into fewer records.
    Reduce,
    /// Forwards records to an output.
    Emit,
}

impl StageKind {
    /// Every kind of stage.
    pub const ALL: [StageKind; 5] = [
        StageKind::Filter,
        StageKind::Map,
        StageKind::Annotate,
        StageKind::Reduce,
        StageKind::Emit,
    ];

    /// The canonical lower-case name.
    pub const fn as_str(self) -> &'static str {
        match self {
            StageKind::Filter => "filter",
            StageKind::Map => "map",
            StageKind::Annotate => "annotate",
            StageKind::Reduce => "reduce",
            StageKind::Emit => "emit",
        }
    }

    /// Parses a kind name, as written in a pipeline spec.
    pub fn from_label(label: &str) -> Result<Self, PipelineError> {
        let lowered = label.trim().to_ascii_lowercase();
        match lowered.as_str() {
            "filter" => Ok(StageKind::Filter),
            "map" => Ok(StageKind::Map),
            "annotate" => Ok(StageKind::Annotate),
            "reduce" => Ok(StageKind::Reduce),
            "emit" => Ok(StageKind::Emit),
            _ => Err(PipelineError::parse("stage kind", label)),
        }
    }

    /// Whether the stage is expected to remove records.
    pub fn is_lossy(self) -> bool {
        matches!(self, StageKind::Filter | StageKind::Reduce)
    }
}

impl fmt::Display for StageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The record counts a stage saw and produced during one batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StageReport {
    /// Records entering the stage.
    pub input: usize,
    /// Records leaving the stage.
    pub output: usize,
    /// Free-form notes the stage wants to surface.
    pub notes: Vec<String>,
}

impl StageReport {
    /// A report without notes.
    pub fn new(input: usize, output: usize) -> Self {
        Self {
            input,
            output,
            notes: Vec::new(),
        }
    }

    /// Adds a note.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// How many records the stage removed.
    pub fn dropped(&self) -> usize {
        self.input.saturating_sub(self.output)
    }
}

impl fmt::Display for StageReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {}", self.input, self.output)?;
        if !self.notes.is_empty() {
            write!(f, " [{}]", self.notes.join("; "))?;
        }
        Ok(())
    }
}

/// A step of a pipeline.
///
/// Four items are required: the stage's category, its name, the per-record
/// transformation and the summary line it contributes to the report. The batch
/// entry point and the one-line description have default bodies; a few
/// implementations override them when the generic behaviour is not good
/// enough. The trait stays object safe so a [`crate::Pipeline`] can hold
/// `Box<dyn Stage>` values.
pub trait Stage {
    /// A stable, human-readable name used in reports.
    fn name(&self) -> &str;

    /// The category the stage reports itself as.
    fn kind(&self) -> StageKind;

    /// Transforms one record into zero or more records.
    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError>;

    /// Describes what the stage did, given the counts of the last batch.
    fn summarise(&self, report: &StageReport) -> String;

    /// Processes a whole batch.
    ///
    /// The default applies the stage record by record, preserving order.
    /// Stages that need to look at the whole batch (deduplication, for
    /// instance) override this method.
    fn apply_all(&mut self, records: Vec<Record>) -> Result<Vec<Record>, PipelineError> {
        let mut output = Vec::with_capacity(records.len());
        for record in records {
            output.extend(self.apply(record)?);
        }
        Ok(output)
    }

    /// A one-line description used by [`crate::Pipeline::describe`].
    fn describe(&self) -> String {
        format!("{}({})", self.name(), self.kind().as_str())
    }
}

/// Two stages glued together; the output of `first` feeds `second`.
#[derive(Clone, Debug)]
pub struct Chain<A, B> {
    first: A,
    second: B,
}

impl<A, B> Chain<A, B> {
    /// Chains two stages.
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }

    /// The upstream stage.
    pub fn first(&self) -> &A {
        &self.first
    }

    /// The downstream stage.
    pub fn second(&self) -> &B {
        &self.second
    }

    /// Splits the chain into its two stages.
    pub fn into_parts(self) -> (A, B) {
        (self.first, self.second)
    }
}

impl<A: Stage, B: Stage> Chain<A, B> {
    /// Runs a batch through both stages without boxing either of them.
    pub fn run(&mut self, records: Vec<Record>) -> Result<Vec<Record>, PipelineError> {
        let intermediate = self.first.apply_all(records)?;
        self.second.apply_all(intermediate)
    }
}

impl<A: Stage, B: Stage> Stage for Chain<A, B> {
    fn name(&self) -> &str {
        "chain"
    }

    fn kind(&self) -> StageKind {
        StageKind::Map
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        let intermediate = self.first.apply(record)?;
        let mut output = Vec::with_capacity(intermediate.len());
        for mid in intermediate {
            output.extend(self.second.apply(mid)?);
        }
        Ok(output)
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!(
            "{} -> {} ({} in, {} out)",
            self.first.name(),
            self.second.name(),
            report.input,
            report.output
        )
    }

    fn describe(&self) -> String {
        format!("chain[{} -> {}]", self.first.name(), self.second.name())
    }
}

/// Convenience methods for every sized stage.
pub trait StageExt: Stage + Sized {
    /// Glues this stage to `next`.
    fn then<S: Stage>(self, next: S) -> Chain<Self, S> {
        Chain::new(self, next)
    }

    /// Erases the concrete type so the stage can join a `Vec<Box<dyn Stage>>`.
    fn boxed(self) -> Box<dyn Stage>
    where
        Self: 'static,
    {
        Box::new(self)
    }

    /// Applies the stage record by record, bypassing any batch-level override.
    fn apply_each(&mut self, records: Vec<Record>) -> Result<Vec<Record>, PipelineError> {
        let mut output = Vec::with_capacity(records.len());
        for record in records {
            output.extend(self.apply(record)?);
        }
        Ok(output)
    }
}

impl<T: Stage + Sized> StageExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::Level;

    struct Rename(&'static str);

    impl Stage for Rename {
        fn name(&self) -> &str {
            "rename"
        }

        fn kind(&self) -> StageKind {
            StageKind::Map
        }

        fn apply(&mut self, mut record: Record) -> Result<Vec<Record>, PipelineError> {
            record.set_message(self.0);
            Ok(vec![record])
        }

        fn summarise(&self, report: &StageReport) -> String {
            format!("renamed {}", report.output)
        }
    }

    struct DropShort(usize);

    impl Stage for DropShort {
        fn name(&self) -> &str {
            "drop-short"
        }

        fn kind(&self) -> StageKind {
            StageKind::Filter
        }

        fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
            if record.word_count() < self.0 {
                Ok(Vec::new())
            } else {
                Ok(vec![record])
            }
        }

        fn summarise(&self, report: &StageReport) -> String {
            format!("dropped {}", report.dropped())
        }
    }

    fn records() -> Vec<Record> {
        vec![
            Record::new("api", Level::Info, "one"),
            Record::new("api", Level::Warn, "two words"),
            Record::new("api", Level::Error, "three short words"),
        ]
    }

    #[test]
    fn kind_names_round_trip() {
        for kind in StageKind::ALL {
            assert_eq!(StageKind::from_label(kind.as_str()), Ok(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(StageKind::from_label("Reduce"), Ok(StageKind::Reduce));
        assert_eq!(
            StageKind::from_label("join"),
            Err(PipelineError::parse("stage kind", "join"))
        );
        assert!(StageKind::Filter.is_lossy());
        assert!(!StageKind::Annotate.is_lossy());
        assert!(StageKind::Reduce.is_lossy());
    }

    #[test]
    fn stage_report_counts() {
        let report = StageReport::new(5, 3).with_note("kept newest");
        assert_eq!(report.dropped(), 2);
        assert_eq!(report.to_string(), "5 -> 3 [kept newest]");
        assert_eq!(StageReport::new(4, 4).to_string(), "4 -> 4");
        assert_eq!(StageReport::new(3, 5).dropped(), 0);
    }

    #[test]
    fn default_apply_all_preserves_order_and_errors() {
        let mut rename = Rename("rewritten");
        let output = rename.apply_all(records()).unwrap();
        assert_eq!(output.len(), 3);
        assert_eq!(output[0].message(), "rewritten");
        assert_eq!(output[2].level(), Level::Error);
    }

    #[test]
    fn chain_runs_both_halves() {
        let mut chain = DropShort(2).then(Rename("kept"));
        let output = chain.run(records()).unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].message(), "kept");
        assert_eq!(chain.first().name(), "drop-short");
        assert_eq!(chain.second().name(), "rename");
        assert_eq!(chain.name(), "chain");
        assert_eq!(chain.describe(), "chain[drop-short -> rename]");
        assert_eq!(
            chain.summarise(&StageReport::new(3, 2)),
            "drop-short -> rename (3 in, 2 out)"
        );
    }

    #[test]
    fn stage_extension_helpers() {
        let mut drop_short = DropShort(3);
        assert_eq!(drop_short.apply(records()[0].clone()).unwrap().len(), 0);
        assert_eq!(drop_short.apply_each(records()).unwrap().len(), 1);

        let boxed: Box<dyn Stage> = Rename("boxed").boxed();
        assert_eq!(boxed.name(), "rename");
        assert_eq!(boxed.describe(), "rename(map)");
        assert_eq!(boxed.kind(), StageKind::Map);
    }
}
