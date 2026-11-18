//! Tests for the `#[derive(Describe)]` output, seen from outside the crate.

use macrokit::describe::Describe;
use macrokit::{CommandSpec, Engine, Kind, Range, Registry, SettingSpec};

#[test]
fn generic_containers_are_described() {
    let range = Range::new(0.5, 8.25);
    assert_eq!(Range::<f64>::NAME, "inclusive range");
    assert_eq!(range.field_names(), &["min", "max"]);
    let description = range.describe();
    assert_eq!(description.get("min"), Some("0.5"));
    assert_eq!(description.get("max"), Some("8.25"));
    assert_eq!(description.len(), 2);

    let integers = Range::new(3, 9);
    assert_eq!(integers.describe().get("min"), Some("3"));
    assert!(integers.has_field("max"));
}

#[test]
fn skipped_fields_are_not_reported() {
    let handler = Registry::builtin()
        .find("get")
        .expect("get is registered")
        .handler();
    let spec = CommandSpec::new(
        "example",
        "test",
        "an example command",
        "example",
        &["ex"],
        handler,
    );
    assert_eq!(CommandSpec::NAME, "command");
    assert_eq!(
        spec.field_names(),
        &["name", "group", "summary", "usage", "aliases"]
    );
    let description = spec.describe();
    assert_eq!(description.get("name"), Some("\"example\""));
    assert_eq!(description.get("aliases"), Some("[\"ex\"]"));
    assert_eq!(description.get("handler"), None);
    assert!(description.to_string().starts_with("name=\"example\""));
}

#[test]
fn specs_describe_every_field_in_declaration_order() {
    let spec = SettingSpec::builder("loop.gain", Kind::float(0.0, 1000.0))
        .default_text("1.25")
        .expect("default parses")
        .alias("kp")
        .doc("proportional gain")
        .build()
        .expect("valid");
    let description = spec.describe();
    let fields: Vec<&str> = description
        .entries()
        .iter()
        .map(|(field, _)| field.as_str())
        .collect();
    assert_eq!(
        fields,
        vec![
            "id",
            "key",
            "kind",
            "default",
            "required",
            "aliases",
            "doc",
            "deprecated"
        ]
    );
    assert_eq!(description.get("key"), Some("\"loop.gain\""));
    assert_eq!(description.get("required"), Some("false"));
    assert_eq!(description.get("aliases"), Some("[\"kp\"]"));
}

#[test]
fn descriptions_render_for_command_output() {
    let engine = Engine::with_baseline();
    let spec = engine.schema().require("pump.efficiency").expect("present");
    let lines = spec.describe().to_lines();
    assert!(
        lines.contains(&"kind = Float(Range { min: 0.2, max: 1.0 })".to_string()),
        "unexpected lines: {lines:?}"
    );
    assert!(lines.iter().any(|line| line == "key = \"pump.efficiency\""));
    assert_eq!(spec.describe().get("id"), Some("SettingId(17)"));
}
