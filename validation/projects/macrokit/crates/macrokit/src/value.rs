//! Typed setting values and the inclusive range type they use.
//!
//! [`SettingValue`] is the union every setting parses to; [`Range`] is a small
//! generic helper that is also a good example of the `Describe` derive on
//! generic containers.

use std::fmt::{self, Display, Formatter};

use macrokit_derive::Describe;

use crate::quantity::Quantity;
use crate::util;

/// A typed configuration value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// A boolean flag.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// A decimal number.
    Float(f64),
    /// Free form text.
    Text(String),
    /// A physical quantity with a unit.
    Quantity(Quantity),
}

impl SettingValue {
    /// The name of the variant, used in error messages.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Text(_) => "text",
            Self::Quantity(_) => "quantity",
        }
    }

    /// The boolean, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// The integer, if this is one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// The number, widening integers to `f64`.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(*value),
            Self::Int(value) => Some(*value as f64),
            _ => None,
        }
    }

    /// The text, if this is one.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }

    /// The quantity, if this is one.
    pub fn as_quantity(&self) -> Option<Quantity> {
        match self {
            Self::Quantity(value) => Some(*value),
            _ => None,
        }
    }

    /// A canonical rendering used in tables and reports.
    pub fn render(&self) -> String {
        self.to_string()
    }

    /// Whether the value would enable a flag, for example in a rule.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Int(value) => *value != 0,
            Self::Float(value) => *value != 0.0,
            Self::Text(value) => !value.is_empty(),
            Self::Quantity(value) => !value.is_zero(0.0),
        }
    }
}

impl Display for SettingValue {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Int(value) => write!(formatter, "{value}"),
            Self::Float(value) => formatter.write_str(&util::format_float(*value)),
            Self::Text(value) => formatter.write_str(value),
            Self::Quantity(value) => Display::fmt(value, formatter),
        }
    }
}

impl From<bool> for SettingValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for SettingValue {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<f64> for SettingValue {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<&str> for SettingValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<String> for SettingValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Quantity> for SettingValue {
    fn from(value: Quantity) -> Self {
        Self::Quantity(value)
    }
}

/// An inclusive range of comparable values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord, Describe)]
#[describe(name = "inclusive range")]
pub struct Range<T> {
    min: T,
    max: T,
}

impl<T> Range<T> {
    /// Builds a range from its inclusive bounds.
    pub const fn new(min: T, max: T) -> Self {
        Self { min, max }
    }
}

impl<T: Copy> Range<T> {
    /// The lower bound.
    pub const fn lower(&self) -> T {
        self.min
    }

    /// The upper bound.
    pub const fn upper(&self) -> T {
        self.max
    }
}

impl<T: PartialOrd> Range<T> {
    /// Whether `value` lies inside the range.
    pub fn contains(&self, value: &T) -> bool {
        *value >= self.min && *value <= self.max
    }
}

impl<T: PartialOrd + Copy> Range<T> {
    /// Clamps `value` into the range.
    pub fn clamp_value(&self, value: T) -> T {
        if value < self.min {
            self.min
        } else if value > self.max {
            self.max
        } else {
            value
        }
    }
}

impl Range<f64> {
    /// Width of the range.
    pub fn span(&self) -> f64 {
        self.max - self.min
    }

    /// Midpoint of the range.
    pub fn mid(&self) -> f64 {
        self.min + (self.max - self.min) / 2.0
    }

    /// Whether both bounds are finite.
    pub fn is_finite(&self) -> bool {
        self.min.is_finite() && self.max.is_finite()
    }

    /// Renders the bounds as `min..max`.
    pub fn bounds_text(&self) -> String {
        format!(
            "{}..{}",
            util::format_float(self.min),
            util::format_float(self.max)
        )
    }
}

impl Range<i64> {
    /// Number of integers inside the range.
    pub fn count(&self) -> u64 {
        u64::try_from(self.max - self.min).unwrap_or(0) + 1
    }

    /// Renders the bounds as `min..max`.
    pub fn bounds_text(&self) -> String {
        format!("{}..{}", self.min, self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::{Range, SettingValue};
    use crate::quantity::Quantity;
    use crate::unit::lookup;

    #[test]
    fn values_report_their_kind() {
        assert_eq!(SettingValue::Bool(true).kind_name(), "bool");
        assert_eq!(SettingValue::Int(3).kind_name(), "int");
        assert_eq!(SettingValue::Float(0.5).kind_name(), "float");
        assert_eq!(SettingValue::from("text").kind_name(), "text");
        assert_eq!(
            SettingValue::from(Quantity::dimensionless(1.0)).kind_name(),
            "quantity"
        );
    }

    #[test]
    fn accessors_are_variant_specific() {
        let value = SettingValue::Int(7);
        assert_eq!(value.as_int(), Some(7));
        assert_eq!(value.as_float(), Some(7.0));
        assert_eq!(value.as_bool(), None);
        assert_eq!(value.as_text(), None);
        assert_eq!(value.as_quantity(), None);
        let bool = SettingValue::Bool(false);
        assert_eq!(bool.as_bool(), Some(false));
        assert_eq!(bool.as_float(), None);
        assert_eq!(SettingValue::Float(2.5).as_float(), Some(2.5));
        assert_eq!(SettingValue::from(String::from("x")).as_text(), Some("x"));
    }

    #[test]
    fn rendering_is_stable() {
        assert_eq!(SettingValue::Bool(true).render(), "true");
        assert_eq!(SettingValue::Int(-4).render(), "-4");
        assert_eq!(SettingValue::Float(1.25).render(), "1.25");
        assert_eq!(SettingValue::Float(1000.0).render(), "1000");
        assert_eq!(SettingValue::from("primary").render(), "primary");
        let quantity = Quantity::new(2.5, lookup("kPa").expect("kPa is registered"));
        assert_eq!(SettingValue::from(quantity).render(), "2.5 kPa");
    }

    #[test]
    fn truthiness_follows_the_variant() {
        assert!(SettingValue::Bool(true).is_truthy());
        assert!(!SettingValue::Int(0).is_truthy());
        assert!(SettingValue::Float(0.1).is_truthy());
        assert!(!SettingValue::from("").is_truthy());
        assert!(!SettingValue::from(Quantity::dimensionless(0.0)).is_truthy());
        assert!(SettingValue::from(Quantity::dimensionless(1.0)).is_truthy());
    }

    #[test]
    fn integer_ranges_count_their_members() {
        let range = Range::new(1, 64);
        assert_eq!(range.lower(), 1);
        assert_eq!(range.upper(), 64);
        assert!(range.contains(&1));
        assert!(range.contains(&64));
        assert!(!range.contains(&65));
        assert!(!range.contains(&0));
        assert_eq!(range.count(), 64);
        assert_eq!(range.clamp_value(0), 1);
        assert_eq!(range.clamp_value(99), 64);
        assert_eq!(range.clamp_value(7), 7);
        assert_eq!(range.bounds_text(), "1..64");
        assert_eq!(Range::<i64>::default(), Range::new(0, 0));
    }

    #[test]
    fn float_ranges_expose_geometry() {
        let range = Range::new(0.0, 2000.0);
        assert_eq!(range.span(), 2000.0);
        assert_eq!(range.mid(), 1000.0);
        assert!(range.is_finite());
        assert_eq!(range.bounds_text(), "0..2000");
        assert_eq!(Range::new(0.5, 1.25).bounds_text(), "0.5..1.25");
    }

    #[test]
    fn ranges_describe_their_bounds() {
        use crate::describe::Describe;
        let range = Range::new(2, 8);
        assert_eq!(range.describe().get("min"), Some("2"));
        assert_eq!(range.describe().get("max"), Some("8"));
        assert_eq!(Range::<i64>::NAME, "inclusive range");
        assert!(range.has_field("min"));
        assert!(!range.has_field("span"));
        assert_eq!(range.field_names(), &["min", "max"]);
    }
}
