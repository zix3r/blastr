//! The macros of `macrokit` used from a third crate.
//!
//! `macrokit-station` already uses them; this test crate shows the same thing
//! with types and commands that exist only in a test, which is how a downstream
//! application would extend the library.

use macrokit::commands::{CmdError, CmdOutcome, CmdResult, CommandSpec, Registry};
use macrokit::{Describe, Engine, SettingValue, command};

/// A reading that a downstream crate wants to describe.
#[derive(Debug, Clone, PartialEq, Describe)]
#[describe(name = "test reading")]
struct Reading {
    label: String,
    #[describe(rename = "kelvin")]
    value: f64,
    #[describe(skip)]
    seen: bool,
}

impl Reading {
    fn new(label: &str, value: f64) -> Self {
        Self {
            label: label.to_string(),
            value,
            seen: false,
        }
    }
}

/// Counts the effective settings; registered with the attribute macro.
#[command(
    name = "audit",
    group = "test",
    summary = "Count the effective settings",
    usage = "audit",
    aliases = ["au"]
)]
fn audit(engine: &mut Engine, args: &[&str]) -> CmdResult {
    macrokit::ensure!(
        args.is_empty(),
        CmdError::TooManyArguments {
            command: "audit",
            expected: 0
        }
    );
    let settings = engine.effective();
    let quantities = settings
        .iter()
        .filter(|entry| matches!(entry.value(), SettingValue::Quantity(_)))
        .count();
    let mut outcome = CmdOutcome::new();
    outcome.push(format!("{} effective settings", settings.len()));
    outcome.push(format!("{quantities} quantities"));
    Ok(outcome)
}

macrokit::command_table! {
    /// The commands this test crate registers.
    EXTRA_COMMANDS = [__command_spec_audit];
}

macrokit::command_table! {
    /// An empty table, to check the trailing semicolon form.
    pub(crate) NO_COMMANDS = [];
}

macrokit::newtype_ids! {
    ReadingId => "reading#",
}

fn registry() -> Registry {
    let mut specs: Vec<CommandSpec> = macrokit::commands::builtin_specs();
    specs.extend_from_slice(EXTRA_COMMANDS);
    Registry::new(specs).expect("the combined registry is consistent")
}

#[test]
fn an_attribute_macro_command_joins_the_registry() {
    let registry = registry();
    assert_eq!(registry.len(), 11);
    assert_eq!(registry.find("au").map(CommandSpec::name), Some("audit"));
    assert_eq!(registry.groups(), vec!["read", "test", "units", "write"]);

    let mut engine = Engine::with_baseline();
    let outcome = registry
        .dispatch(&mut engine, "audit")
        .expect("audit works");
    assert_eq!(
        outcome.lines(),
        [
            "28 effective settings".to_string(),
            "17 quantities".to_string()
        ]
    );
    assert!(registry.dispatch(&mut engine, "audit now").is_err());
    assert_eq!(EXTRA_COMMANDS.len(), 1);
    assert_eq!(EXTRA_COMMANDS[0].usage(), "audit");
    assert!(NO_COMMANDS.is_empty());
}

#[test]
fn a_derived_description_in_this_crate_exposes_renamed_fields() {
    let reading = Reading::new("supply", 350.0);
    assert_eq!(Reading::NAME, "test reading");
    assert_eq!(reading.field_names(), &["label", "kelvin"]);
    let description = reading.describe();
    assert_eq!(description.get("label"), Some("\"supply\""));
    assert_eq!(description.get("kelvin"), Some("350.0"));
    assert_eq!(description.get("seen"), None);
    assert_eq!(description.len(), 2);
    assert!(reading.has_field("kelvin"));
    assert!(!reading.has_field("value"));
    assert_eq!(
        description.to_lines(),
        vec![
            "label = \"supply\"".to_string(),
            "kelvin = 350.0".to_string()
        ]
    );
}

#[test]
fn generated_identifiers_and_guards_work_downstream() {
    let id = ReadingId::new(7);
    assert_eq!(id.get(), 7);
    assert_eq!(id.to_string(), "reading#7");
    assert!(!id.is_unassigned());
    assert!(ReadingId::unassigned().is_unassigned());
    assert!(ReadingId::new(1) < ReadingId::new(2));

    let guard = |value: Option<f64>| -> Result<f64, String> {
        macrokit::ensure!(
            let Some(number) = value,
            otherwise "missing".to_string()
        );
        macrokit::ensure!(number > 0.0, "not positive".to_string());
        Ok(number)
    };
    assert_eq!(guard(Some(2.5)), Ok(2.5));
    assert_eq!(guard(None), Err("missing".to_string()));
    assert_eq!(guard(Some(-1.0)), Err("not positive".to_string()));

    let mut engine = Engine::with_baseline();
    engine.set("site.name", "bay-2").expect("in range");
    assert_eq!(engine.get_text("site.name").expect("text"), "bay-2");
    assert_eq!(engine.effective().len(), 28);
}
