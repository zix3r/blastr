//! Setting kinds and the specifications built from them.
//!
//! A [`Kind`] knows which text is a legal value and which bounds apply; a
//! [`SettingSpec`] attaches a key, documentation, aliases and an optional
//! default to it.

use macrokit_derive::Describe;

use crate::dimension::Dimension;
use crate::error::{ConfigError, ConfigResult, SchemaError, ValueError};
use crate::ids::SettingId;
use crate::quantity::Quantity;
use crate::unit::Unit;
use crate::util;
use crate::value::{Range, SettingValue};

/// The variant tag of a [`Kind`], used for reports and parse errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KindTag {
    /// Boolean flag.
    Bool,
    /// Signed integer.
    Int,
    /// Decimal number.
    Float,
    /// Text.
    Text,
    /// Physical quantity.
    Quantity,
}

impl KindTag {
    /// The lower case name used in specifications.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::Text => "text",
            Self::Quantity => "quantity",
        }
    }

    /// Parses the tag of a kind specification.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "bool" => Some(Self::Bool),
            "int" => Some(Self::Int),
            "float" => Some(Self::Float),
            "text" => Some(Self::Text),
            "quantity" => Some(Self::Quantity),
            _ => None,
        }
    }
}

/// Limits of a [`Kind::Text`] setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Describe)]
#[describe(name = "text limits")]
pub struct TextLimits {
    max_len: usize,
    allow_empty: bool,
}

impl TextLimits {
    /// Limits that reject empty text.
    pub const fn new(max_len: usize) -> Self {
        Self {
            max_len,
            allow_empty: false,
        }
    }

    /// Maximum number of characters.
    pub const fn max_len(&self) -> usize {
        self.max_len
    }

    /// Whether empty text is accepted.
    pub const fn allow_empty(&self) -> bool {
        self.allow_empty
    }

    /// A copy that also accepts empty text.
    #[must_use]
    pub const fn with_empty_allowed(mut self) -> Self {
        self.allow_empty = true;
        self
    }
}

/// Dimension and bounds of a [`Kind::Quantity`] setting.
#[derive(Debug, Clone, Copy, PartialEq, Describe)]
#[describe(name = "quantity kind")]
pub struct QuantityKind {
    dimension: Dimension,
    bounds: Range<f64>,
}

impl QuantityKind {
    /// Builds a quantity kind; the bounds are expressed in canonical units.
    pub const fn new(dimension: Dimension, bounds: Range<f64>) -> Self {
        Self { dimension, bounds }
    }

    /// The required dimension.
    pub const fn dimension(&self) -> Dimension {
        self.dimension
    }

    /// The accepted canonical bounds.
    pub const fn bounds(&self) -> Range<f64> {
        self.bounds
    }
}

/// What a setting accepts.
///
/// The derive macro only supports structs, so `Kind` describes itself through
/// [`Kind::describe_kind`] and [`Kind::to_spec`] instead of `Describe`.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// A boolean flag.
    Bool,
    /// A signed integer inside the bounds.
    Int(Range<i64>),
    /// A decimal number inside the bounds.
    Float(Range<f64>),
    /// Text with a length limit.
    Text(TextLimits),
    /// A quantity of a fixed dimension inside the canonical bounds.
    Quantity(QuantityKind),
}

impl Kind {
    /// A boolean kind.
    pub const fn bool() -> Self {
        Self::Bool
    }

    /// An integer kind.
    pub fn int(min: i64, max: i64) -> Self {
        Self::Int(Range::new(min, max))
    }

    /// A float kind.
    pub fn float(min: f64, max: f64) -> Self {
        Self::Float(Range::new(min, max))
    }

    /// A text kind.
    pub fn text(max_len: usize) -> Self {
        Self::Text(TextLimits::new(max_len))
    }

    /// A quantity kind; the bounds are canonical.
    pub fn quantity(dimension: Dimension, min: f64, max: f64) -> Self {
        Self::Quantity(QuantityKind::new(dimension, Range::new(min, max)))
    }

    /// The variant tag.
    pub fn tag(&self) -> KindTag {
        match self {
            Self::Bool => KindTag::Bool,
            Self::Int(_) => KindTag::Int,
            Self::Float(_) => KindTag::Float,
            Self::Text(_) => KindTag::Text,
            Self::Quantity(_) => KindTag::Quantity,
        }
    }

    /// The dimension of a quantity kind.
    pub fn dimension(&self) -> Option<Dimension> {
        match self {
            Self::Quantity(specification) => Some(specification.dimension()),
            _ => None,
        }
    }

    /// The preferred unit for a new value, if the kind has a canonical one.
    pub fn canonical_unit(&self) -> Option<Unit> {
        let dimension = self.dimension()?;
        crate::unit::units_with_dimension(dimension)
            .into_iter()
            .find(|unit| unit.scale() == 1.0 && !unit.is_affine())
    }

    /// Parses text into a value of this kind.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] for text of the wrong shape, unknown units,
    /// dimension mismatches and out of range values.
    pub fn parse(&self, raw: &str) -> Result<SettingValue, ValueError> {
        let text = raw.trim();
        if text.is_empty() && self.tag() != KindTag::Text {
            return Err(ValueError::Empty);
        }
        let value = match self {
            Self::Bool => {
                SettingValue::Bool(util::parse_bool(text).ok_or_else(|| ValueError::NotABool {
                    text: text.to_string(),
                })?)
            }
            Self::Int(_) => {
                SettingValue::Int(text.parse().map_err(|_| ValueError::NotAnInteger {
                    text: text.to_string(),
                })?)
            }
            Self::Float(_) => {
                let parsed: f64 = text.parse().map_err(|_| ValueError::NotANumber {
                    text: text.to_string(),
                })?;
                if !parsed.is_finite() {
                    return Err(ValueError::NotANumber {
                        text: text.to_string(),
                    });
                }
                SettingValue::Float(parsed)
            }
            Self::Text(limits) => {
                let text = strip_quotes(text);
                let length = text.chars().count();
                if length == 0 && !limits.allow_empty() {
                    return Err(ValueError::Empty);
                }
                if length > limits.max_len() {
                    return Err(ValueError::TextTooLong {
                        limit: limits.max_len(),
                        actual: length,
                    });
                }
                SettingValue::Text(text.to_string())
            }
            Self::Quantity(_) => SettingValue::Quantity(Quantity::parse(text)?),
        };
        self.check(&value)?;
        Ok(value)
    }

    /// Checks a value that was built programmatically.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the variant, dimension or bounds do not
    /// match.
    pub fn check(&self, value: &SettingValue) -> Result<(), ValueError> {
        match (self, value) {
            (Self::Bool, SettingValue::Bool(_)) => Ok(()),
            (Self::Int(bounds), SettingValue::Int(number)) => check_int(bounds, *number, value),
            (Self::Float(bounds), SettingValue::Float(number)) => {
                check_float(bounds, *number, value)
            }
            (Self::Float(bounds), SettingValue::Int(_)) => {
                let number = value.as_float().unwrap_or(f64::NAN);
                check_float(bounds, number, value)
            }
            (Self::Text(limits), SettingValue::Text(text)) => {
                let length = text.chars().count();
                if length == 0 && !limits.allow_empty() {
                    return Err(ValueError::Empty);
                }
                if length > limits.max_len() {
                    return Err(ValueError::TextTooLong {
                        limit: limits.max_len(),
                        actual: length,
                    });
                }
                Ok(())
            }
            (Self::Quantity(specification), SettingValue::Quantity(quantity)) => {
                let dimension = specification.dimension();
                if quantity.dimension() != dimension {
                    return Err(ValueError::DimensionMismatch {
                        expected: dimension.describe(),
                        found: quantity.dimension().describe(),
                    });
                }
                check_float(&specification.bounds(), quantity.canonical(), value)
            }
            (kind, value) => Err(ValueError::TypeMismatch {
                expected: kind.tag().as_str(),
                found: value.kind_name(),
            }),
        }
    }

    /// Whether the kind accepts the value.
    pub fn accepts(&self, value: &SettingValue) -> bool {
        self.check(value).is_ok()
    }

    /// Widens integers to floats for float kinds, so `set loop.gain 2` stores
    /// `2.0` rather than an integer value.
    pub fn normalize(&self, value: SettingValue) -> SettingValue {
        match (self, value) {
            (Self::Float(_), SettingValue::Int(number)) => SettingValue::Float(number as f64),
            (_, value) => value,
        }
    }

    /// A human readable description of the accepted values.
    pub fn describe_kind(&self) -> String {
        match self {
            Self::Bool => "true or false".to_string(),
            Self::Int(bounds) => format!("integer in {}", bounds.bounds_text()),
            Self::Float(bounds) => format!("number in {}", bounds.bounds_text()),
            Self::Text(limits) => {
                if limits.allow_empty() {
                    format!("text up to {} characters", limits.max_len())
                } else {
                    format!("text of 1..{} characters", limits.max_len())
                }
            }
            Self::Quantity(specification) => format!(
                "quantity of {} in {} (canonical)",
                specification.dimension().describe(),
                specification.bounds().bounds_text()
            ),
        }
    }

    /// The inverse of [`Kind::from_spec`].
    pub fn to_spec(&self) -> String {
        match self {
            Self::Bool => "bool".to_string(),
            Self::Int(bounds) => format!("int:{}", bounds.bounds_text()),
            Self::Float(bounds) => format!("float:{}", bounds.bounds_text()),
            Self::Text(limits) => format!("text:{}", limits.max_len()),
            Self::Quantity(specification) => format!(
                "quantity:{};{}",
                specification.dimension().describe(),
                specification.bounds().bounds_text()
            ),
        }
    }

    /// Parses the kind specifications used by the generated schema.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::InvalidKind`] for unknown tags, malformed bounds
    /// or malformed dimensions.
    pub fn from_spec(text: &str) -> Result<Self, SchemaError> {
        let trimmed = text.trim();
        let (tag, payload) = match trimmed.split_once(':') {
            Some((tag, payload)) => (tag, Some(payload)),
            None => (trimmed, None),
        };
        let tag = KindTag::parse(tag).ok_or_else(|| SchemaError::InvalidKind {
            text: trimmed.to_string(),
            reason: "unknown kind tag".to_string(),
        })?;
        match (tag, payload) {
            (KindTag::Bool, None) => Ok(Self::Bool),
            (KindTag::Int, Some(payload)) => {
                let (min, max) = split_bounds(trimmed, payload)?;
                Ok(Self::int(
                    parse_bound(trimmed, min)?,
                    parse_bound(trimmed, max)?,
                ))
            }
            (KindTag::Float, Some(payload)) => {
                let (min, max) = split_bounds(trimmed, payload)?;
                Ok(Self::float(
                    parse_bound(trimmed, min)?,
                    parse_bound(trimmed, max)?,
                ))
            }
            (KindTag::Text, Some(payload)) => {
                let max_len = payload
                    .trim()
                    .parse()
                    .map_err(|_| SchemaError::InvalidKind {
                        text: trimmed.to_string(),
                        reason: "text kinds need a character limit".to_string(),
                    })?;
                Ok(Self::text(max_len))
            }
            (KindTag::Quantity, Some(payload)) => {
                let (dimension, bounds) =
                    payload
                        .split_once(';')
                        .ok_or_else(|| SchemaError::InvalidKind {
                            text: trimmed.to_string(),
                            reason: "quantity kinds need `<dimension>;<min>..<max>`".to_string(),
                        })?;
                let dimension =
                    Dimension::parse(dimension).map_err(|error| SchemaError::InvalidKind {
                        text: trimmed.to_string(),
                        reason: error.to_string(),
                    })?;
                let (min, max) = split_bounds(trimmed, bounds)?;
                Ok(Self::quantity(
                    dimension,
                    parse_bound(trimmed, min)?,
                    parse_bound(trimmed, max)?,
                ))
            }
            (tag, _) => Err(SchemaError::InvalidKind {
                text: trimmed.to_string(),
                reason: format!("`{}` needs a `:` argument", tag.as_str()),
            }),
        }
    }
}

fn split_bounds<'a>(text: &str, payload: &'a str) -> Result<(&'a str, &'a str), SchemaError> {
    let (min, max) = payload
        .split_once("..")
        .ok_or_else(|| SchemaError::InvalidKind {
            text: text.to_string(),
            reason: "bounds need to be `<min>..<max>`".to_string(),
        })?;
    let (min, max) = (min.trim(), max.trim());
    if min.is_empty() || max.is_empty() {
        return Err(SchemaError::InvalidKind {
            text: text.to_string(),
            reason: "bounds need two values".to_string(),
        });
    }
    Ok((min, max))
}

fn parse_bound<T: std::str::FromStr>(text: &str, bound: &str) -> Result<T, SchemaError> {
    bound.parse().map_err(|_| SchemaError::InvalidKind {
        text: text.to_string(),
        reason: format!("`{bound}` is not a valid bound"),
    })
}

fn check_int(bounds: &Range<i64>, number: i64, value: &SettingValue) -> Result<(), ValueError> {
    if bounds.contains(&number) {
        return Ok(());
    }
    Err(ValueError::OutOfRange {
        text: value.render(),
        bounds: bounds.bounds_text(),
    })
}

/// Removes one pair of surrounding double quotes, as config files write them.
fn strip_quotes(text: &str) -> &str {
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        &text[1..text.len() - 1]
    } else {
        text
    }
}

fn check_float(bounds: &Range<f64>, number: f64, value: &SettingValue) -> Result<(), ValueError> {
    if number.is_finite() && bounds.contains(&number) {
        return Ok(());
    }
    Err(ValueError::OutOfRange {
        text: value.render(),
        bounds: bounds.bounds_text(),
    })
}

/// Everything the schema knows about one setting.
#[derive(Debug, Clone, PartialEq, Describe)]
#[describe(name = "setting")]
pub struct SettingSpec {
    id: SettingId,
    key: String,
    kind: Kind,
    default: Option<SettingValue>,
    required: bool,
    aliases: Vec<String>,
    doc: String,
    deprecated: Option<String>,
}

impl SettingSpec {
    /// Starts building a setting with the given key and kind.
    pub fn builder(key: impl Into<String>, kind: Kind) -> SettingSpecBuilder {
        SettingSpecBuilder::new(key.into(), kind)
    }

    /// The identifier assigned by the schema, or the unassigned placeholder.
    pub fn id(&self) -> SettingId {
        self.id
    }

    /// The canonical key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The accepted values.
    pub fn kind(&self) -> &Kind {
        &self.kind
    }

    /// The declared default, if any.
    pub fn default(&self) -> Option<&SettingValue> {
        self.default.as_ref()
    }

    /// Whether the setting must have a value.
    pub fn required(&self) -> bool {
        self.required
    }

    /// Alternative names accepted by the schema.
    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    /// Documentation shown by `describe`.
    pub fn doc(&self) -> &str {
        &self.doc
    }

    /// The replacement hint of a deprecated setting.
    pub fn deprecation(&self) -> Option<&str> {
        self.deprecated.as_deref()
    }

    /// Whether `alias` names this setting.
    pub fn matches_alias(&self, alias: &str) -> bool {
        self.aliases.iter().any(|candidate| candidate == alias)
    }

    /// Parses text for this setting, attributing errors to the key.
    ///
    /// # Errors
    ///
    /// Wraps [`ValueError`] into [`ConfigError::InvalidValue`].
    pub fn parse(&self, raw: &str) -> ConfigResult<SettingValue> {
        self.kind
            .parse(raw)
            .map_err(|error| ConfigError::InvalidValue {
                key: self.key.clone(),
                reason: error.to_string(),
            })
    }

    /// Checks a programmatic value, attributing errors to the key.
    ///
    /// # Errors
    ///
    /// Wraps [`ValueError`] into [`ConfigError::InvalidValue`].
    pub fn check_value(&self, value: &SettingValue) -> ConfigResult<()> {
        self.kind
            .check(value)
            .map_err(|error| ConfigError::InvalidValue {
                key: self.key.clone(),
                reason: error.to_string(),
            })
    }

    /// Records the identifier the schema assigned.
    pub(crate) fn assign_id(&mut self, id: SettingId) {
        self.id = id;
    }
}

/// Builder for [`SettingSpec`].
#[derive(Debug, Clone)]
pub struct SettingSpecBuilder {
    key: String,
    kind: Kind,
    default: Option<SettingValue>,
    required: bool,
    aliases: Vec<String>,
    doc: Option<String>,
    deprecated: Option<String>,
}

impl SettingSpecBuilder {
    fn new(key: String, kind: Kind) -> Self {
        Self {
            key,
            kind,
            default: None,
            required: false,
            aliases: Vec::new(),
            doc: None,
            deprecated: None,
        }
    }

    /// Records documentation.
    #[must_use]
    pub fn doc(mut self, doc: impl Into<String>) -> Self {
        self.doc = Some(doc.into());
        self
    }

    /// Records a typed default.
    #[must_use]
    pub fn default_value(mut self, value: SettingValue) -> Self {
        self.default = Some(value);
        self
    }

    /// Parses and records a default from text.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::InvalidDefault`] when the text does not satisfy
    /// the kind.
    pub fn default_text(mut self, raw: &str) -> Result<Self, SchemaError> {
        let value = self
            .kind
            .parse(raw)
            .map_err(|error| SchemaError::InvalidDefault {
                key: self.key.clone(),
                reason: error.to_string(),
            })?;
        self.default = Some(value);
        Ok(self)
    }

    /// Marks the setting as required.
    #[must_use]
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Adds one alias.
    #[must_use]
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    /// Marks the setting as deprecated with a replacement hint.
    #[must_use]
    pub fn deprecated(mut self, hint: impl Into<String>) -> Self {
        self.deprecated = Some(hint.into());
        self
    }

    /// Validates the collected data and builds the specification.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::InvalidKey`] for malformed keys or aliases,
    /// [`SchemaError::AliasConflict`] for aliases that repeat the key and
    /// [`SchemaError::InvalidDefault`] for defaults that do not satisfy the
    /// kind.
    pub fn build(self) -> Result<SettingSpec, SchemaError> {
        validate_key(&self.key).map_err(|reason| SchemaError::InvalidKey {
            key: self.key.clone(),
            reason,
        })?;
        let mut seen: Vec<String> = Vec::new();
        for alias in &self.aliases {
            validate_key(alias).map_err(|reason| SchemaError::InvalidKey {
                key: alias.clone(),
                reason,
            })?;
            if *alias == self.key || seen.contains(alias) {
                return Err(SchemaError::AliasConflict {
                    alias: alias.clone(),
                    owner: self.key.clone(),
                });
            }
            seen.push(alias.clone());
        }
        if let Some(default) = &self.default {
            self.kind
                .check(default)
                .map_err(|error| SchemaError::InvalidDefault {
                    key: self.key.clone(),
                    reason: error.to_string(),
                })?;
        }
        Ok(SettingSpec {
            id: SettingId::unassigned(),
            key: self.key,
            kind: self.kind,
            default: self.default,
            required: self.required,
            aliases: self.aliases,
            doc: self.doc.unwrap_or_default(),
            deprecated: self.deprecated,
        })
    }
}

/// Validates a setting key or alias.
///
/// # Errors
///
/// Returns a reason string for empty keys, segments that do not start with
/// `[a-z0-9]` and characters outside `[a-z0-9_-]`.
pub fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("key is empty".to_string());
    }
    for segment in key.split('.') {
        let mut characters = segment.chars();
        match characters.next() {
            Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
            _ => return Err("each segment must start with [a-z0-9]".to_string()),
        }
        if !characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '-'
        }) {
            return Err("only [a-z0-9_-] are allowed inside a segment".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Kind, KindTag, SettingSpec, TextLimits, validate_key};
    use crate::describe::Describe;
    use crate::dimension::Dimension;
    use crate::error::{SchemaError, ValueError};
    use crate::quantity::Quantity;
    use crate::unit::lookup;
    use crate::value::SettingValue;

    #[test]
    fn kinds_parse_their_specification() {
        assert_eq!(Kind::from_spec("bool").unwrap(), Kind::Bool);
        assert_eq!(Kind::from_spec("int:1..64").unwrap(), Kind::int(1, 64));
        assert_eq!(
            Kind::from_spec("float:-2.5..1000").unwrap(),
            Kind::float(-2.5, 1000.0)
        );
        assert_eq!(Kind::from_spec("text:32").unwrap(), Kind::text(32));
        assert_eq!(
            Kind::from_spec("quantity:TIME1;0..3600").unwrap(),
            Kind::quantity(Dimension::TIME, 0.0, 3600.0)
        );
    }

    #[test]
    fn kind_specifications_round_trip() {
        for kind in [
            Kind::bool(),
            Kind::int(0, 12),
            Kind::float(-1.5, 12.0),
            Kind::text(8),
            Kind::quantity(Dimension::PRESSURE, 0.0, 2_000_000.0),
        ] {
            assert_eq!(Kind::from_spec(&kind.to_spec()).unwrap(), kind);
        }
        assert_eq!(Kind::text(8).to_spec(), "text:8");
        assert_eq!(
            Kind::quantity(Dimension::FLOW, 0.0, 10.0).to_spec(),
            "quantity:LEN3 TIME-1;0..10"
        );
    }

    #[test]
    fn malformed_specifications_are_rejected() {
        assert!(matches!(
            Kind::from_spec("integer:1..2"),
            Err(SchemaError::InvalidKind { .. })
        ));
        assert!(matches!(
            Kind::from_spec("int:1"),
            Err(SchemaError::InvalidKind { .. })
        ));
        assert!(matches!(
            Kind::from_spec("quantity:TIME1"),
            Err(SchemaError::InvalidKind { .. })
        ));
        assert!(matches!(
            Kind::from_spec("quantity:BOGUS1;0..1"),
            Err(SchemaError::InvalidKind { .. })
        ));
        assert!(matches!(
            Kind::from_spec("text:many"),
            Err(SchemaError::InvalidKind { .. })
        ));
        assert!(matches!(
            Kind::from_spec("bool:true"),
            Err(SchemaError::InvalidKind { .. })
        ));
    }

    #[test]
    fn tags_and_dimensions() {
        assert_eq!(KindTag::parse("float"), Some(KindTag::Float));
        assert_eq!(KindTag::parse("Float"), None);
        assert_eq!(Kind::int(0, 1).tag().as_str(), "int");
        assert_eq!(
            Kind::quantity(Dimension::TIME, 0.0, 1.0).dimension(),
            Some(Dimension::TIME)
        );
        assert_eq!(Kind::bool().dimension(), None);
        assert_eq!(
            Kind::quantity(Dimension::TIME, 0.0, 1.0)
                .canonical_unit()
                .map(|unit| unit.symbol()),
            Some("s")
        );
        assert_eq!(Kind::bool().canonical_unit(), None);
    }

    #[test]
    fn parsing_checks_bounds_and_variants() {
        let averaging = Kind::int(1, 64);
        assert_eq!(averaging.parse("8").unwrap(), SettingValue::Int(8));
        assert_eq!(
            averaging.parse("65"),
            Err(ValueError::OutOfRange {
                text: "65".to_string(),
                bounds: "1..64".to_string(),
            })
        );
        assert!(matches!(
            averaging.parse("2.5"),
            Err(ValueError::NotAnInteger { .. })
        ));
        let gain = Kind::float(0.0, 1000.0);
        assert_eq!(gain.parse("1.25").unwrap(), SettingValue::Float(1.25));
        assert!(matches!(
            gain.parse("2000"),
            Err(ValueError::OutOfRange { .. })
        ));
        assert!(gain.accepts(&SettingValue::Int(3)));
        assert!(!gain.accepts(&SettingValue::Bool(true)));
        assert!(matches!(
            gain.check(&SettingValue::Text("1".to_string())),
            Err(ValueError::TypeMismatch { .. })
        ));
        assert_eq!(
            gain.normalize(SettingValue::Int(3)),
            SettingValue::Float(3.0)
        );
        assert_eq!(
            averaging.normalize(SettingValue::Int(3)),
            SettingValue::Int(3)
        );
    }

    #[test]
    fn quantity_kinds_convert_before_checking() {
        let pressure = Kind::quantity(Dimension::PRESSURE, 0.0, 2_000_000.0);
        let value = pressure.parse("1.5 MPa").expect("1.5 MPa is in range");
        let quantity = value.as_quantity().expect("quantity variant");
        assert_eq!(quantity.canonical(), 1_500_000.0);
        assert_eq!(
            pressure.parse("12 s"),
            Err(ValueError::DimensionMismatch {
                expected: "LEN-1 MASS1 TIME-2".to_string(),
                found: "TIME1".to_string(),
            })
        );
        assert!(matches!(
            pressure.parse("3 MPa"),
            Err(ValueError::OutOfRange { .. })
        ));
        assert!(matches!(pressure.parse("12"), Err(ValueError::Unit(_))));
    }

    #[test]
    fn text_limits_count_characters() {
        let name = Kind::text(4);
        assert_eq!(
            name.parse(" bay ").unwrap(),
            SettingValue::Text("bay".to_string())
        );
        assert_eq!(
            name.parse("bay ").unwrap(),
            SettingValue::Text("bay".to_string())
        );
        assert!(matches!(name.parse("   "), Err(ValueError::Empty)));
        assert_eq!(
            name.parse("bay-7"),
            Err(ValueError::TextTooLong {
                limit: 4,
                actual: 5
            })
        );
        let optional = Kind::Text(TextLimits::new(4).with_empty_allowed());
        assert_eq!(
            optional.parse("").unwrap(),
            SettingValue::Text(String::new())
        );
        assert!(optional.accepts(&SettingValue::from("")));
        assert_eq!(
            name.parse("\"bay\"").unwrap(),
            SettingValue::Text("bay".to_string())
        );
    }

    #[test]
    fn descriptions_are_human_readable() {
        assert_eq!(Kind::bool().describe_kind(), "true or false");
        assert_eq!(Kind::int(1, 64).describe_kind(), "integer in 1..64");
        assert_eq!(Kind::text(8).describe_kind(), "text of 1..8 characters");
        assert_eq!(
            Kind::quantity(Dimension::TIME, 0.0, 3600.0).describe_kind(),
            "quantity of TIME1 in 0..3600 (canonical)"
        );
        assert_eq!(Kind::float(0.0, 1.0).describe_kind(), "number in 0..1");
    }

    #[test]
    fn builders_validate_their_input() {
        let spec = SettingSpec::builder("loop.gain", Kind::float(0.0, 1000.0))
            .doc("proportional gain")
            .default_text("1.25")
            .expect("default parses")
            .alias("kp")
            .build()
            .expect("spec is valid");
        assert_eq!(spec.key(), "loop.gain");
        assert_eq!(spec.doc(), "proportional gain");
        assert_eq!(spec.default(), Some(&SettingValue::Float(1.25)));
        assert!(spec.matches_alias("kp"));
        assert!(!spec.matches_alias("ki"));
        assert!(spec.id().is_unassigned());
        assert!(!spec.required());
        assert_eq!(spec.deprecation(), None);
        assert_eq!(spec.parse("2").unwrap(), SettingValue::Float(2.0));
        assert!(spec.check_value(&SettingValue::Float(500.0)).is_ok());
        assert!(matches!(
            spec.check_value(&SettingValue::Float(5000.0)),
            Err(crate::error::ConfigError::InvalidValue { .. })
        ));
    }

    #[test]
    fn builders_reject_bad_keys_and_defaults() {
        assert!(matches!(
            SettingSpec::builder("Loop Gain", Kind::bool()).build(),
            Err(SchemaError::InvalidKey { .. })
        ));
        assert!(matches!(
            SettingSpec::builder("loop..gain", Kind::bool()).build(),
            Err(SchemaError::InvalidKey { .. })
        ));
        assert!(matches!(
            SettingSpec::builder("loop.gain", Kind::bool())
                .alias("loop.gain")
                .build(),
            Err(SchemaError::AliasConflict { .. })
        ));
        assert!(matches!(
            SettingSpec::builder("loop.gain", Kind::bool())
                .alias("kp")
                .alias("kp")
                .build(),
            Err(SchemaError::AliasConflict { .. })
        ));
        assert!(matches!(
            SettingSpec::builder("loop.gain", Kind::int(0, 10)).default_text("11"),
            Err(SchemaError::InvalidDefault { .. })
        ));
        assert!(matches!(
            SettingSpec::builder("also.bad", Kind::int(0, 10)).default_text("x"),
            Err(SchemaError::InvalidDefault { .. })
        ));
    }

    #[test]
    fn required_and_deprecated_flags_survive() {
        let spec = SettingSpec::builder(
            "legacy.offset",
            Kind::quantity(Dimension::LENGTH, -100.0, 100.0),
        )
        .required()
        .deprecated("use site.elevation")
        .build()
        .expect("valid");
        assert!(spec.required());
        assert_eq!(spec.deprecation(), Some("use site.elevation"));
        assert!(spec.aliases().is_empty());
        assert_eq!(spec.default(), None);
    }

    #[test]
    fn description_of_a_spec_lists_its_fields() {
        let spec = SettingSpec::builder(
            "sample.interval",
            Kind::quantity(Dimension::TIME, 0.001, 3600.0),
        )
        .default_text("1 s")
        .expect("default parses")
        .required()
        .build()
        .expect("valid");
        let description = spec.describe();
        assert_eq!(description.fields().len(), 8);
        assert_eq!(description.get("key"), Some("\"sample.interval\""));
        assert_eq!(description.get("required"), Some("true"));
        assert!(description.get("default").is_some());
        assert_eq!(SettingSpec::NAME, "setting");
        assert!(spec.has_field("kind"));
        assert!(!spec.has_field("secret"));
    }

    #[test]
    fn keys_are_validated_uniformly() {
        assert!(validate_key("motor.nominal.speed").is_ok());
        assert!(validate_key("log-format").is_ok());
        assert!(validate_key("2fast").is_ok());
        assert_eq!(validate_key(""), Err("key is empty".to_string()));
        assert_eq!(
            validate_key("Bad"),
            Err("each segment must start with [a-z0-9]".to_string())
        );
        assert_eq!(
            validate_key("bad!"),
            Err("only [a-z0-9_-] are allowed inside a segment".to_string())
        );
        assert!(validate_key(".leading").is_err());
    }

    #[test]
    fn canonical_units_are_looked_up_per_dimension() {
        let quantity = Quantity::new(2.0, lookup("kPa").expect("kPa is registered"));
        assert_eq!(quantity.value(), 2.0);
        let kind = Kind::quantity(Dimension::PRESSURE, 0.0, 1.0e7);
        assert!(kind.accepts(&SettingValue::Quantity(quantity)));
    }
}
