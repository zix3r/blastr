//! A full station session: configure, derive, report and trip.

use macrokit::Source;
use macrokit_station::{Station, StationError};

fn station() -> Station {
    Station::new().expect("the baseline station builds")
}

#[test]
fn a_commissioning_session_derives_the_estimate() {
    let mut station = station();
    assert_eq!(station.registry().len(), 14);
    assert_eq!(station.engine().schema().len(), 29);

    let document = "\
site.name = bay-4
sensor.supply.temperature = 355 K
sensor.return.temperature = 315 K
fluid.flow.nominal = 0.002 m3/s
";
    assert_eq!(
        station.apply_document(document).expect("document applies"),
        4
    );
    assert_eq!(station.engine().source_of("site.name"), Some(Source::File));

    let estimate = station.estimate().expect("estimate works");
    assert!((estimate.mass_flow().canonical() - 1.996).abs() < 1e-12);
    assert!((estimate.duty().canonical() - 334_210.24).abs() < 1e-6);
    assert!((estimate.hydraulic_power().canonical() - 300.0).abs() < 1e-12);
    assert!((estimate.electrical_power().canonical() - 441.176_470_588_235_3).abs() < 1e-6);

    let outcome = station.run("duty").expect("duty works");
    assert_eq!(outcome.lines(), ["duty: 334210.24 W".to_string()]);
    let outcome = station.run("profile").expect("profile works");
    assert_eq!(outcome.lines()[0], "thermal profile: bay-4");
    let outcome = station.run("estimate").expect("estimate works");
    assert_eq!(outcome.lines()[0], "mass flow: 1.996 kg/s");
    assert_eq!(outcome.lines()[2], "hydraulic power: 300 W");
}

#[test]
fn the_report_combines_every_section() {
    let station = station();
    let report = station.report().expect("report works");
    assert!(report.starts_with("key"));
    assert!(report.contains("sample.interval"));
    assert!(report.contains("1 s"));
    assert!(report.contains("source"));
    assert!(report.contains("thermal profile: bay-1"));
    assert!(report.contains("electrical power: 551.470588 W"));
    assert!(report.ends_with("efficiency: 0.68"));

    let outcome = station.registry().help();
    assert!(outcome.contains("Estimate duty, hydraulic power and electrical power"));
}

#[test]
fn inconsistent_configurations_trip_the_station() {
    let mut station = station();
    station
        .run("set sensor.supply.temperature 300 K")
        .expect("set works");
    match station.estimate() {
        Err(StationError::Inconsistent { reason }) => {
            assert!(
                reason.contains("is not above the return temperature"),
                "unexpected reason: {reason}"
            );
        }
        other => panic!("unexpected result: {other:?}"),
    }

    station
        .run("set sensor.supply.temperature 360 K")
        .expect("set works");
    station.run("set power.limit 1000 W").expect("set works");
    match station.estimate() {
        Err(StationError::Inconsistent { reason }) => {
            assert!(reason.contains("exceeds the power budget"), "{reason}");
        }
        other => panic!("unexpected result: {other:?}"),
    }

    station.run("set power.limit 500 kW").expect("set works");
    station
        .run("set pump.efficiency 0")
        .expect_err("zero efficiency is out of range");
    assert!(
        (station.estimate().expect("estimate recovers"))
            .duty()
            .canonical()
            > 0.0
    );
}

#[test]
fn station_commands_coexist_with_the_library_commands() {
    let mut station = station();
    let names = station.registry().names();
    assert!(names.contains(&"duty"));
    assert!(names.contains(&"validate"));
    assert_eq!(names.len(), 14);

    let outcome = station.run("list pump.").expect("list works");
    assert_eq!(outcome.lines()[0], "pump.efficiency = 0.68 (default)");
    assert_eq!(outcome.lines()[1], "pump.head = 150000 Pa (default)");

    let outcome = station
        .run("get duty")
        .expect_err("commands are not settings");
    assert!(outcome.to_string().contains("unknown setting"));
    let outcome = station.run("audit").expect_err("audit is not registered");
    assert!(outcome.to_string().contains("unknown command"));
}
