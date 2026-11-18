//! Integration tests for the core runner, using stages defined in this test.

use pipeline_core::{
    Chain, CollectSink, CountingSink, Level, Pipeline, PipelineError, Record, Sink, Stage,
    StageExt, StageKind, StageReport, TallySink, describe_sink, drain, parse_line, read_records,
    run_pair, run_stage,
};

/// Adds a constant field; a typical annotate stage.
struct Stamp(&'static str);

impl Stage for Stamp {
    fn name(&self) -> &str {
        "stamp"
    }

    fn kind(&self) -> StageKind {
        StageKind::Annotate
    }

    fn apply(&mut self, mut record: Record) -> Result<Vec<Record>, PipelineError> {
        record.set_field("stamp", self.0);
        Ok(vec![record])
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!("stamped {} of {}", report.output, report.input)
    }
}

/// Emits two records for two-word messages.
struct Split;

impl Stage for Split {
    fn name(&self) -> &str {
        "split"
    }

    fn kind(&self) -> StageKind {
        StageKind::Map
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        if record.word_count() == 2 {
            let mut copy = record.clone();
            copy.set_message(format!("{} copy", record.message()));
            Ok(vec![record, copy])
        } else {
            Ok(vec![record])
        }
    }

    fn summarise(&self, report: &StageReport) -> String {
        format!("split {} into {}", report.input, report.output)
    }
}

/// Always refuses the record.
struct Reject;

impl Stage for Reject {
    fn name(&self) -> &str {
        "reject"
    }

    fn kind(&self) -> StageKind {
        StageKind::Emit
    }

    fn apply(&mut self, record: Record) -> Result<Vec<Record>, PipelineError> {
        Err(PipelineError::stage(
            "reject",
            format!("refused `{}`", record.message()),
        ))
    }

    fn summarise(&self, _report: &StageReport) -> String {
        String::from("reject")
    }
}

fn records() -> Vec<Record> {
    vec![
        Record::new("api", Level::Info, "hello world"),
        Record::new("worker", Level::Error, "boom"),
    ]
}

#[test]
fn pipeline_runs_stages_and_reports_counts() {
    let mut pipeline = Pipeline::new("demo")
        .with_stage(Stamp("first"))
        .with_stage(Split);
    assert_eq!(pipeline.label(), "demo");
    assert_eq!(pipeline.names(), vec!["stamp", "split"]);
    assert_eq!(pipeline.describe(), "demo: stamp(annotate) -> split(map)");

    let output = pipeline.run(records()).unwrap();
    assert_eq!(output.records.len(), 3);
    assert_eq!(output.stats.input, 2);
    assert_eq!(output.stats.output, 3);
    assert_eq!(output.stats.stages[0].input, 2);
    assert_eq!(output.stats.stages[0].output, 2);
    assert_eq!(output.stats.stages[1].output, 3);
    assert_eq!(
        output.stats.detail(),
        vec!["stamp: 2 -> 2", "split: 2 -> 3"]
    );
    assert_eq!(
        output.messages(),
        vec!["hello world", "hello world copy", "boom"]
    );
    assert_eq!(output.records[0].get("stamp"), Some("first"));
    assert_eq!(
        pipeline.summaries(&output.stats),
        vec!["stamped 2 of 2", "split 2 into 3"]
    );
}

#[test]
fn stage_errors_name_the_failing_stage() {
    let mut pipeline = Pipeline::new("broken")
        .with_stage(Stamp("x"))
        .with_stage(Reject);
    let error = pipeline.run(records()).unwrap_err();
    assert_eq!(
        error,
        PipelineError::stage("reject", "refused `hello world`")
    );
    assert_eq!(error.stage_name(), Some("reject"));
    assert_eq!(error.to_string(), "stage `reject`: refused `hello world`");
}

#[test]
fn chains_run_without_a_pipeline() {
    let mut chain: Chain<Stamp, Split> = Stamp("chained").then(Split);
    let output = chain.run(records()).unwrap();
    assert_eq!(output.len(), 3);
    assert_eq!(chain.first().name(), "stamp");
    assert_eq!(chain.second().name(), "split");
    assert_eq!(chain.describe(), "chain[stamp -> split]");
    assert_eq!(chain.kind(), StageKind::Map);

    let (first, second) = chain.into_parts();
    assert_eq!(first.0, "chained");
    assert_eq!(second.name(), "split");
}

#[test]
fn generic_helpers_accept_sized_and_unsized_stages() {
    let mut stamp = Stamp("helper");
    let stamped = run_stage(&mut stamp, records()).unwrap();
    assert_eq!(stamped.len(), 2);
    assert_eq!(stamped[1].get("stamp"), Some("helper"));

    let mut first = Stamp("pair");
    let mut second = Split;
    let paired = run_pair(&mut first, &mut second, records()).unwrap();
    assert_eq!(paired.len(), 3);

    let boxed: Box<dyn Stage> = Stamp("boxed").boxed();
    let mut boxed = boxed;
    let unsized_output = run_stage(boxed.as_mut(), records()).unwrap();
    assert_eq!(unsized_output.len(), 2);
}

#[test]
fn sinks_receive_records_through_a_trait_object() {
    let mut pipeline = Pipeline::new("sinks")
        .with_stage(Stamp("sink"))
        .with_stage(Split);

    let mut collect = CollectSink::new("collect");
    let stats = pipeline.run_into(records(), &mut collect).unwrap();
    assert_eq!(stats.output, 3);
    assert_eq!(collect.written(), 3);
    assert_eq!(collect.records()[2].message(), "boom");
    assert_eq!(describe_sink(&collect), "collect: 3 record(s)");

    let mut tally = TallySink::new();
    let sink: &mut dyn pipeline_core::Sink = &mut tally;
    pipeline.run_into(records(), sink).unwrap();
    assert_eq!(tally.count_of(Level::Info), 2);
    assert_eq!(tally.count_of(Level::Error), 1);
    assert_eq!(tally.written(), 3);
    assert_eq!(tally.capacity(), 1024);

    let mut counting = CountingSink::new();
    let emitted = drain(records(), &mut counting).unwrap();
    assert_eq!(emitted, 2);
    assert_eq!(counting.count(), 2);
    assert_eq!(counting.flush().unwrap(), 0);
}

#[test]
fn records_can_be_parsed_and_built() {
    let parsed = parse_line("WARN api slow request handled status=504").unwrap();
    assert_eq!(parsed.level(), Level::Warn);
    assert_eq!(parsed.get("status"), Some("504"));

    let built = Record::builder()
        .source("api")
        .level_label("error")
        .unwrap()
        .message("upstream down")
        .field("attempt", "3")
        .build()
        .unwrap();
    assert_eq!(built.to_string(), "[ERROR] api upstream down attempt=3");

    let records = read_records("INFO api one\n# ignored\n\nWARN api two words\n").unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].word_count(), 2);
}
