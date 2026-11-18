//! Tests that drive a subset of the stages through `&mut dyn Stage`.
//!
//! Only the stages below are reached dynamically here; the rest of the stage
//! crate is covered through `Pipeline` in the other integration tests.

use pipeline_core::{Level, Pipeline, Record, Stage, StageExt, read_records};
use pipeline_stages::{
    BySource, Dedup, Filter, Group, Limit, Map, Tag, Tap, is_error, uppercase_message,
};

fn records() -> Vec<Record> {
    vec![
        Record::new("api", Level::Info, "first event"),
        Record::new("api", Level::Error, "second event"),
        Record::new("worker", Level::Error, "third event"),
        Record::new("worker", Level::Error, "third event"),
    ]
}

fn dispatch(stage: &mut dyn Stage, records: Vec<Record>) -> Vec<Record> {
    stage.apply_all(records).unwrap()
}

#[test]
fn boxed_stages_run_in_order_with_default_batching() {
    let mut stages: Vec<Box<dyn Stage>> = vec![
        Box::new(Filter::from_fn("errors", is_error)),
        Box::new(Map::from_fn("shout", uppercase_message)),
        Box::new(Tag::new("lane", "ops")),
    ];

    let mut current = records();
    for stage in &mut stages {
        current = stage.apply_all(current).unwrap();
    }

    assert_eq!(current.len(), 3);
    assert_eq!(current[0].message(), "SECOND EVENT");
    assert_eq!(current[1].get("lane"), Some("ops"));
    assert_eq!(current[2].source(), "worker");
    assert_eq!(
        stages
            .iter()
            .map(|stage| stage.name())
            .collect::<Vec<&str>>(),
        vec!["errors", "shout", "tag"]
    );
}

#[test]
fn closures_and_function_pointers_dispatch_identically() {
    let mut closure_stage = Map::new("via-closure", |mut record: Record| {
        let message = record.message().to_uppercase();
        record.set_message(message);
        record
    });
    let mut pointer_stage = Map::from_fn("via-pointer", uppercase_message);

    let from_closure = dispatch(&mut closure_stage, records());
    let from_pointer = dispatch(&mut pointer_stage, records());

    assert_eq!(from_closure, from_pointer);
    assert_eq!(from_closure[1].message(), "SECOND EVENT");
    assert_eq!(closure_stage.describe(), "via-closure(map)");
}

#[test]
fn a_single_record_can_be_pushed_through_a_dyn_stage() {
    let mut tap = Tap::new();
    let stage: &mut dyn Stage = &mut tap;
    let output = stage.apply(records()[0].clone());
    assert_eq!(output.unwrap().len(), 1);
    let output = dispatch(stage, records());
    assert_eq!(output.len(), 4);
    assert_eq!(tap.total(), 5);
    assert_eq!(tap.describe(), "tap(5 records seen)");
}

#[test]
fn only_the_deduplicator_overrides_batch_processing() {
    let batch = records();
    let mut dedup = Dedup::new();
    let batched = dispatch(&mut dedup, batch.clone());
    assert_eq!(batched.len(), 3);
    assert_eq!(batched[0].message(), "first event");
    assert_eq!(batched[2].source(), "worker");

    let mut per_record = Dedup::new();
    let each = per_record.apply_each(batch).unwrap();
    assert_eq!(each.len(), 3);
    assert_eq!(each[2].message(), "third event");

    let mut limit = Limit::new(2);
    let limited = dispatch(&mut limit, records());
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[1].message(), "second event");
}

#[test]
fn dynamic_stages_share_the_pipeline_runner() {
    let mut pipeline = Pipeline::new("mixed");
    pipeline.push_boxed(Box::new(Group::new(BySource)));
    pipeline.push_boxed(Box::new(Filter::from_fn("errors", is_error)));

    let output = pipeline.run(records()).unwrap();
    assert_eq!(output.records.len(), 3);
    assert_eq!(
        pipeline.summaries(&output.stats),
        vec![
            "group by source found 2 bucket(s) in 4 record(s): api=2,worker=2",
            "errors rejected 1 of 4 record(s)",
        ]
    );
    assert_eq!(
        pipeline.names(),
        vec!["group".to_string(), "errors".to_string()]
    );
    assert_eq!(read_records("INFO api one\n").unwrap().len(), 1);
}
