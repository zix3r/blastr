//! Integration tests for the command layer.
//!
//! The scenarios mirror what an operator does on the terminal: read a value,
//! change it through an alias, convert units, apply a batch and validate.

use macrokit::{CmdError, ConfigError, Engine, Registry, Source};

fn session() -> (Engine, Registry) {
    (Engine::with_baseline(), Registry::builtin())
}

#[test]
fn a_commissioning_session_runs_end_to_end() {
    let (mut engine, registry) = session();
    let help = registry.help();
    assert!(help.starts_with("command"));
    assert!(help.contains("Show the schema entry of a setting"));

    let outcome = registry
        .dispatch(&mut engine, "describe loop.integral")
        .expect("describe works");
    assert_eq!(outcome.lines()[0], "loop.integral (setting)");
    assert!(
        outcome
            .lines()
            .contains(&"accepts: quantity of TIME1 in 0..3600 (canonical)".to_string())
    );

    let outcome = registry
        .dispatch(&mut engine, "set ti 45 s")
        .expect("the alias works");
    assert_eq!(outcome.lines(), ["loop.integral = 45 s".to_string()]);
    assert_eq!(
        engine
            .get_quantity("loop.integral")
            .expect("set")
            .canonical(),
        45.0
    );

    let outcome = registry
        .dispatch(&mut engine, "convert 45 s min")
        .expect("convert works");
    assert_eq!(outcome.lines(), ["45 s = 0.75 min".to_string()]);

    let outcome = registry
        .dispatch(&mut engine, "units LEN1")
        .expect("units works");
    assert!(
        outcome
            .lines()
            .contains(&"km (canonical = value * 1000)".to_string())
    );

    let outcome = registry
        .dispatch(&mut engine, "apply loop.gain=1.5 pump.efficiency=0.8")
        .expect("apply works");
    assert_eq!(outcome.lines(), ["applied 2 assignments".to_string()]);

    let outcome = registry
        .dispatch(&mut engine, "validate")
        .expect("validate works");
    assert!(
        outcome.lines()[0].starts_with("ok: 35 checks"),
        "unexpected validation line: {}",
        outcome.lines()[0]
    );
    assert_eq!(engine.source_of("pump.efficiency"), Some(Source::Argument));
}

#[test]
fn list_reports_the_effective_values() {
    let (mut engine, registry) = session();
    registry
        .dispatch(&mut engine, "set return.temp 312 K")
        .expect("set works");
    let outcome = registry
        .dispatch(&mut engine, "list sensor.")
        .expect("list works");
    assert_eq!(outcome.len(), 3);
    assert_eq!(
        outcome.lines()[0],
        "sensor.return.temperature = 312 K (argument)"
    );
    assert_eq!(
        outcome.lines()[1],
        "sensor.supply.pressure = 101325 Pa (default)"
    );
    assert_eq!(
        outcome.lines()[2],
        "sensor.supply.temperature = 350 K (default)"
    );
}

#[test]
fn arguments_keep_their_quoting() {
    let (mut engine, registry) = session();
    registry
        .dispatch(&mut engine, "set site.name \"bay 9 north\"")
        .expect("set works");
    assert_eq!(engine.get_text("site.name").expect("text"), "bay 9 north");
    let outcome = registry
        .dispatch(&mut engine, "get site.name")
        .expect("get works");
    assert_eq!(outcome.lines()[0], "site.name = bay 9 north");
}

#[test]
fn dispatch_errors_are_reported_precisely() {
    let (mut engine, registry) = session();
    assert_eq!(
        registry.dispatch(&mut engine, ""),
        Err(CmdError::EmptyCommandLine)
    );
    assert_eq!(
        registry.dispatch(&mut engine, "lst"),
        Err(CmdError::UnknownCommand {
            name: "lst".to_string(),
            suggestion: Some("list".to_string())
        })
    );
    match registry.dispatch(&mut engine, "set sample.averaging 100") {
        Err(CmdError::Config(ConfigError::InvalidValue { key, reason })) => {
            assert_eq!(key, "sample.averaging");
            assert_eq!(reason, "`100` is outside 1..64");
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(
        registry.dispatch(&mut engine, "convert 1 bar"),
        Err(CmdError::MissingArgument {
            command: "convert",
            argument: "to"
        })
    );
    assert!(matches!(
        registry.dispatch(&mut engine, "units NOPE1"),
        Err(CmdError::Unit(_))
    ));
}

#[test]
fn resetting_and_unsetting_keep_the_engine_consistent() {
    let (mut engine, registry) = session();
    registry
        .dispatch(&mut engine, "set loop.gain 4")
        .expect("set works");
    registry
        .dispatch(&mut engine, "set sample.averaging 16")
        .expect("set works");
    assert_eq!(engine.count_by_source()[&Source::Argument], 2);

    let outcome = registry
        .dispatch(&mut engine, "reset")
        .expect("reset works");
    assert_eq!(
        outcome.lines(),
        ["reset every setting to its default".to_string()]
    );
    assert_eq!(engine.get_float("loop.gain").expect("default"), 1.25);
    assert_eq!(engine.get_int("sample.averaging").expect("default"), 4);
    assert_eq!(engine.count_by_source()[&Source::Default], 28);

    registry
        .dispatch(&mut engine, "set sample.averaging 32")
        .expect("set works");
    let outcome = registry
        .dispatch(&mut engine, "unset sample.averaging")
        .expect("unset works");
    assert_eq!(
        outcome.lines(),
        [
            "unset sample.averaging".to_string(),
            "effective: 4".to_string()
        ]
    );
    assert!(!engine.is_set("sample.averaging"));
}
