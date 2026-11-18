//! Integration tests for the configuration language.

use pipeline_app::{StageSpec, build_pipeline, build_stage, parse_spec, run_spec};
use pipeline_core::{Level, PipelineError, Record, Stage};
use pipeline_stages::TextSink;

#[test]
fn every_stage_family_can_be_built_from_a_spec() {
    let spec = "level|filter:error|map:upper|tag:env=prod|truncate:24|dedup|redact|group:source|enrich|tap|limit:5|validate:status";
    let pipeline = build_pipeline(spec).unwrap();
    assert_eq!(pipeline.len(), 12);
    assert_eq!(
        pipeline.names(),
        vec![
            "normalize-level",
            "filter:error",
            "map:upper",
            "tag",
            "truncate",
            "dedup",
            "redact",
            "group",
            "enrich",
            "tap",
            "limit",
            "validate",
        ]
    );
    assert_eq!(
        pipeline.describe(),
        "level>filter>map>tag>truncate>dedup>redact>group>enrich>tap>limit>validate: \
         normalize-level(annotate) -> filter:error(filter) -> map:upper(map) -> tag(annotate) -> \
         truncate(24 chars, ellipsis=true) -> dedup(reduce) -> redact(map) -> group(source) -> \
         enrich(annotate) -> tap(0 records seen) -> limit(filter) -> validate(status)"
    );
}

#[test]
fn specs_can_be_parsed_without_building() {
    let specs = parse_spec("level |truncate:8| tag:team=core ").unwrap();
    assert_eq!(specs.len(), 3);
    assert_eq!(
        specs[0],
        StageSpec {
            kind: "level".to_string(),
            argument: None
        }
    );
    assert_eq!(
        specs[1],
        StageSpec {
            kind: "truncate".to_string(),
            argument: Some("8".to_string())
        }
    );
    assert_eq!(specs[2].kind, "tag");
    assert_eq!(specs[2].require_argument().unwrap(), "team=core");
}

#[test]
fn building_a_stage_returns_a_trait_object() {
    let spec = StageSpec {
        kind: "filter".to_string(),
        argument: Some("error".to_string()),
    };
    let mut stage: Box<dyn Stage> = build_stage(&spec).unwrap();
    let record = Record::new("api", Level::Error, "boom");
    let output = stage.apply(record).unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(stage.name(), "filter:error");

    let spec = StageSpec {
        kind: "map".to_string(),
        argument: Some("severity".to_string()),
    };
    let mut stage = build_stage(&spec).unwrap();
    let output = stage
        .apply(Record::new("api", Level::Fatal, "dead"))
        .unwrap();
    assert_eq!(output[0].get("severity"), Some("fatal"));
}

#[test]
fn configuration_errors_name_the_problem() {
    assert_eq!(
        build_pipeline("level|frobnicate").unwrap_err(),
        PipelineError::config("unknown stage `frobnicate`")
    );
    assert_eq!(
        build_pipeline("filter:critical").unwrap_err(),
        PipelineError::config("unknown predicate `critical`")
    );
    assert_eq!(
        build_pipeline("map:tidy").unwrap_err(),
        PipelineError::config("unknown transformation `tidy`")
    );
    assert_eq!(
        build_pipeline("group:service").unwrap_err(),
        PipelineError::config("stage `group` cannot group by `service`")
    );
    assert_eq!(
        build_pipeline("truncate:2").unwrap_err(),
        PipelineError::config("stage `truncate` needs at least 4 characters")
    );
    assert_eq!(
        build_pipeline(" | ").unwrap_err(),
        PipelineError::config("spec contains no stages")
    );
    assert_eq!(
        build_pipeline("tag").unwrap_err(),
        PipelineError::config("stage `tag` needs an argument")
    );
    assert_eq!(
        build_pipeline("limit:many").unwrap_err(),
        PipelineError::Config(
            "expected a positive number: invalid digit found in string".to_string()
        )
    );
}

#[test]
fn truncation_can_disable_the_ellipsis() {
    let report = run_spec(
        "truncate:10:false",
        "WARN api this message is far too long\n",
    )
    .unwrap();
    assert_eq!(report.lines, vec!["api WARN this messa truncated=28"]);
    let report = run_spec("truncate:10", "WARN api this message is far too long\n").unwrap();
    assert_eq!(report.lines, vec!["api WARN this messa... truncated=28"]);
}

#[test]
fn configured_pipelines_run_over_parsed_records() {
    let mut pipeline = build_pipeline("tag:team=core|group:level|map:words|limit:2").unwrap();
    let text = "\
WARN api first message here
ERROR api second
ERROR worker third
";
    let records: Vec<Record> = pipeline_core::read_records(text).unwrap();
    let mut sink = TextSink::new("dsl");
    let stats = pipeline.run_into(records, &mut sink).unwrap();
    assert_eq!(stats.input, 3);
    assert_eq!(stats.output, 2);
    assert_eq!(
        sink.lines(),
        vec![
            "api WARN first message here team=core words=3",
            "api ERROR second team=core words=1",
        ]
    );
    assert_eq!(
        pipeline.summaries(&stats),
        vec![
            "tagged 3 record(s) with team=core (3 passed)",
            "group by level found 2 bucket(s) in 3 record(s): warn=1,error=2",
            "map:words transformed 3 record(s)",
            "limit 2 kept 2 of 3 record(s), 0 left",
        ]
    );
}
