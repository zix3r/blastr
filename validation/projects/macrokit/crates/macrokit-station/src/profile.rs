//! The thermal profile the station is configured for.
//!
//! Every field is read from a macrokit [`Engine`], so the profile is always a
//! view of the effective configuration rather than a second copy of it.

use macrokit::{Describe, Dimension, Engine, Quantity, Unit};

use crate::station::StationError;

/// Dimension of a mass flow, `MASS1 TIME-1`.
pub const MASS_FLOW: Dimension = Dimension::from_exponents([0, 1, -1, 0, 0, 0, 0]);
/// Canonical unit of a mass flow.
pub const KILOGRAM_PER_SECOND: Unit = Unit::new("kg/s", MASS_FLOW, 1.0, 0.0);
/// Dimension of a heat rate per kelvin, `MASS1 LEN2 TIME-3 TEMP-1`.
pub const HEAT_RATE: Dimension = Dimension::from_exponents([2, 1, -3, 0, -1, 0, 0]);
/// Canonical unit of a heat rate per kelvin.
pub const WATT_PER_KELVIN: Unit = Unit::new("W/K", HEAT_RATE, 1.0, 0.0);
/// Dimension of power, `MASS1 LEN2 TIME-3`.
pub const POWER: Dimension = Dimension::from_exponents([2, 1, -3, 0, 0, 0, 0]);
/// Canonical unit of power.
pub const WATT: Unit = Unit::new("W", POWER, 1.0, 0.0);

/// Multiplies two quantities and checks the dimension of the product.
///
/// macrokit only converts between units of one dimension, so products of
/// quantities are built here, where the resulting unit is known.
pub(crate) fn product(
    left: Quantity,
    right: Quantity,
    dimension: Dimension,
    unit: Unit,
    context: &str,
) -> Result<Quantity, StationError> {
    let actual = left.dimension() * right.dimension();
    macrokit::ensure!(
        actual == dimension,
        StationError::Inconsistent {
            reason: format!(
                "{context}: `{}` is not `{}`",
                actual.describe(),
                dimension.describe()
            ),
        }
    );
    Ok(Quantity::from_canonical(
        left.canonical() * right.canonical(),
        unit,
    ))
}

/// Reads a quantity setting, reporting a missing key as an inconsistency.
pub(crate) fn measurement(engine: &Engine, key: &str) -> Result<Quantity, StationError> {
    match engine.get_quantity(key) {
        Ok(value) => Ok(value),
        Err(error) => Err(StationError::Inconsistent {
            reason: format!("`{key}` is not usable as a measurement: {error}"),
        }),
    }
}

/// The effective thermal profile of the station.
#[derive(Debug, Clone, PartialEq, Describe)]
#[describe(name = "thermal profile")]
pub struct ThermalProfile {
    name: String,
    supply: Quantity,
    return_temperature: Quantity,
    flow: Quantity,
    density: Quantity,
    heat_capacity: Quantity,
    duty_limit: Quantity,
    #[describe(skip)]
    revision: u32,
}

impl ThermalProfile {
    /// Settings the profile needs; checked by [`ThermalProfile::from_engine`].
    pub const REQUIRED_KEYS: [&'static str; 6] = [
        "fluid.density",
        "fluid.flow.nominal",
        "fluid.heat.capacity",
        "power.limit",
        "sensor.return.temperature",
        "sensor.supply.temperature",
    ];

    /// Reads the profile from the effective configuration.
    ///
    /// # Errors
    ///
    /// Returns [`StationError`] when a required setting is missing or has the
    /// wrong kind.
    pub fn from_engine(engine: &Engine) -> Result<Self, StationError> {
        Ok(Self {
            name: engine.get_text("site.name")?,
            supply: measurement(engine, "sensor.supply.temperature")?,
            return_temperature: measurement(engine, "sensor.return.temperature")?,
            flow: measurement(engine, "fluid.flow.nominal")?,
            density: measurement(engine, "fluid.density")?,
            heat_capacity: measurement(engine, "fluid.heat.capacity")?,
            duty_limit: measurement(engine, "power.limit")?,
            revision: engine.revision(),
        })
    }

    /// The operator label of the site.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Supply side temperature.
    pub fn supply(&self) -> Quantity {
        self.supply
    }

    /// Return side temperature.
    pub fn return_temperature(&self) -> Quantity {
        self.return_temperature
    }

    /// Nominal volumetric flow.
    pub fn flow(&self) -> Quantity {
        self.flow
    }

    /// Density of the working fluid.
    pub fn density(&self) -> Quantity {
        self.density
    }

    /// Specific heat capacity of the working fluid.
    pub fn heat_capacity(&self) -> Quantity {
        self.heat_capacity
    }

    /// Power budget of the station.
    pub fn duty_limit(&self) -> Quantity {
        self.duty_limit
    }

    /// Engine revision the profile was read at.
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// The temperature drop across the loop.
    ///
    /// # Errors
    ///
    /// Returns [`StationError`] when the two temperatures do not match.
    pub fn drop(&self) -> Result<Quantity, StationError> {
        Ok(self.supply.sub(&self.return_temperature)?)
    }

    /// The mass flow through the loop.
    ///
    /// # Errors
    ///
    /// Returns [`StationError::Inconsistent`] when the dimensions do not
    /// multiply to `MASS1 TIME-1`.
    pub fn mass_flow(&self) -> Result<Quantity, StationError> {
        product(
            self.density,
            self.flow,
            MASS_FLOW,
            KILOGRAM_PER_SECOND,
            "density times flow",
        )
    }

    /// The thermal duty of the loop, `mass flow * heat capacity * drop`.
    ///
    /// # Errors
    ///
    /// Returns [`StationError`] when the dimensions do not multiply to power.
    pub fn duty(&self) -> Result<Quantity, StationError> {
        let mass_flow = self.mass_flow()?;
        let drop = self.drop()?;
        let heat_rate = product(
            mass_flow,
            self.heat_capacity,
            HEAT_RATE,
            WATT_PER_KELVIN,
            "mass flow times heat capacity",
        )?;
        // Multiplying by the temperature drop cancels the inverse kelvin, so
        // the result is a plain power.
        Ok(Quantity::from_canonical(
            heat_rate.canonical() * drop.canonical(),
            WATT,
        ))
    }

    /// Checks that the profile describes a physically sensible loop.
    ///
    /// # Errors
    ///
    /// Returns [`StationError::Inconsistent`] for a zero flow, a non positive
    /// temperature drop and a duty above the power budget.
    pub fn validate(&self) -> Result<(), StationError> {
        macrokit::ensure!(
            !self.flow.is_zero(0.0),
            StationError::Inconsistent {
                reason: "nominal flow must be positive".to_string(),
            }
        );
        let drop = self.drop()?;
        macrokit::ensure!(
            drop.canonical() > 0.0,
            StationError::Inconsistent {
                reason: format!(
                    "supply temperature {} is not above the return temperature {}",
                    self.supply.humanize(),
                    self.return_temperature.humanize()
                ),
            }
        );
        let duty = self.duty()?;
        macrokit::ensure!(
            duty.canonical() <= self.duty_limit.canonical(),
            StationError::Inconsistent {
                reason: format!(
                    "duty {} exceeds the power budget {}",
                    duty.humanize(),
                    self.duty_limit.humanize()
                ),
            }
        );
        Ok(())
    }

    /// The profile as the `profile` command prints it.
    pub fn describe_lines(&self) -> Vec<String> {
        let mut lines = vec![format!("{}: {}", <Self as Describe>::NAME, self.name)];
        lines.extend(self.describe().to_lines());
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::{ThermalProfile, product};
    use crate::station::{Station, StationError};
    use macrokit::{Describe, Dimension, Engine, Quantity, Unit};

    #[test]
    fn the_baseline_profile_is_read_from_the_engine() {
        let engine = Engine::with_baseline();
        let profile = ThermalProfile::from_engine(&engine).expect("baseline profile");
        assert_eq!(profile.name(), "bay-1");
        assert_eq!(profile.supply().canonical(), 350.0);
        assert_eq!(profile.return_temperature().canonical(), 320.0);
        assert_eq!(profile.flow().canonical(), 0.0025);
        assert_eq!(profile.density().canonical(), 998.0);
        assert_eq!(profile.heat_capacity().canonical(), 4186.0);
        assert_eq!(profile.duty_limit().canonical(), 350_000.0);
        assert_eq!(profile.revision(), 0);
    }

    #[test]
    fn derived_values_use_canonical_units() {
        let profile = ThermalProfile::from_engine(&Engine::with_baseline()).expect("profile");
        let drop = profile.drop().expect("compatible");
        assert_eq!(drop.canonical(), 30.0);
        assert_eq!(drop.unit().symbol(), "K");
        let mass_flow = profile.mass_flow().expect("dimensions multiply");
        assert!((mass_flow.canonical() - 2.495).abs() < 1e-12);
        assert_eq!(mass_flow.unit().symbol(), "kg/s");
        let duty = profile.duty().expect("dimensions multiply");
        assert!((duty.canonical() - 313_322.1).abs() < 1e-6);
        assert_eq!(duty.unit().symbol(), "W");
        profile.validate().expect("baseline is consistent");
    }

    #[test]
    fn profiles_follow_engine_changes() {
        let mut engine = Engine::with_baseline();
        engine
            .set("sensor.return.temperature", "300 K")
            .expect("in range");
        let profile = ThermalProfile::from_engine(&engine).expect("profile");
        assert_eq!(profile.drop().expect("compatible").canonical(), 50.0);
        let duty = profile.duty().expect("dimensions multiply");
        assert!((duty.canonical() - 522_203.5).abs() < 1e-6);
        engine
            .set_from("site.name", "bay-7", macrokit::Source::File)
            .expect("in range");
        let profile = ThermalProfile::from_engine(&engine).expect("profile");
        assert_eq!(profile.name(), "bay-7");
        assert_eq!(profile.revision(), engine.revision());
    }

    #[test]
    fn inconsistent_profiles_are_rejected() {
        let mut engine = Engine::with_baseline();
        engine
            .set("sensor.supply.temperature", "300 K")
            .expect("in range");
        let profile = ThermalProfile::from_engine(&engine).expect("profile");
        let error = profile.validate().expect_err("drop is zero");
        assert!(matches!(error, StationError::Inconsistent { .. }));
        assert!(
            error
                .to_string()
                .contains("is not above the return temperature"),
            "unexpected message: {error}"
        );

        engine
            .set("sensor.supply.temperature", "360 K")
            .expect("in range");
        engine.set("power.limit", "1000 W").expect("in range");
        let profile = ThermalProfile::from_engine(&engine).expect("profile");
        let error = profile.validate().expect_err("duty is too high");
        assert!(error.to_string().contains("exceeds the power budget"));
    }

    #[test]
    fn dimensions_are_part_of_the_profile() {
        assert_eq!(ThermalProfile::REQUIRED_KEYS.len(), 6);
        assert_eq!(ThermalProfile::REQUIRED_KEYS[0], "fluid.density");
        let engine = Engine::with_baseline();
        let profile = ThermalProfile::from_engine(&engine).expect("profile");
        assert_eq!(profile.density().dimension(), Dimension::DENSITY);
        assert_eq!(profile.flow().dimension(), Dimension::FLOW);
        assert_eq!(profile.supply().dimension(), Dimension::TEMPERATURE);
    }

    #[test]
    fn products_check_their_dimension() {
        let engine = Engine::with_baseline();
        let profile = ThermalProfile::from_engine(&engine).expect("profile");
        let error = product(
            profile.density(),
            profile.supply(),
            super::MASS_FLOW,
            super::KILOGRAM_PER_SECOND,
            "density times temperature",
        )
        .expect_err("dimensions do not multiply");
        assert!(error.to_string().contains("density times temperature"));
        let energy = product(
            Quantity::from_canonical(2.0, Unit::new("N", Dimension::FORCE, 1.0, 0.0)),
            Quantity::from_canonical(3.0, Unit::new("m", Dimension::LENGTH, 1.0, 0.0)),
            Dimension::ENERGY,
            Unit::new("J", Dimension::ENERGY, 1.0, 0.0),
            "force times length",
        )
        .expect("force times length is energy");
        assert_eq!(energy.canonical(), 6.0);
    }

    #[test]
    fn the_profile_describes_itself() {
        let engine = Engine::with_baseline();
        let profile = ThermalProfile::from_engine(&engine).expect("profile");
        assert_eq!(ThermalProfile::NAME, "thermal profile");
        assert_eq!(
            profile.field_names(),
            &[
                "name",
                "supply",
                "return_temperature",
                "flow",
                "density",
                "heat_capacity",
                "duty_limit"
            ]
        );
        let lines = profile.describe_lines();
        assert_eq!(lines[0], "thermal profile: bay-1");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("supply = Quantity"))
        );
        assert!(!lines.iter().any(|line| line.starts_with("revision")));
        let text = profile.describe().to_string();
        assert!(text.contains("duty_limit="));
        assert!(!text.contains("revision"));
    }

    #[test]
    fn building_the_station_checks_the_schema() {
        let station = Station::new().expect("station builds");
        station.check_schema().expect("baseline schema is complete");
    }
}
