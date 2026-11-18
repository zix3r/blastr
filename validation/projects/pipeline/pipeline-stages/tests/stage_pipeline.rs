//! Integration tests that build whole pipelines from the public stage API.

use pipeline_core::{Level, Pipeline, Record, Sink, Stage, StageExt, read_records};
use pipeline_stages::{
    ByLevel, BySource, Dedup, Enrich, Filter, Group, Limit, Map, NormalizeLevel, Redact, Tag, Tap,
    TextSink, Truncate, is_error, is_warn_or_higher, uppercase_message,
};

const LOG: &str = "\
INFO api cache warmup entries=64
WARN worker queue depth high depth=91
ERROR api upstream timeout token=abc123
ERROR api upstream timeout token=abc123
";

fn records() -> Vec<Record> {
    read_records(LOG).unwrap()
}

#[test]
fn end_to_end_pipeline_over_the_log_format() {
    let mut pipeline = Pipeline::new("ops")
        .with_stage(Filter::from_fn("errors", is_error))
        .with_stage(Dedup::new())
        .with_stage(Redact::new())
        .with_stage(Tag::new("env", "prod"))
        .with_stage(Truncate::new(12));

    let output = pipeline.run(records()).unwrap();
    assert_eq!(output.records.len(), 1);
    assert_eq!(
        output.lines(),
        vec!["[ERROR] api upstream tim... env=prod token=*** truncated=16"]
    );
    assert_eq!(output.stats.input, 4);
    assert_eq!(output.stats.output, 1);
    assert_eq!(
        output.stats.detail(),
        vec![
            "errors: 4 -> 2 (2 dropped)",
            "dedup: 2 -> 1 (1 dropped)",
            "redact: 1 -> 1",
            "tag: 1 -> 1",
            "truncate: 1 -> 1",
        ]
    );
    assert_eq!(
        pipeline.summaries(&output.stats),
        vec![
            "errors rejected 2 of 4 record(s)",
            "dedup dropped 1 of 2 record(s), 1 distinct",
            "1 secret(s) masked across 1 record(s)",
            "tagged 1 record(s) with env=prod (1 passed)",
            "1 -> 1 [1 of 1 shortened to 12 chars]",
        ]
    );
}

#[test]
fn map_and_filter_compose_as_a_chain() {
    let mut chain = Filter::from_fn("warnings", is_warn_or_higher)
        .then(Map::from_fn("shout", uppercase_message));
    let output = chain.run(records()).unwrap();
    assert_eq!(output.len(), 3);
    assert_eq!(output[0].message(), "QUEUE DEPTH HIGH");
    assert_eq!(output[1].message(), "UPSTREAM TIMEOUT");
    assert_eq!(output[2].level(), Level::Error);
    assert_eq!(chain.describe(), "chain[warnings -> shout]");
    assert_eq!(
        chain.summarise(&pipeline_core::StageReport::new(4, 3)),
        "warnings -> shout (4 in, 3 out)"
    );
}

#[test]
fn dedup_prefers_the_last_copy_of_a_batch() {
    let mut dedup = Dedup::new();
    let batch = vec![
        Record::new("api", Level::Error, "timeout").with_field("attempt", "1"),
        Record::new("api", Level::Error, "timeout").with_field("attempt", "2"),
        Record::new("api", Level::Error, "timeout").with_field("attempt", "2"),
    ];
    let batched = dedup.apply_all(batch.clone()).unwrap();
    assert_eq!(batched.len(), 2);
    assert_eq!(batched[0].get("attempt"), Some("1"));
    assert_eq!(batched[1].get("attempt"), Some("2"));
    assert_eq!(dedup.dropped(), 1);

    let mut per_record = Dedup::new();
    let each = per_record.apply_each(batch).unwrap();
    assert_eq!(each.len(), 2);
    assert_eq!(each[0].get("attempt"), Some("1"));
    assert_eq!(each[1].get("attempt"), Some("2"));
    assert_eq!(per_record.dropped(), 1);
}

#[test]
fn grouping_by_source_and_level() {
    let mut by_source = Pipeline::new("by-source")
        .with_stage(Group::new(BySource))
        .with_stage(Filter::from_fn("errors", is_error));
    let output = by_source.run(records()).unwrap();
    assert_eq!(output.stats.output, 2);
    assert_eq!(
        by_source.summaries(&output.stats),
        vec![
            "group by source found 2 bucket(s) in 4 record(s): api=3,worker=1",
            "errors rejected 2 of 4 record(s)",
        ]
    );

    let mut by_level = Pipeline::new("by-level").with_stage(Group::new(ByLevel));
    let output = by_level.run(records()).unwrap();
    assert_eq!(
        by_level.summaries(&output.stats),
        vec!["group by level found 3 bucket(s) in 4 record(s): info=1,warn=1,error=2"]
    );
}

#[test]
fn normalisation_limit_and_enrichment_work_together() {
    let text = "\
INFO api first
INFO api second level=warn status=429
WARN api third
";
    let records = read_records(text).unwrap();
    let mut pipeline = Pipeline::new("mixed")
        .with_stage(NormalizeLevel::new())
        .with_stage(Limit::new(2))
        .with_stage(Enrich::new().with_services([("api", "gateway")]));

    let output = pipeline.run(records).unwrap();
    assert_eq!(output.records.len(), 2);
    assert_eq!(
        output.lines(),
        vec![
            "[INFO] api first service=gateway words=1",
            "[WARN] api second level_source=warn service=gateway status=429 words=1",
        ]
    );
    assert_eq!(
        pipeline.summaries(&output.stats),
        vec![
            "normalised 1 record(s), promoted 1, kept 3",
            "limit 2 kept 2 of 3 record(s), 0 left",
            "enriched 2 record(s) with 1 known service(s)",
        ]
    );
}

#[test]
fn taps_and_text_sinks_observe_the_same_records() {
    let mut pipeline = Pipeline::new("observe")
        .with_stage(Filter::new("keep-all", |_record: &Record| true))
        .with_stage(Tap::new());
    let mut sink = TextSink::new("text");
    let stats = pipeline.run_into(records(), &mut sink).unwrap();
    assert_eq!(stats.output, 4);
    assert_eq!(sink.written(), 4);
    // `run_into` flushed the sink already.
    assert_eq!(sink.flush().unwrap(), 0);
    assert_eq!(sink.lines()[1], "worker WARN queue depth high depth=91");
    assert_eq!(
        pipeline.summaries(&stats),
        vec![
            "keep-all rejected 0 of 4 record(s)",
            "tap saw 4 record(s) of 4: info=1,warn=1,error=2",
        ]
    );
    assert_eq!(
        pipeline.describe(),
        "observe: keep-all(filter) -> tap(4 records seen)"
    );
}

#[test]
fn redaction_happens_before_truncation() {
    let mut pipeline = Pipeline::new("secret")
        .with_stage(Redact::new())
        .with_stage(Truncate::new(40).with_ellipsis(false));
    let text = "WARN api login failed password=hunter2-and-more user=ada\n";
    let output = pipeline.run(read_records(text).unwrap()).unwrap();
    assert_eq!(
        output.lines(),
        vec!["[WARN] api login failed password=*** user=ada"]
    );
    assert_eq!(output.records[0].get("password"), Some("***"));
}

#[test]
fn stages_keep_the_order_of_the_records() {
    let mut pipeline = Pipeline::new("order")
        .with_stage(Map::from_fn("trim", |mut record: Record| {
            let message = record.message().trim().to_string();
            record.set_message(message);
            record
        }))
        .with_stage(Tag::new("seen", "1"));
    let text = "INFO api   padded   message   \nDEBUG api second\n";
    let output = pipeline.run(read_records(text).unwrap()).unwrap();
    assert_eq!(output.messages(), vec!["padded message", "second"]);
    assert_eq!(output.records[0].get("seen"), Some("1"));
    assert_eq!(output.records[1].get("seen"), Some("1"));
}
