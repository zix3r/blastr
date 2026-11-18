//! Unit-aware quantities.
//!
//! A [`Quantity`] stores its value in the canonical unit of its dimension and
//! remembers the unit it was written in, so `350 K` and `76.85 degC` are the
//! same quantity with different preferred spellings.

use std::cmp::Ordering;
use std::fmt::{self, Display, Formatter};

use crate::dimension::Dimension;
use crate::error::UnitError;
use crate::unit::{self, Unit};
use crate::util;

/// The preferred display unit for temperatures, selected by the `imperial`
/// feature.
#[cfg(feature = "imperial")]
pub const PREFERRED_TEMPERATURE: &str = "degF";
/// The preferred display unit for temperatures, selected by the `imperial`
/// feature.
#[cfg(not(feature = "imperial"))]
pub const PREFERRED_TEMPERATURE: &str = "degC";

/// A physical value with a unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantity {
    canonical: f64,
    unit: Unit,
}

impl Quantity {
    /// Builds a quantity from a value written in `unit`.
    pub fn new(value: f64, unit: Unit) -> Self {
        Self {
            canonical: unit.to_canonical(value),
            unit,
        }
    }

    /// Builds a quantity from a value already expressed in the canonical unit.
    pub fn from_canonical(canonical: f64, unit: Unit) -> Self {
        Self { canonical, unit }
    }

    /// A dimensionless value.
    pub fn dimensionless(value: f64) -> Self {
        Self {
            canonical: value,
            unit: Unit::ratio(),
        }
    }

    /// Parses text such as `0.0025 m3/s` or `-12,5`-free `2.5 kPa`.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError`] when the number or the unit cannot be parsed.
    pub fn parse(text: &str) -> Result<Self, UnitError> {
        let trimmed = text.trim();
        let (number, symbol) = split_number(trimmed)?;
        let value: f64 = number.parse().map_err(|_| UnitError::MalformedNumber {
            input: number.to_string(),
            reason: "not a decimal number".to_string(),
        })?;
        if !value.is_finite() {
            return Err(UnitError::NonFinite {
                input: trimmed.to_string(),
            });
        }
        if symbol.is_empty() {
            return Err(UnitError::MissingUnit {
                input: trimmed.to_string(),
            });
        }
        let unit = unit::resolve(symbol)?;
        Ok(Self::new(value, unit))
    }

    /// The value in the canonical unit of the dimension.
    pub fn canonical(&self) -> f64 {
        self.canonical
    }

    /// The value in the preferred unit of this quantity.
    pub fn value(&self) -> f64 {
        self.unit.from_canonical(self.canonical)
    }

    /// The preferred unit of this quantity.
    pub fn unit(&self) -> Unit {
        self.unit
    }

    /// The dimension of this quantity.
    pub fn dimension(&self) -> Dimension {
        self.unit.dimension()
    }

    /// The value expressed in `unit`.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError::DimensionMismatch`] when the dimensions differ.
    pub fn value_in(&self, unit: &Unit) -> Result<f64, UnitError> {
        if !self.unit.compatible_with(unit) {
            return Err(UnitError::DimensionMismatch {
                expected: self.dimension().describe(),
                found: unit.dimension().describe(),
            });
        }
        Ok(unit.from_canonical(self.canonical))
    }

    /// The same quantity with a different preferred unit.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError::DimensionMismatch`] when the dimensions differ.
    pub fn to(&self, unit: Unit) -> Result<Self, UnitError> {
        if !self.unit.compatible_with(&unit) {
            return Err(UnitError::DimensionMismatch {
                expected: self.dimension().describe(),
                found: unit.dimension().describe(),
            });
        }
        Ok(Self {
            canonical: self.canonical,
            unit,
        })
    }

    /// Switches the preferred unit in place.
    ///
    /// # Errors
    ///
    /// See [`Quantity::to`].
    pub fn convert(&mut self, unit: Unit) -> Result<(), UnitError> {
        *self = self.to(unit)?;
        Ok(())
    }

    /// Whether the canonical value is finite.
    pub fn is_finite(&self) -> bool {
        self.canonical.is_finite()
    }

    /// Whether the canonical value is within `epsilon` of zero.
    pub fn is_zero(&self, epsilon: f64) -> bool {
        self.canonical.abs() <= epsilon
    }

    /// The absolute value.
    pub fn abs(&self) -> Self {
        Self {
            canonical: self.canonical.abs(),
            unit: self.unit,
        }
    }

    /// Multiplies the canonical value by `factor`, keeping the unit.
    pub fn scale_by(&self, factor: f64) -> Self {
        Self {
            canonical: self.canonical * factor,
            unit: self.unit,
        }
    }

    /// Adds two quantities of the same dimension.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError::DimensionMismatch`] when the dimensions differ.
    pub fn add(&self, other: &Self) -> Result<Self, UnitError> {
        self.combine(other, 1.0)
    }

    /// Subtracts two quantities of the same dimension.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError::DimensionMismatch`] when the dimensions differ.
    pub fn sub(&self, other: &Self) -> Result<Self, UnitError> {
        self.combine(other, -1.0)
    }

    fn combine(&self, other: &Self, sign: f64) -> Result<Self, UnitError> {
        if self.dimension() != other.dimension() {
            return Err(UnitError::DimensionMismatch {
                expected: self.dimension().describe(),
                found: other.dimension().describe(),
            });
        }
        Ok(Self {
            canonical: self.canonical + sign * other.canonical,
            unit: self.unit,
        })
    }

    /// The dimensionless ratio `self / other`.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError::DimensionMismatch`] for different dimensions and
    /// [`UnitError::DivisionByZero`] for a zero divisor.
    pub fn ratio(&self, other: &Self) -> Result<f64, UnitError> {
        if self.dimension() != other.dimension() {
            return Err(UnitError::DimensionMismatch {
                expected: self.dimension().describe(),
                found: other.dimension().describe(),
            });
        }
        if other.canonical == 0.0 {
            return Err(UnitError::DivisionByZero);
        }
        Ok(self.canonical / other.canonical)
    }

    /// Linear interpolation towards `other` with factor `t`.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError::DimensionMismatch`] when the dimensions differ.
    pub fn lerp(&self, other: &Self, t: f64) -> Result<Self, UnitError> {
        if self.dimension() != other.dimension() {
            return Err(UnitError::DimensionMismatch {
                expected: self.dimension().describe(),
                found: other.dimension().describe(),
            });
        }
        Ok(Self {
            canonical: self.canonical + (other.canonical - self.canonical) * t,
            unit: self.unit,
        })
    }

    /// Compares two quantities of the same dimension.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError::DimensionMismatch`] when the dimensions differ.
    pub fn compare(&self, other: &Self) -> Result<Ordering, UnitError> {
        if self.dimension() != other.dimension() {
            return Err(UnitError::DimensionMismatch {
                expected: self.dimension().describe(),
                found: other.dimension().describe(),
            });
        }
        Ok(self
            .canonical
            .partial_cmp(&other.canonical)
            .unwrap_or(Ordering::Equal))
    }

    /// Whether both quantities agree within `tolerance` canonical units.
    pub fn approx_eq(&self, other: &Self, tolerance: f64) -> bool {
        self.dimension() == other.dimension()
            && (self.canonical - other.canonical).abs() <= tolerance
    }

    /// Rounds the canonical value to `digits` decimal places.
    pub fn round_to(&self, digits: u32) -> Self {
        let factor = 10f64.powi(i32::try_from(digits).unwrap_or(0));
        Self {
            canonical: (self.canonical * factor).round() / factor,
            unit: self.unit,
        }
    }

    /// Renders the quantity in the preferred temperature unit when it measures
    /// a temperature, in its own unit otherwise.
    pub fn humanize(&self) -> String {
        if self.dimension() == Dimension::TEMPERATURE
            && let Ok(unit) = unit::resolve(PREFERRED_TEMPERATURE)
            && let Ok(value) = self.value_in(&unit)
        {
            return format!("{} {}", util::format_float(value), unit.symbol());
        }
        self.to_string()
    }

    /// A one line description including the dimension.
    pub fn describe(&self) -> String {
        format!("{} [{}]", self.humanize(), self.dimension().describe())
    }
}

impl Display for Quantity {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {}",
            util::format_float(self.value()),
            self.unit.symbol()
        )
    }
}

/// Splits `2.5 kPa` (or `1e-3m3/s`) into number text and unit text.
fn split_number(text: &str) -> Result<(&str, &str), UnitError> {
    let bytes = text.as_bytes();
    let mut index = 0;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        index += 1;
    }
    let mut digits = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
        digits += 1;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return Err(UnitError::MalformedNumber {
            input: text.to_string(),
            reason: "expected a number at the start of the value".to_string(),
        });
    }
    if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
        let before_exponent = index;
        index += 1;
        if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
            index += 1;
        }
        let exponent_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if exponent_start == index {
            index = before_exponent;
        }
    }
    let number = &text[..index];
    Ok((number, text[index..].trim()))
}

#[cfg(test)]
mod tests {
    use super::Quantity;
    use crate::dimension::Dimension;
    use crate::error::UnitError;
    use crate::unit::lookup;

    fn metre(value: f64) -> Quantity {
        Quantity::new(value, lookup("m").expect("m is registered"))
    }

    fn second(value: f64) -> Quantity {
        Quantity::new(value, lookup("s").expect("s is registered"))
    }

    #[test]
    fn parse_reads_value_and_symbol() {
        let flow = Quantity::parse("0.0025 m3/s").expect("flow parses");
        assert_eq!(flow.canonical(), 0.0025);
        assert_eq!(flow.unit().symbol(), "m3/s");
        assert_eq!(flow.dimension(), Dimension::FLOW);
        let compact = Quantity::parse("12.5kPa").expect("compact form parses");
        assert_eq!(compact.canonical(), 12_500.0);
        let shifted = Quantity::parse("-2.5 degC").expect("negative parses");
        assert_eq!(shifted.canonical(), 270.65);
        let exponent = Quantity::parse("1e-3 l").expect("exponent parses");
        assert_eq!(exponent.canonical(), 0.000_001);
    }

    #[test]
    fn parse_reports_bad_input() {
        assert_eq!(
            Quantity::parse("abc"),
            Err(UnitError::MalformedNumber {
                input: "abc".to_string(),
                reason: "expected a number at the start of the value".to_string(),
            })
        );
        assert_eq!(
            Quantity::parse("12"),
            Err(UnitError::MissingUnit {
                input: "12".to_string()
            })
        );
        assert_eq!(
            Quantity::parse("1 xyzzy"),
            Err(UnitError::UnknownUnit {
                symbol: "xyzzy".to_string(),
                suggestion: None,
            })
        );
        assert!(Quantity::parse(".5 m").is_ok());
        assert!(Quantity::parse("-.5 m").is_ok());
    }

    #[test]
    fn conversion_keeps_the_canonical_value() {
        let temperature = Quantity::parse("350 K").expect("temperature parses");
        let celsius = lookup("degC").expect("degC is registered");
        assert!((temperature.value_in(&celsius).unwrap() - 76.85).abs() < 1e-9);
        let converted = temperature.to(celsius).expect("compatible");
        assert_eq!(converted.canonical(), 350.0);
        assert_eq!(converted.unit().symbol(), "degC");
        assert_eq!(
            temperature.to(lookup("s").expect("s is registered")),
            Err(UnitError::DimensionMismatch {
                expected: "TEMP1".to_string(),
                found: "TIME1".to_string(),
            })
        );
    }

    #[test]
    fn arithmetic_checks_dimensions() {
        let total = metre(1.5).add(&metre(2.0)).expect("same dimension");
        assert_eq!(total.value(), 3.5);
        let difference = metre(3.0).sub(&metre(0.5)).expect("same dimension");
        assert_eq!(difference.value(), 2.5);
        assert_eq!(
            metre(1.0).add(&second(1.0)),
            Err(UnitError::DimensionMismatch {
                expected: "LEN1".to_string(),
                found: "TIME1".to_string(),
            })
        );
        assert_eq!(second(2.0).ratio(&second(4.0)).unwrap(), 0.5);
        assert_eq!(
            second(2.0).ratio(&second(0.0)),
            Err(UnitError::DivisionByZero)
        );
        let middle = metre(0.0).lerp(&metre(10.0), 0.25).expect("compatible");
        assert_eq!(middle.value(), 2.5);
        assert_eq!(metre(-2.0).abs().value(), 2.0);
        assert_eq!(metre(2.0).scale_by(3.0).value(), 6.0);
        assert!(metre(1.0).is_finite());
        assert!(metre(0.0001).is_zero(0.001));
    }

    #[test]
    fn comparison_and_rounding() {
        assert_eq!(
            metre(2.0).compare(&metre(1.0)).unwrap(),
            std::cmp::Ordering::Greater
        );
        assert!(metre(1.0).compare(&second(1.0)).is_err());
        assert!(metre(1.0).approx_eq(&metre(1.000_000_1), 1e-6));
        assert!(!metre(1.0).approx_eq(&metre(1.1), 1e-6));
        assert_eq!(metre(1.234_567).round_to(2).canonical(), 1.23);
        assert_eq!(metre(1.0).round_to(0).canonical(), 1.0);
    }

    #[test]
    fn rendering_uses_the_preferred_unit() {
        assert_eq!(metre(1.5).to_string(), "1.5 m");
        assert_eq!(metre(12_500.0).to_string(), "12500 m");
        let kelvin = Quantity::parse("350 K").expect("parses");
        #[cfg(not(feature = "imperial"))]
        assert_eq!(kelvin.humanize(), "76.85 degC");
        #[cfg(feature = "imperial")]
        assert_eq!(kelvin.humanize(), "170.33 degF");
        assert_eq!(metre(1.5).humanize(), "1.5 m");
        assert_eq!(metre(1.5).describe(), "1.5 m [LEN1]");
    }

    #[test]
    fn dimensionless_quantities_are_ratios() {
        let ratio = Quantity::dimensionless(0.5);
        assert!(ratio.dimension().is_dimensionless());
        assert_eq!(ratio.to_string(), "0.5 1");
        assert_eq!(ratio.canonical(), 0.5);
        assert_eq!(Quantity::from_canonical(2.0, ratio.unit()).value(), 2.0);
    }
}
