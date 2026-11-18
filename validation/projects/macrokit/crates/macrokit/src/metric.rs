//! Metric helpers, compiled with the default `metric` feature.
//!
//! The prefix ladder comes straight from the generated prefix table, so the
//! names and the exponents can never drift apart.

use crate::error::UnitError;
use crate::generated::{GENERATED_PREFIX_COUNT, PREFIX_TABLE};
use crate::quantity::PREFERRED_TEMPERATURE;
use crate::unit::PrefixSeed;
use crate::unit::Unit;

/// The SI decimal prefixes, longest symbol first.
pub fn prefixes() -> &'static [PrefixSeed] {
    PREFIX_TABLE
}

/// Number of prefixes in the generated table.
pub fn prefix_count() -> usize {
    GENERATED_PREFIX_COUNT
}

/// The name of a power of ten, `unknown` for exponents that are not a named
/// decimal prefix.
pub fn scale_name(power: i32) -> &'static str {
    match power {
        -24 => "yocto",
        -21 => "zepto",
        -18 => "atto",
        -15 => "femto",
        -12 => "pico",
        -9 => "nano",
        -6 => "micro",
        -3 => "milli",
        -2 => "centi",
        -1 => "deci",
        0 => "unit",
        1 => "deca",
        2 => "hecto",
        3 => "kilo",
        6 => "mega",
        9 => "giga",
        12 => "tera",
        15 => "peta",
        18 => "exa",
        _ => "unknown",
    }
}

/// Applies a named decimal prefix to a unit.
///
/// # Errors
///
/// Returns [`UnitError::UnknownUnit`] for unknown prefixes and
/// [`UnitError::OffsetUnit`] for affine units.
pub fn scaled(unit: Unit, prefix: &str) -> Result<Unit, UnitError> {
    let power = PREFIX_TABLE
        .iter()
        .find(|(symbol, _)| *symbol == prefix)
        .map(|(_, power)| *power)
        .ok_or_else(|| UnitError::UnknownUnit {
            symbol: prefix.to_string(),
            suggestion: None,
        })?;
    unit.scaled(10f64.powi(power))
}

/// The metric temperature unit used for display.
pub fn preferred_temperature_symbol() -> &'static str {
    PREFERRED_TEMPERATURE
}

/// Renders a unit with its prefix and scale, for `units` output.
pub fn describe(unit: Unit) -> String {
    format!(
        "{} = {} * {}",
        unit.symbol(),
        crate::util::format_float(unit.scale()),
        unit.formula()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        describe, preferred_temperature_symbol, prefix_count, prefixes, scale_name, scaled,
    };
    use crate::unit::{lookup, resolve};

    #[test]
    fn the_prefix_ladder_is_generated() {
        assert!(prefix_count() >= 15);
        assert_eq!(prefixes().len(), prefix_count());
        let kilo = prefixes()
            .iter()
            .find(|(symbol, _)| *symbol == "k")
            .expect("kilo is registered");
        assert_eq!(kilo.1, 3);
        for pair in prefixes().windows(2) {
            assert!(pair[0].0.len() >= pair[1].0.len());
        }
    }

    #[test]
    fn scale_names_are_known_for_decimal_prefixes() {
        assert_eq!(scale_name(3), "kilo");
        assert_eq!(scale_name(-6), "micro");
        assert_eq!(scale_name(0), "unit");
        assert_eq!(scale_name(4), "unknown");
    }

    #[test]
    fn prefixes_rescale_base_units() {
        let pascal = lookup("Pa").expect("Pa is registered");
        let megapascal = scaled(pascal, "M").expect("M is a prefix");
        assert_eq!(megapascal.scale(), 1.0e6);
        let millisecond =
            scaled(resolve("s").expect("s is registered"), "m").expect("m is a prefix");
        assert_eq!(millisecond.scale(), 0.001);
        assert!(scaled(pascal, "q").is_err());
        assert!(
            scaled(lookup("degC").expect("degC is registered"), "k").is_err(),
            "affine units cannot be rescaled"
        );
    }

    #[test]
    fn descriptions_include_the_formula() {
        let pascal = lookup("kPa").expect("kPa is registered");
        assert_eq!(describe(pascal), "kPa = 1000 * canonical = value * 1000");
        #[cfg(not(feature = "imperial"))]
        assert_eq!(preferred_temperature_symbol(), "degC");
        #[cfg(feature = "imperial")]
        assert_eq!(preferred_temperature_symbol(), "degF");
    }
}
