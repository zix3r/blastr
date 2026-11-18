//! The station itself: engine, command registry and derived estimates.

use macrokit::commands::{CmdError, CmdOutcome, CmdResult, CommandSpec, Registry};
use macrokit::{ConfigError, Describe, Engine, Quantity, Source, Unit, UnitError, command};

use crate::profile::ThermalProfile;
use crate::profile::{POWER, WATT, measurement, product};

/// Result alias used across the station crate.
pub type StationResult<T> = Result<T, StationError>;

/// Everything that can go wrong while configuring the station.
#[derive(Debug, Clone, PartialEq)]
pub enum StationError {
    /// The configuration engine rejected an operation.
    Config(ConfigError),
    /// The command registry rejected an operation.
    Command(CmdError),
    /// The unit layer rejected an operation.
    Unit(UnitError),
    /// The configuration is complete but physically inconsistent.
    Inconsistent {
        /// Why the configuration was rejected.
        reason: String,
    },
}

impl std::fmt::Display for StationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => std::fmt::Display::fmt(error, formatter),
            Self::Command(error) => std::fmt::Display::fmt(error, formatter),
            Self::Unit(error) => std::fmt::Display::fmt(error, formatter),
            Self::Inconsistent { reason } => {
                write!(formatter, "inconsistent configuration: {reason}")
            }
        }
    }
}

impl std::error::Error for StationError {}

impl From<ConfigError> for StationError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<CmdError> for StationError {
    fn from(error: CmdError) -> Self {
        Self::Command(error)
    }
}

impl From<UnitError> for StationError {
    fn from(error: UnitError) -> Self {
        Self::Unit(error)
    }
}

/// The estimates the station derives from its configuration.
#[derive(Debug, Clone, PartialEq, Describe)]
#[describe(name = "station estimate")]
pub struct Estimate {
    mass_flow: Quantity,
    duty: Quantity,
    hydraulic_power: Quantity,
    electrical_power: Quantity,
    efficiency: f64,
}

impl Estimate {
    /// Derives the estimate from the effective configuration.
    ///
    /// # Errors
    ///
    /// Returns [`StationError`] when a setting is missing, the profile is
    /// inconsistent or the pump efficiency is unusable.
    pub fn compute(engine: &Engine) -> StationResult<Self> {
        let profile = ThermalProfile::from_engine(engine)?;
        profile.validate()?;
        let mass_flow = profile.mass_flow()?;
        let duty = profile.duty()?;
        let head = measurement(engine, "pump.head")?;
        let hydraulic = product(profile.flow(), head, POWER, WATT, "flow times head")?;
        let efficiency = engine.get_float("pump.efficiency")?;
        macrokit::ensure!(
            efficiency > 0.0 && efficiency <= 1.0,
            StationError::Inconsistent {
                reason: format!("pump efficiency {efficiency} is outside 0..1"),
            }
        );
        Ok(Self {
            mass_flow,
            duty,
            hydraulic_power: hydraulic,
            electrical_power: hydraulic.scale_by(1.0 / efficiency),
            efficiency,
        })
    }

    /// The mass flow through the loop.
    pub fn mass_flow(&self) -> Quantity {
        self.mass_flow
    }

    /// The thermal duty of the loop.
    pub fn duty(&self) -> Quantity {
        self.duty
    }

    /// The hydraulic power the pump delivers.
    pub fn hydraulic_power(&self) -> Quantity {
        self.hydraulic_power
    }

    /// The electrical power the pump draws.
    pub fn electrical_power(&self) -> Quantity {
        self.electrical_power
    }

    /// The efficiency used for the electrical power.
    pub fn efficiency(&self) -> f64 {
        self.efficiency
    }

    /// How much of the power budget the estimate uses, clamped to `0..=1`.
    pub fn budget_share(&self, limit: Quantity) -> StationResult<f64> {
        let share = self.duty.ratio(&limit)?;
        Ok(share.clamp(0.0, 1.0))
    }

    /// The estimate as the `estimate` command prints it.
    pub fn summary(&self) -> Vec<String> {
        vec![
            format!("mass flow: {}", self.mass_flow.humanize()),
            format!("duty: {}", self.duty.humanize()),
            format!("hydraulic power: {}", self.hydraulic_power.humanize()),
            format!("electrical power: {}", self.electrical_power.humanize()),
            format!(
                "efficiency: {}",
                macrokit::util::format_float(self.efficiency)
            ),
        ]
    }
}

/// A configured station: an engine, a registry and the derived estimates.
#[derive(Debug, Clone)]
pub struct Station {
    engine: Engine,
    registry: Registry,
}

impl Station {
    /// Builds a station on the generated baseline schema.
    ///
    /// # Errors
    ///
    /// Returns [`StationError`] when the schema does not provide the settings
    /// the station needs or when the command table is inconsistent.
    pub fn new() -> StationResult<Self> {
        let engine = Engine::with_baseline();
        let mut specs: Vec<CommandSpec> = macrokit::commands::builtin_specs();
        specs.extend_from_slice(STATION_COMMANDS);
        let registry = Registry::new(specs)?;
        let station = Self { engine, registry };
        station.check_schema()?;
        Ok(station)
    }

    /// Verifies that every setting the station reads exists and is a quantity.
    ///
    /// # Errors
    ///
    /// Returns [`StationError::Inconsistent`] for a missing key or a setting
    /// of the wrong kind.
    pub fn check_schema(&self) -> StationResult<()> {
        for key in ThermalProfile::REQUIRED_KEYS {
            let spec =
                self.engine
                    .schema()
                    .resolve(key)
                    .ok_or_else(|| StationError::Inconsistent {
                        reason: format!("setting `{key}` is missing from the schema"),
                    })?;
            macrokit::ensure!(
                let Some(dimension) = spec.kind().dimension(),
                otherwise StationError::Inconsistent {
                    reason: format!("setting `{key}` must be a quantity"),
                }
            );
            macrokit::ensure!(
                !dimension.is_dimensionless(),
                StationError::Inconsistent {
                    reason: format!("setting `{key}` has no dimension"),
                }
            );
        }
        Ok(())
    }

    /// The configuration engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The configuration engine, mutably.
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// The command registry, including the station commands.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The current thermal profile.
    ///
    /// # Errors
    ///
    /// See [`ThermalProfile::from_engine`].
    pub fn profile(&self) -> StationResult<ThermalProfile> {
        ThermalProfile::from_engine(&self.engine)
    }

    /// The current estimate.
    ///
    /// # Errors
    ///
    /// See [`Estimate::compute`].
    pub fn estimate(&self) -> StationResult<Estimate> {
        Estimate::compute(&self.engine)
    }

    /// Applies a configuration document.
    ///
    /// # Errors
    ///
    /// Returns [`StationError::Config`] for malformed documents.
    pub fn apply_document(&mut self, document: &str) -> StationResult<usize> {
        Ok(self.engine.apply_text(document)?)
    }

    /// Applies one command line.
    ///
    /// # Errors
    ///
    /// Returns [`StationError::Command`] for unknown commands, bad arguments
    /// and engine errors.
    pub fn run(&mut self, line: &str) -> StationResult<CmdOutcome> {
        Ok(self.registry.dispatch(&mut self.engine, line)?)
    }

    /// A multi section report: settings, profile and estimate.
    ///
    /// # Errors
    ///
    /// Returns [`StationError`] when the profile or the estimate cannot be
    /// derived.
    pub fn report(&self) -> StationResult<String> {
        let profile = self.profile()?;
        let estimate = self.estimate()?;
        let mut lines = vec![
            self.engine.report(),
            String::new(),
            profile.describe_lines().join("\n"),
            String::new(),
        ];
        lines.extend(estimate.summary());
        Ok(lines.join("\n"))
    }
}

fn to_command_error(error: StationError) -> CmdError {
    match error {
        StationError::Config(inner) => CmdError::Config(inner),
        StationError::Command(inner) => inner,
        StationError::Unit(inner) => CmdError::Unit(inner),
        StationError::Inconsistent { reason } => CmdError::BadUsage {
            command: "station",
            hint: reason,
        },
    }
}

/// Prints the effective thermal profile.
#[command(
    name = "profile",
    group = "station",
    summary = "Print the effective thermal profile",
    usage = "profile"
)]
pub fn profile_command(engine: &mut Engine, args: &[&str]) -> CmdResult {
    macrokit::ensure!(
        args.is_empty(),
        CmdError::TooManyArguments {
            command: "profile",
            expected: 0
        }
    );
    let profile = ThermalProfile::from_engine(engine).map_err(to_command_error)?;
    let mut outcome = CmdOutcome::new();
    outcome.extend(profile.describe_lines());
    Ok(outcome)
}

/// Prints the derived estimate of duty and pump power.
#[command(
    name = "estimate",
    group = "station",
    summary = "Estimate duty, hydraulic power and electrical power",
    usage = "estimate"
)]
pub fn estimate_command(engine: &mut Engine, args: &[&str]) -> CmdResult {
    macrokit::ensure!(
        args.is_empty(),
        CmdError::TooManyArguments {
            command: "estimate",
            expected: 0
        }
    );
    let estimate = Estimate::compute(engine).map_err(to_command_error)?;
    let mut outcome = CmdOutcome::new();
    outcome.extend(estimate.summary());
    Ok(outcome)
}

/// Prints only the thermal duty.
#[command(
    name = "duty",
    group = "station",
    summary = "Print the thermal duty of the loop",
    usage = "duty"
)]
pub fn duty_command(engine: &mut Engine, args: &[&str]) -> CmdResult {
    macrokit::ensure!(
        args.is_empty(),
        CmdError::TooManyArguments {
            command: "duty",
            expected: 0
        }
    );
    let estimate = Estimate::compute(engine).map_err(to_command_error)?;
    let mut outcome = CmdOutcome::new();
    outcome.push(format!("duty: {}", estimate.duty().humanize()));
    Ok(outcome)
}

/// Prints the report of the whole station.
#[command(
    name = "report",
    group = "station",
    summary = "Print settings, profile and estimate",
    usage = "report"
)]
pub fn report_command(engine: &mut Engine, args: &[&str]) -> CmdResult {
    macrokit::ensure!(
        args.is_empty(),
        CmdError::TooManyArguments {
            command: "report",
            expected: 0
        }
    );
    let profile = ThermalProfile::from_engine(engine).map_err(to_command_error)?;
    let estimate = Estimate::compute(engine).map_err(to_command_error)?;
    let mut outcome = CmdOutcome::new();
    outcome.extend(profile.describe_lines());
    outcome.extend(estimate.summary());
    Ok(outcome)
}

macrokit::command_table! {
    /// The commands the station crate adds to the macrokit registry.
    pub(crate) STATION_COMMANDS = [
        __command_spec_duty_command,
        __command_spec_estimate_command,
        __command_spec_profile_command,
        __command_spec_report_command,
    ];
}

/// A unit the station defines itself, to show how `Unit::new` composes.
pub const JOULE: Unit = Unit::new("J", macrokit::Dimension::ENERGY, 1.0, 0.0);

/// Sets a value with the file source, as a configuration loader would.
///
/// # Errors
///
/// Returns [`StationError::Config`] for unknown keys and invalid values.
pub fn apply_file_value(station: &mut Station, key: &str, value: &str) -> StationResult<()> {
    station.engine.set_from(key, value, Source::File)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Estimate, JOULE, Station, StationError, apply_file_value, to_command_error};
    use macrokit::{Describe, Source};

    #[test]
    fn the_station_registers_its_own_commands() {
        let station = Station::new().expect("station builds");
        let registry = station.registry();
        assert_eq!(registry.len(), 14);
        assert!(registry.names().contains(&"profile"));
        assert!(registry.names().contains(&"estimate"));
        assert!(registry.names().contains(&"duty"));
        assert!(registry.names().contains(&"report"));
        assert!(registry.groups().contains(&"station"));
        assert_eq!(super::STATION_COMMANDS.len(), 4);
    }

    #[test]
    fn station_commands_read_the_engine() {
        let mut station = Station::new().expect("station builds");
        let outcome = station.run("profile").expect("profile works");
        assert_eq!(outcome.lines()[0], "thermal profile: bay-1");
        assert!(outcome.lines().len() >= 8);
        let outcome = station.run("duty").expect("duty works");
        assert_eq!(outcome.lines(), ["duty: 313322.1 W".to_string()]);
        let outcome = station.run("estimate").expect("estimate works");
        assert_eq!(
            outcome.lines(),
            vec![
                "mass flow: 2.495 kg/s".to_string(),
                "duty: 313322.1 W".to_string(),
                "hydraulic power: 375 W".to_string(),
                "electrical power: 551.470588 W".to_string(),
                "efficiency: 0.68".to_string(),
            ]
        );
        let outcome = station.run("report").expect("report works");
        assert!(outcome.lines().len() >= 13);
    }

    #[test]
    fn inherited_commands_still_work() {
        let mut station = Station::new().expect("station builds");
        let outcome = station.run("get loop.gain").expect("get works");
        assert_eq!(outcome.lines()[0], "loop.gain = 1.25");
        station.run("set loop.gain 2").expect("set works");
        assert_eq!(station.engine().get_float("loop.gain").expect("float"), 2.0);
        let outcome = station.run("validate").expect("validate works");
        assert_eq!(outcome.lines()[0], "ok: 35 checks, no issues");
        assert!(
            station.run("zork").is_err(),
            "unknown commands are reported"
        );
        assert!(station.run("estimate extra").is_err());
        assert!(station.run("profile extra").is_err());
        assert!(station.run("duty extra").is_err());
        assert!(station.run("report extra").is_err());
    }

    #[test]
    fn estimates_follow_configuration_changes() {
        let mut station = Station::new().expect("station builds");
        station
            .run("set fluid.flow.nominal 0.005 m3/s")
            .expect("set works");
        station
            .run("set fluid.flow.maximum 0.01 m3/s")
            .expect("set works");
        station
            .run("set heat.design.duty 700 kW")
            .expect("set works");
        station.run("set power.limit 1000 kW").expect("set works");
        let estimate = station.estimate().expect("estimate works");
        assert!((estimate.mass_flow().canonical() - 4.99).abs() < 1e-12);
        assert!((estimate.duty().canonical() - 626_644.2).abs() < 1e-6);
        assert_eq!(estimate.hydraulic_power().canonical(), 750.0);
        assert!((estimate.electrical_power().canonical() - 1_102.941_176_470_588_2).abs() < 1e-6);
        assert_eq!(estimate.efficiency(), 0.68);
        let limit = station
            .engine()
            .get_quantity("power.limit")
            .expect("has a value");
        assert!((estimate.budget_share(limit).expect("compatible") - 0.626_644_2).abs() < 1e-9);
    }

    #[test]
    fn inconsistent_configurations_are_reported() {
        let mut station = Station::new().expect("station builds");
        station
            .run("set sensor.supply.temperature 300 K")
            .expect("set works");
        let error = station.estimate().expect_err("drop is zero");
        assert!(matches!(error, StationError::Inconsistent { .. }));
        assert!(error.to_string().starts_with("inconsistent configuration:"));
        assert!(matches!(
            to_command_error(error),
            macrokit::CmdError::BadUsage { .. }
        ));
        station
            .run("set sensor.supply.temperature 360 K")
            .expect("set works");
        station.run("set power.limit 1000 W").expect("set works");
        let error = station.estimate().expect_err("duty is too high");
        assert!(error.to_string().contains("exceeds the power budget"));
    }

    #[test]
    fn documents_can_be_applied() {
        let mut station = Station::new().expect("station builds");
        let applied = station
            .apply_document("# site tuning\nloop.gain = 2.5\nsample.averaging = 8\n")
            .expect("document applies");
        assert_eq!(applied, 2);
        assert_eq!(station.engine().get_float("loop.gain").expect("float"), 2.5);
        assert_eq!(
            station.engine().get_int("sample.averaging").expect("int"),
            8
        );
        assert_eq!(station.engine().source_of("loop.gain"), Some(Source::File));
        apply_file_value(&mut station, "logging.level", "debug").expect("value applies");
        assert_eq!(
            station.engine().get_text("logging.level").expect("text"),
            "debug"
        );
        assert!(apply_file_value(&mut station, "nope", "1").is_err());
        assert!(station.apply_document("broken").is_err());
    }

    #[test]
    fn the_report_contains_every_section() {
        let station = Station::new().expect("station builds");
        let report = station.report().expect("report works");
        assert!(report.contains("fluid.density"));
        assert!(report.contains("thermal profile: bay-1"));
        assert!(report.contains("electrical power: 551.470588 W"));
        assert!(report.starts_with("key"));
        assert_eq!(JOULE.symbol(), "J");
    }

    #[test]
    fn the_estimate_describes_itself() {
        let station = Station::new().expect("station builds");
        let estimate = station.estimate().expect("estimate works");
        assert_eq!(Estimate::NAME, "station estimate");
        assert_eq!(
            estimate.field_names(),
            &[
                "mass_flow",
                "duty",
                "hydraulic_power",
                "electrical_power",
                "efficiency"
            ]
        );
        assert!(estimate.describe().get("duty").is_some());
        assert_eq!(estimate.describe().get("efficiency"), Some("0.68"));
        assert!(estimate.has_field("mass_flow"));
        assert!(!estimate.has_field("pressure"));
    }
}
