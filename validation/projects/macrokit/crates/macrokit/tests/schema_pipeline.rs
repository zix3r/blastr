//! End-to-end tests of the generated schema and the engine built on it.
//!
//! These tests only use the public API: they are the contract an application
//! like `macrokit-station` relies on.

use macrokit::describe::Describe;
use macrokit::{
    ConfigError, Engine, Kind, MergePolicy, Schema, SettingSpec, SettingValue, Source, unit,
};

#[test]
fn the_generated_schema_is_internally_consistent() {
    let schema = Schema::baseline();
    assert_eq!(schema.len(), 29);
    assert_eq!(schema.keys().count(), 29);

    let mut identifiers = Vec::new();
    for spec in schema.iter() {
        identifiers.push(spec.id().get());
        assert!(
            spec.key()
                .chars()
                .all(|character| character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '.' | '_' | '-')),
            "{} has an unexpected character",
            spec.key()
        );
        if let Some(default) = spec.default() {
            assert!(
                spec.kind().accepts(default),
                "{} has a default its kind rejects",
                spec.key()
            );
        } else {
            assert!(
                !spec.required(),
                "{} is required but has no default",
                spec.key()
            );
        }
    }
    identifiers.sort_unstable();
    identifiers.dedup();
    assert_eq!(identifiers.len(), 29);
}

#[test]
fn every_generated_default_renders_and_parses_again() {
    for spec in Schema::baseline().iter() {
        let Some(default) = spec.default() else {
            continue;
        };
        let text = default.render();
        let reparsed = spec
            .parse(&text)
            .unwrap_or_else(|error| panic!("{}: cannot reparse `{text}`: {error}", spec.key()));
        assert_eq!(
            &reparsed,
            default,
            "{} does not round trip through its rendering",
            spec.key()
        );
    }
}

#[test]
fn aliases_are_unique_and_resolve_to_their_owner() {
    let schema = Schema::baseline();
    let mut seen: Vec<&str> = Vec::new();
    for spec in schema.iter() {
        assert_eq!(
            schema.resolve(spec.key()).map(SettingSpec::key),
            Some(spec.key())
        );
        for alias in spec.aliases() {
            assert!(
                !seen.contains(&alias.as_str()),
                "alias `{alias}` is registered twice"
            );
            seen.push(alias.as_str());
            assert_eq!(
                schema.resolve(alias).map(SettingSpec::key),
                Some(spec.key()),
                "alias `{alias}` does not resolve to `{}`",
                spec.key()
            );
        }
    }
    assert_eq!(seen.len(), 14);
}

#[test]
fn a_commissioning_document_tunes_the_engine() {
    let mut engine = Engine::with_baseline();
    assert!(engine.check().is_ok());

    let document = "\
# commissioning sheet
loop.gain = 2.5
sample.averaging = 8
fluid.flow.nominal = 0.002 m3/s
sensor.return.temperature = 318 K
pump.efficiency = 0.74
";
    assert_eq!(engine.apply_text(document).expect("document applies"), 5);
    assert_eq!(engine.source_of("loop.gain"), Some(Source::File));
    assert_eq!(engine.get_float("loop.gain").expect("float"), 2.5);
    assert_eq!(engine.get_int("sample.averaging").expect("int"), 8);

    let nominal = engine.get_quantity("fluid.flow.nominal").expect("quantity");
    assert_eq!(nominal.canonical(), 0.002);
    let litres = unit::lookup("l/s").expect("l/s is registered");
    let per_second = engine
        .get_quantity_in("fluid.flow.nominal", litres)
        .expect("compatible");
    assert_eq!(per_second.value(), 2.0);

    let report = engine.validate();
    assert!(report.is_ok(), "unexpected issues: {:?}", report.issues());
    assert_eq!(report.checked(), 35);

    let snapshot = engine.snapshot();
    engine.set("loop.gain", "3").expect("in range");
    let differences = engine.diff(&snapshot);
    assert_eq!(differences.len(), 1);
    assert_eq!(differences[0].render(), "loop.gain: 2.5 -> 3");
    assert_eq!(engine.take_log().len(), 6);
    assert!(engine.log().is_empty());
}

#[test]
fn broken_documents_report_their_line() {
    let mut engine = Engine::with_baseline();
    let error = engine
        .apply_text("loop.gain = 2.5\nthis line is broken\n")
        .expect_err("the document is broken");
    assert_eq!(
        error,
        ConfigError::Syntax {
            location: "line 2".to_string(),
            text: "this line is broken".to_string(),
            reason: "expected `key = value`".to_string()
        }
    );
    assert_eq!(
        engine.get_float("loop.gain").expect("first line applied"),
        2.5
    );

    let error = engine
        .apply_text("sample.averaging = 128")
        .expect_err("the value is out of range");
    assert!(error.to_string().contains("`128` is outside 1..64"));
}

#[test]
fn cross_field_rules_catch_inconsistent_values() {
    let mut engine = Engine::with_baseline();
    engine
        .set("sensor.supply.temperature", "310 K")
        .expect("in range");
    engine
        .set("sensor.return.temperature", "330 K")
        .expect("in range");
    let report = engine.validate();
    assert_eq!(report.errors().len(), 1);
    assert_eq!(report.errors()[0].key(), "sensor.supply.temperature");
    assert_eq!(
        report.errors()[0].message(),
        "supply temperature must exceed the return temperature"
    );
    assert_eq!(
        engine.check(),
        Err(ConfigError::Validation {
            errors: 1,
            first: "supply temperature must exceed the return temperature".to_string()
        })
    );
}

#[test]
fn an_application_can_extend_the_schema() {
    let baseline = Schema::baseline();
    let mut overlay = Schema::new();
    overlay
        .insert(
            SettingSpec::builder("station.mode", Kind::text(16))
                .default_text("auto")
                .expect("default parses")
                .doc("operating mode of the station")
                .build()
                .expect("valid"),
        )
        .expect("insert succeeds");
    overlay
        .insert(
            SettingSpec::builder("station.mode.revision", Kind::int(0, 99))
                .default_value(SettingValue::Int(1))
                .build()
                .expect("valid"),
        )
        .expect("insert succeeds");

    let mut schema = baseline;
    assert_eq!(
        schema
            .merge(&overlay, MergePolicy::Reject)
            .expect("no conflict"),
        2
    );
    assert_eq!(schema.len(), 31);
    let engine = Engine::new(schema);
    assert_eq!(engine.get_text("station.mode").expect("default"), "auto");
    assert_eq!(engine.get_int("station.mode.revision").expect("int"), 1);
}

#[test]
fn the_build_script_tables_are_reachable() {
    let registry = unit::registry();
    assert!(registry.len() >= 60);
    let symbols = registry.symbols();
    let mut sorted = symbols.clone();
    sorted.sort_unstable();
    assert_eq!(symbols, sorted, "the unit table must stay sorted");
    sorted.dedup();
    assert_eq!(sorted.len(), symbols.len(), "unit symbols must be unique");

    let pascal = unit::resolve("Pa").expect("Pa is registered");
    assert_eq!(pascal.dimension(), macrokit::Dimension::PRESSURE);
    assert_eq!(pascal.scale(), 1.0);
    let kilopascal = unit::resolve("kPa").expect("kPa is registered");
    assert_eq!(
        kilopascal.convert(1.0, &pascal).expect("compatible"),
        1000.0
    );
    let seconds = unit::resolve("s").expect("s is registered");
    assert_eq!(
        registry.with_dimension(seconds.dimension()).len(),
        6,
        "time should keep six spellings"
    );
}

#[test]
fn describe_reports_the_public_spec_fields() {
    let schema = Schema::baseline();
    let spec = schema.require("sample.interval").expect("present");
    assert_eq!(SettingSpec::NAME, "setting");
    let description = spec.describe();
    assert_eq!(description.fields().len(), 8);
    assert_eq!(description.get("key"), Some("\"sample.interval\""));
    assert_eq!(description.get("required"), Some("true"));
    assert!(spec.has_field("aliases"));
    assert!(!spec.has_field("hidden"));
    assert_eq!(description.to_lines().len(), 8);
}
