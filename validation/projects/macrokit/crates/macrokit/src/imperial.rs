//! Imperial helpers, compiled only with the optional `imperial` feature.
//!
//! The feature changes [`PREFERRED_TEMPERATURE`](crate::quantity::PREFERRED_TEMPERATURE)
//! to Fahrenheit, so `describe` output and quantity rendering switch to
//! imperial spellings, and it adds the conversion helpers below.

use crate::error::UnitError;
use crate::quantity::{PREFERRED_TEMPERATURE, Quantity};
use crate::unit;

/// One inch in metres.
pub const INCH_IN_METRES: f64 = 0.0254;
/// One pound in kilograms.
pub const POUND_IN_KILOGRAMS: f64 = 0.453_592_37;
/// One US gallon in cubic metres.
pub const GALLON_IN_CUBIC_METRES: f64 = 0.003_785_411_784;
/// One pound per square inch in pascals.
pub const PSI_IN_PASCALS: f64 = 6_894.757_293_168_361;

/// A temperature written in degrees Fahrenheit.
///
/// # Errors
///
/// Returns [`UnitError`] when `degF` is missing from the registry.
pub fn fahrenheit(value: f64) -> Result<Quantity, UnitError> {
    Ok(Quantity::new(value, unit::resolve("degF")?))
}

/// A pressure written in pounds per square inch.
///
/// # Errors
///
/// Returns [`UnitError`] when `psi` is missing from the registry.
pub fn psi(value: f64) -> Result<Quantity, UnitError> {
    Ok(Quantity::new(value, unit::resolve("psi")?))
}

/// A volume written in US gallons.
///
/// # Errors
///
/// Returns [`UnitError`] when `gal` is missing from the registry.
pub fn gallon(value: f64) -> Result<Quantity, UnitError> {
    Ok(Quantity::new(value, unit::resolve("gal")?))
}

/// The temperature of `quantity` in degrees Fahrenheit.
///
/// # Errors
///
/// Returns [`UnitError::DimensionMismatch`] when the quantity is not a
/// temperature.
pub fn in_fahrenheit(quantity: Quantity) -> Result<f64, UnitError> {
    quantity.value_in(&unit::resolve("degF")?)
}

/// The pressure of `quantity` in psi.
///
/// # Errors
///
/// Returns [`UnitError::DimensionMismatch`] when the quantity is not a
/// pressure.
pub fn in_psi(quantity: Quantity) -> Result<f64, UnitError> {
    quantity.value_in(&unit::resolve("psi")?)
}

/// A pressure rendered in psi and, when it is a pressure, in the metric unit
/// as well.
pub fn describe_pressure(quantity: Quantity) -> Result<String, UnitError> {
    let psi = in_psi(quantity)?;
    Ok(format!(
        "{} psi ({})",
        crate::util::format_float(psi),
        quantity
    ))
}

/// The imperial temperature unit used for display.
pub fn preferred_temperature_symbol() -> &'static str {
    PREFERRED_TEMPERATURE
}

#[cfg(test)]
mod tests {
    use super::{
        GALLON_IN_CUBIC_METRES, INCH_IN_METRES, POUND_IN_KILOGRAMS, PSI_IN_PASCALS,
        describe_pressure, fahrenheit, gallon, in_fahrenheit, in_psi, preferred_temperature_symbol,
        psi,
    };
    use crate::unit::lookup;

    #[test]
    fn constants_match_the_unit_table() {
        assert!((lookup("in").expect("in").scale() - INCH_IN_METRES).abs() < f64::EPSILON);
        assert!((lookup("lb").expect("lb").scale() - POUND_IN_KILOGRAMS).abs() < f64::EPSILON);
        assert!(
            (lookup("gal").expect("gal").scale() - GALLON_IN_CUBIC_METRES).abs() < f64::EPSILON
        );
        assert!((lookup("psi").expect("psi").scale() - PSI_IN_PASCALS).abs() < 1e-9);
    }

    #[test]
    fn fahrenheit_converts_to_kelvin() {
        let boiling = fahrenheit(212.0).expect("degF is registered");
        assert!((boiling.canonical() - 373.15).abs() < 1e-6);
        assert!((in_fahrenheit(boiling).expect("temperature") - 212.0).abs() < 1e-9);
        assert_eq!(preferred_temperature_symbol(), "degF");
    }

    #[test]
    fn pressure_and_volume_helpers() {
        let atmosphere = psi(14.695_948).expect("psi is registered");
        assert!((atmosphere.canonical() - 101_325.0).abs() < 1.0);
        let rendered = describe_pressure(atmosphere).expect("pressure");
        assert!(rendered.starts_with("14.695948 psi ("));
        assert!((in_psi(atmosphere).expect("pressure") - 14.695_948).abs() < 1e-6);
        let bucket = gallon(2.0).expect("gal is registered");
        assert!((bucket.canonical() - 0.007_570_823_568).abs() < 1e-12);
        assert!(in_psi(crate::quantity::Quantity::dimensionless(1.0)).is_err());
    }
}
