//! The pipeline runner: a named sequence of boxed stages.

use std::fmt;

use crate::error::PipelineError;
use crate::record::Record;
use crate::sink::Sink;
use crate::stage::{Stage, StageReport};

/// Counts for a single stage of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageStats {
    /// The stage name.
    pub name: String,
    /// Records entering the stage.
    pub input: usize,
    /// Records leaving the stage.
    pub output: usize,
}

impl StageStats {
    /// Builds the stats for one stage.
    pub fn new(name: impl Into<String>, input: usize, output: usize) -> Self {
        Self {
            name: name.into(),
            input,
            output,
        }
    }

    /// How many records the stage removed.
    pub fn dropped(&self) -> usize {
        self.input.saturating_sub(self.output)
    }

    /// The report handed to [`Stage::summarise`].
    pub fn report(&self) -> StageReport {
        StageReport::new(self.input, self.output)
    }
}

impl fmt::Display for StageStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} -> {}", self.name, self.input, self.output)?;
        if self.dropped() > 0 {
            write!(f, " ({} dropped)", self.dropped())?;
        }
        Ok(())
    }
}

/// Counts for a whole run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunStats {
    /// Records entering the first stage.
    pub input: usize,
    /// Records leaving the last stage.
    pub output: usize,
    /// One entry per stage, in pipeline order.
    pub stages: Vec<StageStats>,
}

impl RunStats {
    /// Builds the stats of a run.
    pub fn new(input: usize, output: usize, stages: Vec<StageStats>) -> Self {
        Self {
            input,
            output,
            stages,
        }
    }

    /// How many records the whole pipeline removed.
    pub fn dropped(&self) -> usize {
        self.input.saturating_sub(self.output)
    }

    /// The stats of the stage called `name`, if the pipeline has one.
    pub fn stage(&self, name: &str) -> Option<&StageStats> {
        self.stages.iter().find(|stats| stats.name == name)
    }

    /// A one-line summary such as `4 in -> 2 out across 3 stage(s)`.
    pub fn summary(&self) -> String {
        format!(
            "{} in -> {} out across {} stage(s)",
            self.input,
            self.output,
            self.stages.len()
        )
    }

    /// One line per stage.
    pub fn detail(&self) -> Vec<String> {
        self.stages.iter().map(ToString::to_string).collect()
    }
}

impl fmt::Display for RunStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary())
    }
}

/// The records and counts produced by [`Pipeline::run`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutput {
    /// Records that survived the pipeline.
    pub records: Vec<Record>,
    /// Counts for the run.
    pub stats: RunStats,
}

impl RunOutput {
    /// Bundles records with their stats.
    pub fn new(records: Vec<Record>, stats: RunStats) -> Self {
        Self { records, stats }
    }

    /// The rendered records.
    pub fn lines(&self) -> Vec<String> {
        self.records.iter().map(ToString::to_string).collect()
    }

    /// The messages of the surviving records.
    pub fn messages(&self) -> Vec<&str> {
        self.records.iter().map(Record::message).collect()
    }

    /// The levels of the surviving records.
    pub fn levels(&self) -> Vec<crate::level::Level> {
        self.records.iter().map(Record::level).collect()
    }
}

/// A named sequence of stages.
pub struct Pipeline {
    label: String,
    stages: Vec<Box<dyn Stage>>,
}

impl Pipeline {
    /// An empty pipeline with a label used in reports.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            stages: Vec::new(),
        }
    }

    /// Builder-style [`Pipeline::push`].
    pub fn with_stage(mut self, stage: impl Stage + 'static) -> Self {
        self.push(stage);
        self
    }

    /// Appends a stage.
    pub fn push(&mut self, stage: impl Stage + 'static) -> &mut Self {
        self.push_boxed(Box::new(stage))
    }

    /// Appends an already boxed stage.
    pub fn push_boxed(&mut self, stage: Box<dyn Stage>) -> &mut Self {
        self.stages.push(stage);
        self
    }

    /// The pipeline label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// How many stages the pipeline has.
    pub fn len(&self) -> usize {
        self.stages.len()
    }

    /// Whether the pipeline has no stages.
    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// The names of the stages, in order.
    pub fn names(&self) -> Vec<String> {
        self.stages
            .iter()
            .map(|stage| stage.name().to_string())
            .collect()
    }

    /// The stages, mutably, for callers that want to drive them directly.
    pub fn stages_mut(&mut self) -> &mut [Box<dyn Stage>] {
        &mut self.stages
    }

    /// A one-line description of the configured stages.
    pub fn describe(&self) -> String {
        if self.stages.is_empty() {
            return format!("{}: (empty)", self.label);
        }
        let stages: Vec<String> = self.stages.iter().map(|stage| stage.describe()).collect();
        format!("{}: {}", self.label, stages.join(" -> "))
    }

    /// The summaries the stages produce for their last batch.
    ///
    /// `reports` must be the stats of the matching run.
    pub fn summaries(&self, stats: &RunStats) -> Vec<String> {
        self.stages
            .iter()
            .map(|stage| {
                let report = stats
                    .stage(stage.name())
                    .map(StageStats::report)
                    .unwrap_or_else(|| StageReport::new(0, 0));
                stage.summarise(&report)
            })
            .collect()
    }

    fn advance(&mut self, records: Vec<Record>) -> Result<(Vec<Record>, RunStats), PipelineError> {
        let input = records.len();
        let mut current = records;
        let mut stats = Vec::with_capacity(self.stages.len());
        for stage in &mut self.stages {
            let before = current.len();
            current = stage.apply_all(current)?;
            stats.push(StageStats::new(stage.name(), before, current.len()));
        }
        let output = current.len();
        Ok((current, RunStats::new(input, output, stats)))
    }

    /// Runs a batch through every stage and returns the surviving records.
    pub fn run(&mut self, records: Vec<Record>) -> Result<RunOutput, PipelineError> {
        let (records, stats) = self.advance(records)?;
        Ok(RunOutput::new(records, stats))
    }

    /// Runs a batch and writes the survivors into `sink`.
    pub fn run_into(
        &mut self,
        records: Vec<Record>,
        sink: &mut dyn Sink,
    ) -> Result<RunStats, PipelineError> {
        let (records, stats) = self.advance(records)?;
        for record in &records {
            sink.emit(record)?;
        }
        sink.flush()?;
        Ok(stats)
    }
}

impl fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pipeline")
            .field("label", &self.label)
            .field("stages", &self.names())
            .finish()
    }
}

/// Runs `records` through one stage that is known statically.
pub fn run_stage<S>(stage: &mut S, records: Vec<Record>) -> Result<Vec<Record>, PipelineError>
where
    S: Stage + ?Sized,
{
    stage.apply_all(records)
}

/// Runs `records` through two stages that are known statically.
pub fn run_pair<A, B>(
    first: &mut A,
    second: &mut B,
    records: Vec<Record>,
) -> Result<Vec<Record>, PipelineError>
where
    A: Stage + ?Sized,
    B: Stage + ?Sized,
{
    second.apply_all(first.apply_all(records)?)
}

/// Emits every record into `sink` without flushing it.
pub fn drain<I, S>(records: I, sink: &mut S) -> Result<usize, PipelineError>
where
    I: IntoIterator<Item = Record>,
    S: Sink + ?Sized,
{
    let mut emitted = 0;
    for record in records {
        sink.emit(&record)?;
        emitted += 1;
    }
    Ok(emitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::Level;
    use crate::sink::{CollectSink, CountingSink};

    struct KeepLevel(Level);

    impl Stage for KeepLevel {
        fn name(&self) -> &str {
            "keep-level"
        }

        fn kind(&self) -> crate::stage::StageKind {
            crate::stage::StageKind::Filter
        }
        fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
            if record.level() == self.0 {
                Ok(vec![record])
            } else {
                Ok(Vec::new())
            }
        }

        fn summarise(&self, report: &StageReport) -> String {
            format!("kept {} of {} {}", report.output, report.input, self.0)
        }
    }

    struct Fail;

    impl Stage for Fail {
        fn name(&self) -> &str {
            "fail"
        }

        fn kind(&self) -> crate::stage::StageKind {
            crate::stage::StageKind::Emit
        }
        fn apply(&mut self, _record: Record) -> Result<Vec<Record>, PipelineError> {
            Err(PipelineError::stage("fail", "always refuses"))
        }

        fn summarise(&self, _report: &StageReport) -> String {
            String::from("fail")
        }
    }

    fn records() -> Vec<Record> {
        vec![
            Record::new("api", Level::Info, "one"),
            Record::new("api", Level::Warn, "two"),
            Record::new("api", Level::Warn, "three"),
        ]
    }

    #[test]
    fn empty_pipeline_passes_records_through() {
        let mut pipeline = Pipeline::new("empty");
        let output = pipeline.run(records()).unwrap();
        assert_eq!(output.records.len(), 3);
        assert_eq!(output.stats.input, 3);
        assert_eq!(output.stats.output, 3);
        assert!(output.stats.stages.is_empty());
        assert_eq!(pipeline.describe(), "empty: (empty)");
        assert!(pipeline.is_empty());
        assert_eq!(pipeline.label(), "empty");
    }

    #[test]
    fn stages_are_applied_in_order() {
        let mut pipeline = Pipeline::new("warnings")
            .with_stage(KeepLevel(Level::Warn))
            .with_stage(KeepLevel(Level::Warn));
        assert_eq!(pipeline.len(), 2);
        assert_eq!(pipeline.names(), vec!["keep-level", "keep-level"]);
        assert_eq!(
            pipeline.describe(),
            "warnings: keep-level(filter) -> keep-level(filter)"
        );
        let output = pipeline.run(records()).unwrap();
        assert_eq!(output.messages(), vec!["two", "three"]);
        assert_eq!(
            output.stats,
            RunStats::new(
                3,
                2,
                vec![
                    StageStats::new("keep-level", 3, 2),
                    StageStats::new("keep-level", 2, 2),
                ]
            )
        );
        assert_eq!(output.stats.summary(), "3 in -> 2 out across 2 stage(s)");
        assert_eq!(output.stats.dropped(), 1);
        assert_eq!(output.stats.stage("keep-level").unwrap().dropped(), 1);
        assert_eq!(
            output.stats.detail(),
            vec!["keep-level: 3 -> 2 (1 dropped)", "keep-level: 2 -> 2"]
        );
        assert_eq!(output.lines()[0], "[WARN] api two");
        assert_eq!(output.levels(), vec![Level::Warn, Level::Warn]);
    }

    #[test]
    fn summaries_come_from_the_stages() {
        let mut pipeline = Pipeline::new("warnings").with_stage(KeepLevel(Level::Warn));
        let output = pipeline.run(records()).unwrap();
        assert_eq!(
            pipeline.summaries(&output.stats),
            vec!["kept 2 of 3 warn".to_string()]
        );
        assert_eq!(
            pipeline.summaries(&RunStats::default()),
            vec!["kept 0 of 0 warn".to_string()]
        );
    }

    #[test]
    fn stage_errors_abort_the_run() {
        let mut pipeline = Pipeline::new("broken").with_stage(Fail);
        let error = pipeline.run(records()).unwrap_err();
        assert_eq!(error, PipelineError::stage("fail", "always refuses"));
        assert_eq!(error.stage_name(), Some("fail"));
    }

    #[test]
    fn run_into_uses_a_trait_object_sink() {
        let mut pipeline = Pipeline::new("warnings").with_stage(KeepLevel(Level::Warn));
        let mut collect = CollectSink::new("out");
        let stats = pipeline.run_into(records(), &mut collect).unwrap();
        assert_eq!(stats.output, 2);
        assert_eq!(collect.written(), 2);
        assert_eq!(collect.messages(), vec!["two", "three"]);

        let mut counting = CountingSink::new();
        let sink: &mut dyn Sink = &mut counting;
        let stats = pipeline.run_into(records(), sink).unwrap();
        assert_eq!(stats.input, 3);
        assert_eq!(counting.count(), 2);
    }

    #[test]
    fn free_runner_functions_take_generic_stages() {
        let mut first = KeepLevel(Level::Warn);
        let mut second = KeepLevel(Level::Warn);
        let output = run_pair(&mut first, &mut second, records()).unwrap();
        assert_eq!(output.len(), 2);

        let mut single = KeepLevel(Level::Info);
        let output = run_stage(&mut single, records()).unwrap();
        assert_eq!(output.len(), 1);

        let mut sink = CollectSink::default();
        let emitted = drain(records(), &mut sink).unwrap();
        assert_eq!(emitted, 3);
        assert_eq!(sink.written(), 3);
    }

    #[test]
    fn stages_can_be_driven_directly() {
        let mut pipeline = Pipeline::new("direct")
            .with_stage(KeepLevel(Level::Info))
            .with_stage(KeepLevel(Level::Warn));
        let mut current = records();
        for stage in pipeline.stages_mut() {
            current = stage.apply_all(current).unwrap();
        }
        assert!(current.is_empty());
        assert_eq!(pipeline.len(), 2);
    }
}
