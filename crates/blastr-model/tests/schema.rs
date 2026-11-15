//! The committed JSON Schema describes the JSON the model produces.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use blastr_model::{
    Analysis, AnalysisParams, Comparison, DiffResult, ImpactLimits, Source, TestSelection,
    analysis_schema,
};

/// The schema file committed in the repository.
fn schema_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/analysis.schema.json"
    ))
}

/// An analysis that compares the working tree with itself and found nothing.
fn empty_analysis() -> Analysis {
    Analysis {
        diff: DiffResult {
            comparison: Comparison {
                base: Source::Worktree,
                head: Source::Worktree,
            },
            base_key: None,
            head_key: None,
            changed_files: Vec::new(),
            changes: Vec::new(),
            limits: Vec::new(),
        },
        items: Vec::new(),
        tests: TestSelection::default(),
        limits: ImpactLimits::default(),
        params: AnalysisParams::default(),
    }
}

#[test]
fn the_committed_schema_matches_the_types() {
    let schema = canonical(analysis_schema());
    let mut rendered = serde_json::to_string_pretty(&schema).expect("the schema serialises");
    rendered.push('\n');

    let path = schema_path();
    if std::env::var_os("BLASTR_UPDATE_SCHEMA").is_some() {
        fs::write(&path, rendered).expect("the schema file is writable");
        return;
    }

    let committed = fs::read_to_string(&path).expect("the schema file is readable");
    let committed: Value = serde_json::from_str(&committed).expect("the schema file is JSON");
    assert_eq!(
        canonical(committed),
        schema,
        "the committed schema is out of date; rerun with BLASTR_UPDATE_SCHEMA=1"
    );
}

/// The same schema with object keys and lists of names in sorted order.
///
/// The order the generator emits them in depends on how the dependencies were
/// built and carries no meaning, so it must not decide whether two schemas
/// are equal or what the committed file looks like.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect(),
            )
        }
        Value::Array(items) => {
            let mut items: Vec<Value> = items.into_iter().map(canonical).collect();
            if items.iter().all(Value::is_string) {
                items.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
            Value::Array(items)
        }
        other => other,
    }
}

#[test]
fn every_top_level_key_of_an_analysis_is_described() {
    let schema = analysis_schema();
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .expect("the schema describes the fields of an analysis");

    let serialised = serde_json::to_value(empty_analysis()).expect("the analysis serialises");
    let object = serialised
        .as_object()
        .expect("an analysis is a JSON object");

    for key in object.keys() {
        assert!(
            properties.contains_key(key),
            "the schema does not describe the field `{key}`"
        );
    }
}
