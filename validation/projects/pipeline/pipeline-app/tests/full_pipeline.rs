//! Whole-pipeline integration tests for the demo application.

use pipeline_app::{DEFAULT_SPEC, SAMPLE_LOG, build_pipeline, run_spec};
use pipeline_core::{Describe, Level, Sink, read_records};
use pipeline_stages::TextSink;

#[test]
fn default_spec_produces_the_documented_report() {
    let report = run_spec(DEFAULT_SPEC, SAMPLE_LOG).unwrap();
    assert_eq!(
        report.label,
        "level>filter>dedup>redact>group>enrich>truncate"
    );
    assert_eq!(report.stats.input, 8);
    assert_eq!(report.stats.output, 6);
    assert_eq!(report.stats.stages.len(), 7);
    assert_eq!(report.record_count(), 6);
    assert_eq!(
        report.lines,
        vec![
            "api INFO request finished path=/orders service=unknown status=200 words=2",
            "api WARN rate limited level_source=warn path=/orders service=unknown status=429 words=2",
            "api WARN slow request finished path=/orders service=unknown status=504 words=3",
            "api ERROR upstream timeout path=/orders service=unknown status=504 token=*** words=2",
            "api INFO health check path=/health service=unknown status=200 words=2",
            "worker WARN compaction finished after sc... service=unknown truncated=63 words=9",
        ]
    );
    assert_eq!(
        report.summaries,
        vec![
            "normalised 1 record(s), promoted 1, kept 8",
            "filter:no-debug rejected 1 of 8 record(s)",
            "dedup dropped 1 of 7 record(s), 6 distinct",
            "1 secret(s) masked across 6 record(s)",
            "group by level found 3 bucket(s) in 6 record(s): info=2,warn=3,error=1",
            "enriched 6 record(s) with 0 known service(s)",
            "6 -> 6 [1 of 6 shortened to 28 chars]",
        ]
    );
    assert_eq!(
        report.stats.detail(),
        vec![
            "normalize-level: 8 -> 8",
            "filter:no-debug: 8 -> 7 (1 dropped)",
            "dedup: 7 -> 6 (1 dropped)",
            "redact: 6 -> 6",
            "group: 6 -> 6",
            "enrich: 6 -> 6",
            "truncate: 6 -> 6",
        ]
    );
}

#[test]
fn reports_render_every_section() {
    let report = run_spec("level|filter:error|map:upper", SAMPLE_LOG).unwrap();
    let rendered = report.render();
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines[0], "pipeline level>filter>map");
    assert_eq!(lines[1], "8 in -> 2 out across 3 stage(s)");
    assert_eq!(lines[2], "  normalize-level: 8 -> 8");
    assert_eq!(lines[3], "  filter:error: 8 -> 2 (6 dropped)");
    assert_eq!(lines[4], "  map:upper: 2 -> 2");
    assert_eq!(lines[5], "  * normalised 1 record(s), promoted 1, kept 8");
    assert_eq!(lines[6], "  * filter:error rejected 6 of 8 record(s)");
    assert_eq!(lines[7], "  * map:upper transformed 2 record(s)");
    assert_eq!(lines[8], "---");
    assert_eq!(
        lines[9],
        "api ERROR UPSTREAM TIMEOUT path=/orders status=504 token=abc123"
    );
    assert_eq!(
        lines[10],
        "api ERROR UPSTREAM TIMEOUT path=/orders status=504 token=abc123"
    );
    assert_eq!(lines.len(), 11);
    assert_eq!(report.headline(), "pipeline level>filter>map");
    assert!(report.mentions("2 out across 3 stage(s)"));
}

#[test]
fn the_pipeline_can_run_without_the_report_wrapper() {
    let mut pipeline = build_pipeline("filter:error|dedup").unwrap();
    assert_eq!(pipeline.len(), 2);
    assert_eq!(pipeline.label(), "filter>dedup");

    let records = read_records(SAMPLE_LOG).unwrap();
    assert_eq!(records.len(), 8);
    let mut sink = TextSink::new("integration");
    let stats = pipeline.run_into(records, &mut sink).unwrap();

    assert_eq!(stats.input, 8);
    assert_eq!(stats.output, 1);
    assert_eq!(stats.stage("dedup").unwrap().dropped(), 1);
    assert_eq!(sink.written(), 1);
    assert_eq!(
        sink.lines(),
        &["api ERROR upstream timeout path=/orders status=504 token=abc123".to_string()]
    );
    // `run_into` flushes the sink itself, so nothing is pending afterwards.
    assert_eq!(sink.flush().unwrap(), 0);
}

#[test]
fn records_reach_the_pipeline_with_their_levels() {
    let mut pipeline = build_pipeline("level|filter:warn").unwrap();
    let output = pipeline.run(read_records(SAMPLE_LOG).unwrap()).unwrap();
    assert_eq!(
        output.levels(),
        vec![
            Level::Warn,
            Level::Warn,
            Level::Error,
            Level::Error,
            Level::Warn
        ]
    );
    assert_eq!(
        output.messages(),
        vec![
            "rate limited",
            "slow request finished",
            "upstream timeout",
            "upstream timeout",
            "compaction finished after scanning every segment in the archive",
        ]
    );
    assert_eq!(output.records[0].get("level_source"), Some("warn"));
}

#[test]
fn stage_errors_surface_through_the_application() {
    let error = run_spec("validate:request_id", SAMPLE_LOG).unwrap_err();
    assert_eq!(
        error.to_string(),
        "stage `validate` requires field `request_id`"
    );
    assert_eq!(error.stage_name(), Some("validate"));
}
