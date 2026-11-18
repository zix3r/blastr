//! Physical dimensions as seven signed exponents.
//!
//! The exponent vector is stored in the order used by the generated tables:
//! length, mass, time, electric current, temperature, amount of substance,
//! luminous intensity.

use std::fmt::{self, Display, Formatter};

use crate::error::UnitError;

/// Exponent vector of a [`Dimension`].
pub type Dim = [i8; DIMENSION_COUNT];

/// Number of base dimensions macrokit tracks.
pub const DIMENSION_COUNT: usize = 7;

/// Index of length in a [`Dim`].
pub const LEN: usize = 0;
/// Index of mass in a [`Dim`].
pub const MASS: usize = 1;
/// Index of time in a [`Dim`].
pub const TIME: usize = 2;
/// Index of electric current in a [`Dim`].
pub const CURR: usize = 3;
/// Index of thermodynamic temperature in a [`Dim`].
pub const TEMP: usize = 4;
/// Index of amount of substance in a [`Dim`].
pub const AMT: usize = 5;
/// Index of luminous intensity in a [`Dim`].
pub const LUM: usize = 6;

/// Dimension codes in table order; also the names used in data files.
const CODES: [&str; DIMENSION_COUNT] = ["LEN", "MASS", "TIME", "CURR", "TEMP", "AMT", "LUM"];

/// A physical dimension expressed as exponents of the base dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Dimension {
    exponents: Dim,
}

impl Dimension {
    /// The dimensionless dimension.
    pub const NONE: Dimension = Dimension::from_exponents([0; DIMENSION_COUNT]);
    /// Length.
    pub const LENGTH: Dimension = Dimension::from_exponents([1, 0, 0, 0, 0, 0, 0]);
    /// Mass.
    pub const MASS: Dimension = Dimension::from_exponents([0, 1, 0, 0, 0, 0, 0]);
    /// Time.
    pub const TIME: Dimension = Dimension::from_exponents([0, 0, 1, 0, 0, 0, 0]);
    /// Electric current.
    pub const CURRENT: Dimension = Dimension::from_exponents([0, 0, 0, 1, 0, 0, 0]);
    /// Thermodynamic temperature.
    pub const TEMPERATURE: Dimension = Dimension::from_exponents([0, 0, 0, 0, 1, 0, 0]);
    /// Amount of substance.
    pub const AMOUNT: Dimension = Dimension::from_exponents([0, 0, 0, 0, 0, 1, 0]);
    /// Luminous intensity.
    pub const LUMINOUS: Dimension = Dimension::from_exponents([0, 0, 0, 0, 0, 0, 1]);
    /// Area, `LEN2`.
    pub const AREA: Dimension = Dimension::from_exponents([2, 0, 0, 0, 0, 0, 0]);
    /// Volume, `LEN3`.
    pub const VOLUME: Dimension = Dimension::from_exponents([3, 0, 0, 0, 0, 0, 0]);
    /// Velocity, `LEN1 TIME-1`.
    pub const VELOCITY: Dimension = Dimension::from_exponents([1, 0, -1, 0, 0, 0, 0]);
    /// Momentum, `MASS1 LEN1 TIME-1`.
    pub const MOMENTUM: Dimension = Dimension::from_exponents([1, 1, -1, 0, 0, 0, 0]);
    /// Acceleration, `LEN1 TIME-2`.
    pub const ACCELERATION: Dimension = Dimension::from_exponents([1, 0, -2, 0, 0, 0, 0]);
    /// Force, `MASS1 LEN1 TIME-2`.
    pub const FORCE: Dimension = Dimension::from_exponents([1, 1, -2, 0, 0, 0, 0]);
    /// Pressure, `MASS1 LEN-1 TIME-2`.
    pub const PRESSURE: Dimension = Dimension::from_exponents([-1, 1, -2, 0, 0, 0, 0]);
    /// Energy, `MASS1 LEN2 TIME-2`.
    pub const ENERGY: Dimension = Dimension::from_exponents([2, 1, -2, 0, 0, 0, 0]);
    /// Power, `MASS1 LEN2 TIME-3`.
    pub const POWER: Dimension = Dimension::from_exponents([2, 1, -3, 0, 0, 0, 0]);
    /// Volumetric flow, `LEN3 TIME-1`.
    pub const FLOW: Dimension = Dimension::from_exponents([3, 0, -1, 0, 0, 0, 0]);
    /// Density, `MASS1 LEN-3`.
    pub const DENSITY: Dimension = Dimension::from_exponents([-3, 1, 0, 0, 0, 0, 0]);
    /// Specific heat capacity, `LEN2 TIME-2 TEMP-1`.
    pub const SPECIFIC_HEAT: Dimension = Dimension::from_exponents([2, 0, -2, 0, -1, 0, 0]);
    /// Frequency, `TIME-1`.
    pub const FREQUENCY: Dimension = Dimension::from_exponents([0, 0, -1, 0, 0, 0, 0]);

    /// Builds a dimension from an explicit exponent vector.
    pub const fn from_exponents(exponents: Dim) -> Self {
        Self { exponents }
    }

    /// Builds a dimension with a single non-zero exponent.
    pub const fn base(index: usize, exponent: i8) -> Self {
        let mut exponents = [0; DIMENSION_COUNT];
        exponents[index] = exponent;
        Self { exponents }
    }

    /// The raw exponent vector.
    pub const fn exponents(&self) -> Dim {
        self.exponents
    }

    /// Exponent of one base dimension.
    pub const fn exponent(&self, index: usize) -> i8 {
        self.exponents[index]
    }

    /// Whether every exponent is zero.
    pub fn is_dimensionless(&self) -> bool {
        self.exponents.iter().all(|exponent| *exponent == 0)
    }

    /// Entrywise negation.
    #[must_use]
    pub fn inverse(self) -> Self {
        let mut exponents = [0; DIMENSION_COUNT];
        for (index, exponent) in self.exponents.iter().enumerate() {
            exponents[index] = exponent.saturating_neg();
        }
        Self { exponents }
    }

    /// Raises every exponent to `exponent`.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError::DimensionOverflow`] when an exponent does not fit
    /// into an `i8`.
    pub fn pow(self, exponent: i8) -> Result<Self, UnitError> {
        let mut exponents = [0; DIMENSION_COUNT];
        for (index, current) in self.exponents.iter().enumerate() {
            let value = i32::from(*current) * i32::from(exponent);
            let narrowed = i8::try_from(value)
                .map_err(|_| UnitError::DimensionOverflow { exponent: value })?;
            exponents[index] = narrowed;
        }
        Ok(Self { exponents })
    }

    fn combine(self, other: Self, sign: i8) -> Self {
        let mut exponents = [0; DIMENSION_COUNT];
        for (index, exponent) in exponents.iter_mut().enumerate() {
            let value = i16::from(self.exponents[index])
                + i16::from(other.exponents[index]) * i16::from(sign);
            *exponent = value.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8;
        }
        Self { exponents }
    }

    /// Parses a `CODE<exponent>` list such as `MASS1 LEN-1 TIME-2`.
    ///
    /// An empty specification, `1` or `none` yields the dimensionless
    /// dimension.
    ///
    /// # Errors
    ///
    /// Returns [`UnitError::MalformedDimension`] for unknown codes, missing or
    /// duplicated exponents and malformed numbers.
    pub fn parse(text: &str) -> Result<Self, UnitError> {
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed == "1" || trimmed.eq_ignore_ascii_case("none") {
            return Ok(Self::NONE);
        }
        let mut exponents = [0; DIMENSION_COUNT];
        let mut seen = [false; DIMENSION_COUNT];
        for token in trimmed.split_whitespace() {
            let split = token
                .find(|character: char| {
                    character.is_ascii_digit() || character == '-' || character == '+'
                })
                .ok_or_else(|| UnitError::MalformedDimension {
                    text: token.to_string(),
                })?;
            let code = &token[..split];
            let index = CODES
                .iter()
                .position(|candidate| *candidate == code)
                .ok_or_else(|| UnitError::MalformedDimension {
                    text: token.to_string(),
                })?;
            if seen[index] {
                return Err(UnitError::MalformedDimension {
                    text: token.to_string(),
                });
            }
            let exponent: i8 =
                token[split..]
                    .parse()
                    .map_err(|_| UnitError::MalformedDimension {
                        text: token.to_string(),
                    })?;
            seen[index] = true;
            exponents[index] = exponent;
        }
        Ok(Self { exponents })
    }

    /// Human readable exponent list, `1` when dimensionless.
    pub fn describe(&self) -> String {
        if self.is_dimensionless() {
            return "1".to_string();
        }
        let mut parts = Vec::new();
        for (index, exponent) in self.exponents.iter().enumerate() {
            if *exponent != 0 {
                parts.push(format!("{}{}", CODES[index], exponent));
            }
        }
        parts.join(" ")
    }
}

impl std::ops::Mul for Dimension {
    type Output = Self;

    /// Combines two dimensions by adding their exponents.
    fn mul(self, other: Self) -> Self {
        self.combine(other, 1)
    }
}

impl std::ops::Div for Dimension {
    type Output = Self;

    /// Combines two dimensions by subtracting their exponents.
    fn div(self, other: Self) -> Self {
        self.combine(other, -1)
    }
}

impl Display for Dimension {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.describe())
    }
}

#[cfg(test)]
mod tests {
    use super::{Dimension, LEN, MASS, TEMP, TIME};
    use crate::error::UnitError;

    #[test]
    fn base_dimensions_have_one_exponent() {
        assert_eq!(Dimension::LENGTH.exponent(LEN), 1);
        assert_eq!(Dimension::LENGTH.exponent(MASS), 0);
        assert_eq!(Dimension::base(TEMP, 2).exponent(TEMP), 2);
        assert!(Dimension::NONE.is_dimensionless());
        assert!(!Dimension::TIME.is_dimensionless());
    }

    #[test]
    fn derived_dimensions_match_their_definition() {
        assert_eq!(Dimension::LENGTH / Dimension::TIME, Dimension::VELOCITY);
        assert_eq!(
            Dimension::MASS / Dimension::LENGTH / Dimension::TIME.pow(2).unwrap(),
            Dimension::PRESSURE
        );
        assert_eq!(Dimension::VOLUME / Dimension::TIME, Dimension::FLOW);
        assert_eq!(Dimension::VELOCITY * Dimension::MASS, Dimension::MOMENTUM);
        assert_eq!(Dimension::ACCELERATION * Dimension::MASS, Dimension::FORCE);
    }

    #[test]
    fn inversion_negates_every_exponent() {
        let inverted = Dimension::VELOCITY.inverse();
        assert_eq!(inverted.exponent(LEN), -1);
        assert_eq!(inverted.exponent(TIME), 1);
        assert_eq!(inverted, Dimension::TIME / Dimension::LENGTH);
    }

    #[test]
    fn pow_reports_overflow() {
        let error = Dimension::ACCELERATION.pow(100).unwrap_err();
        assert_eq!(error, UnitError::DimensionOverflow { exponent: -200 });
        assert_eq!(
            Dimension::LENGTH.pow(-2).unwrap(),
            Dimension::AREA.inverse()
        );
    }

    #[test]
    fn parse_accepts_codes_and_rejects_nonsense() {
        assert_eq!(
            Dimension::parse("MASS1 LEN-1 TIME-2").unwrap(),
            Dimension::PRESSURE
        );
        assert_eq!(Dimension::parse("  ").unwrap(), Dimension::NONE);
        assert_eq!(Dimension::parse("none").unwrap(), Dimension::NONE);
        assert_eq!(
            Dimension::parse("BOGUS1"),
            Err(UnitError::MalformedDimension {
                text: "BOGUS1".to_string()
            })
        );
        assert_eq!(
            Dimension::parse("LEN1 LEN2"),
            Err(UnitError::MalformedDimension {
                text: "LEN2".to_string()
            })
        );
        assert_eq!(
            Dimension::parse("LEN"),
            Err(UnitError::MalformedDimension {
                text: "LEN".to_string()
            })
        );
    }

    #[test]
    fn describe_lists_non_zero_exponents() {
        assert_eq!(Dimension::PRESSURE.describe(), "LEN-1 MASS1 TIME-2");
        assert_eq!(Dimension::NONE.describe(), "1");
        assert_eq!(Dimension::FREQUENCY.to_string(), "TIME-1");
    }
}
