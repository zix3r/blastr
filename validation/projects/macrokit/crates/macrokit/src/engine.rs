//! The configuration engine: effective values with provenance.
//!
//! An [`Engine`] owns a [`Schema`] and a set of live values. Values remember
//! where they came from ([`Source`]), every change is appended to a change log
//! and validation combines the required-value checks with the cross-field
//! [`RuleSet`].

use std::collections::BTreeMap;

use crate::error::{ConfigError, ConfigResult};
use crate::ids::SettingId;
use crate::quantity::Quantity;
use crate::schema::Schema;
use crate::spec::SettingSpec;
use crate::unit::Unit;
use crate::util;
use crate::validation::{Issue, RuleSet, Severity, ValidationReport};
use crate::value::SettingValue;

/// Where an effective value came from, lowest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Source {
    /// The declared default of the setting.
    #[default]
    Default,
    /// A configuration file.
    File,
    /// An environment variable.
    Environment,
    /// A command line argument.
    Argument,
    /// A runtime override, for example a trimmed value.
    Override,
}

impl Source {
    /// The lower case name used in reports.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::File => "file",
            Self::Environment => "environment",
            Self::Argument => "argument",
            Self::Override => "override",
        }
    }

    /// Precedence weight, higher wins.
    pub fn weight(&self) -> u8 {
        match self {
            Self::Default => 0,
            Self::File => 1,
            Self::Environment => 2,
            Self::Argument => 3,
            Self::Override => 4,
        }
    }

    /// Parses a source name.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "default" => Some(Self::Default),
            "file" => Some(Self::File),
            "environment" | "env" => Some(Self::Environment),
            "argument" | "arg" => Some(Self::Argument),
            "override" => Some(Self::Override),
            _ => None,
        }
    }
}

/// One effective value together with its provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    value: SettingValue,
    source: Source,
    revision: u32,
}

impl Entry {
    /// The value.
    pub fn value(&self) -> &SettingValue {
        &self.value
    }

    /// Where the value came from.
    pub fn source(&self) -> Source {
        self.source
    }

    /// The engine revision that produced the value.
    pub fn revision(&self) -> u32 {
        self.revision
    }
}

/// One applied change.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    key: String,
    id: SettingId,
    previous: Option<SettingValue>,
    current: Option<SettingValue>,
    source: Source,
}

impl Change {
    /// The affected key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The identifier of the affected setting.
    pub fn id(&self) -> SettingId {
        self.id
    }

    /// The value before the change, if any.
    pub fn previous(&self) -> Option<&SettingValue> {
        self.previous.as_ref()
    }

    /// The value after the change, if any.
    pub fn current(&self) -> Option<&SettingValue> {
        self.current.as_ref()
    }

    /// The source of the new value.
    pub fn source(&self) -> Source {
        self.source
    }

    /// Whether the change removed the value.
    pub fn is_removal(&self) -> bool {
        self.current.is_none()
    }

    /// `key: old -> new (source)`.
    pub fn render(&self) -> String {
        let previous = self
            .previous
            .as_ref()
            .map_or_else(|| "<unset>".to_string(), SettingValue::render);
        let current = self
            .current
            .as_ref()
            .map_or_else(|| "<unset>".to_string(), SettingValue::render);
        format!(
            "{}: {} -> {} ({})",
            self.key,
            previous,
            current,
            self.source.as_str()
        )
    }
}

/// A value as the engine sees it, default included.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveEntry {
    key: String,
    value: SettingValue,
    source: Source,
}

impl EffectiveEntry {
    fn new(key: String, value: SettingValue, source: Source) -> Self {
        Self { key, value, source }
    }

    /// The key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The effective value.
    pub fn value(&self) -> &SettingValue {
        &self.value
    }

    /// Where the value came from.
    pub fn source(&self) -> Source {
        self.source
    }
}

/// The difference between two snapshots of the engine.
#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    key: String,
    earlier: Option<SettingValue>,
    later: Option<SettingValue>,
}

impl Diff {
    /// The affected key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The value in the earlier snapshot.
    pub fn earlier(&self) -> Option<&SettingValue> {
        self.earlier.as_ref()
    }

    /// The value in the later snapshot.
    pub fn later(&self) -> Option<&SettingValue> {
        self.later.as_ref()
    }

    /// `key: earlier -> later`.
    pub fn render(&self) -> String {
        let earlier = self
            .earlier
            .as_ref()
            .map_or_else(|| "<absent>".to_string(), SettingValue::render);
        let later = self
            .later
            .as_ref()
            .map_or_else(|| "<absent>".to_string(), SettingValue::render);
        format!("{}: {} -> {}", self.key, earlier, later)
    }
}

/// A snapshot of every effective value, keyed by setting key.
pub type Snapshot = BTreeMap<String, SettingValue>;

/// The configuration engine.
#[derive(Debug, Clone)]
pub struct Engine {
    schema: Schema,
    entries: BTreeMap<SettingId, Entry>,
    log: Vec<Change>,
    revision: u32,
    rules: RuleSet,
}

impl Engine {
    /// Builds an engine over `schema` and applies every declared default.
    pub fn new(schema: Schema) -> Self {
        let mut engine = Self {
            schema,
            entries: BTreeMap::new(),
            log: Vec::new(),
            revision: 0,
            rules: RuleSet::default(),
        };
        engine.apply_defaults();
        engine
    }

    /// Builds an engine over the generated baseline schema and rules.
    pub fn with_baseline() -> Self {
        let mut engine = Self::new(Schema::baseline());
        engine.rules = RuleSet::baseline();
        engine
    }

    /// Replaces the rule set used by [`Engine::validate`].
    #[must_use]
    pub fn with_rules(mut self, rules: RuleSet) -> Self {
        self.rules = rules;
        self
    }

    fn apply_defaults(&mut self) {
        let defaults: Vec<(SettingId, SettingValue)> = self
            .schema
            .iter()
            .filter_map(|spec| spec.default().map(|value| (spec.id(), value.clone())))
            .collect();
        for (id, value) in defaults {
            self.entries.insert(
                id,
                Entry {
                    value,
                    source: Source::Default,
                    revision: 0,
                },
            );
        }
    }

    /// The schema behind the engine.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// The rule set used for validation.
    pub fn rules(&self) -> &RuleSet {
        &self.rules
    }

    /// The current revision counter.
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// Sets a setting from text, attributing the value to
    /// [`Source::Argument`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for unknown keys and invalid values.
    pub fn set(&mut self, key: &str, raw: &str) -> ConfigResult<()> {
        self.set_from(key, raw, Source::Argument)
    }

    /// Sets a setting from text with an explicit source.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for unknown keys and invalid values.
    pub fn set_from(&mut self, key: &str, raw: &str, source: Source) -> ConfigResult<()> {
        let spec = self.schema.require(key)?.clone();
        let value = spec.parse(raw)?;
        self.store(spec.id(), &spec, Some(value), source);
        Ok(())
    }

    /// Sets a setting from a typed value.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidValue`] when the value does not satisfy
    /// the kind of the setting.
    pub fn set_value(
        &mut self,
        key: &str,
        value: SettingValue,
        source: Source,
    ) -> ConfigResult<()> {
        let spec = self.schema.require(key)?.clone();
        let value = spec.kind().normalize(value);
        spec.check_value(&value)?;
        self.store(spec.id(), &spec, Some(value), source);
        Ok(())
    }

    /// Removes an explicit value, falling back to the declared default.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::UnknownKey`] for unknown keys.
    pub fn unset(&mut self, key: &str, source: Source) -> ConfigResult<bool> {
        let spec = self.schema.require(key)?.clone();
        let Some(entry) = self.entries.remove(&spec.id()) else {
            return Ok(false);
        };
        self.revision += 1;
        self.log.push(Change {
            key: spec.key().to_string(),
            id: spec.id(),
            previous: Some(entry.value),
            current: None,
            source,
        });
        Ok(true)
    }

    /// Restores the declared default of one setting.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::UnknownKey`] for unknown keys.
    pub fn reset(&mut self, key: &str) -> ConfigResult<()> {
        let spec = self.schema.require(key)?.clone();
        match spec.default() {
            Some(default) => {
                let value = default.clone();
                self.store(spec.id(), &spec, Some(value), Source::Default);
                Ok(())
            }
            None => {
                self.unset(spec.key(), Source::Default)?;
                Ok(())
            }
        }
    }

    /// Restores every declared default and drops the rest.
    pub fn reset_all(&mut self) {
        self.entries.clear();
        self.apply_defaults();
        self.revision += 1;
    }

    fn store(
        &mut self,
        id: SettingId,
        spec: &SettingSpec,
        value: Option<SettingValue>,
        source: Source,
    ) {
        let previous = self.entries.get(&id).map(|entry| entry.value.clone());
        let previous_source = self.entries.get(&id).map(Entry::source);
        self.revision += 1;
        let revision = self.revision;
        match &value {
            Some(new_value) => {
                self.entries.insert(
                    id,
                    Entry {
                        value: new_value.clone(),
                        source,
                        revision,
                    },
                );
            }
            None => {
                self.entries.remove(&id);
            }
        }
        if previous != value || previous_source != Some(source) {
            self.log.push(Change {
                key: spec.key().to_string(),
                id,
                previous,
                current: value,
                source,
            });
        }
    }

    /// The effective value of a setting, default included.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::UnknownKey`] for unknown keys and
    /// [`ConfigError::MissingValue`] when the setting has no value.
    pub fn get(&self, key: &str) -> ConfigResult<&SettingValue> {
        let spec = self.schema.require(key)?;
        if let Some(entry) = self.entries.get(&spec.id()) {
            return Ok(&entry.value);
        }
        spec.default().ok_or_else(|| ConfigError::MissingValue {
            key: spec.key().to_string(),
        })
    }

    /// The entry of a setting, if it has an explicit value.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::UnknownKey`] for unknown keys.
    pub fn get_entry(&self, key: &str) -> ConfigResult<Option<&Entry>> {
        let spec = self.schema.require(key)?;
        Ok(self.entries.get(&spec.id()))
    }

    /// The boolean value of a setting.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidValue`] when the setting is not a bool.
    pub fn get_bool(&self, key: &str) -> ConfigResult<bool> {
        self.get(key)?
            .as_bool()
            .ok_or_else(|| ConfigError::InvalidValue {
                key: key.trim().to_string(),
                reason: "expected a bool value".to_string(),
            })
    }

    /// The integer value of a setting.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidValue`] when the setting is not an int.
    pub fn get_int(&self, key: &str) -> ConfigResult<i64> {
        self.get(key)?
            .as_int()
            .ok_or_else(|| ConfigError::InvalidValue {
                key: key.trim().to_string(),
                reason: "expected an int value".to_string(),
            })
    }

    /// The numeric value of a setting, widening integers.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidValue`] when the setting is not numeric.
    pub fn get_float(&self, key: &str) -> ConfigResult<f64> {
        self.get(key)?
            .as_float()
            .ok_or_else(|| ConfigError::InvalidValue {
                key: key.trim().to_string(),
                reason: "expected a numeric value".to_string(),
            })
    }

    /// The text value of a setting.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidValue`] when the setting is not text.
    pub fn get_text(&self, key: &str) -> ConfigResult<String> {
        self.get(key)?
            .as_text()
            .map(str::to_string)
            .ok_or_else(|| ConfigError::InvalidValue {
                key: key.trim().to_string(),
                reason: "expected a text value".to_string(),
            })
    }

    /// The quantity value of a setting.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidValue`] when the setting is not a
    /// quantity.
    pub fn get_quantity(&self, key: &str) -> ConfigResult<Quantity> {
        self.get(key)?
            .as_quantity()
            .ok_or_else(|| ConfigError::InvalidValue {
                key: key.trim().to_string(),
                reason: "expected a quantity value".to_string(),
            })
    }

    /// The quantity value of a setting, re-expressed in `unit`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Unit`] when the units do not match.
    pub fn get_quantity_in(&self, key: &str, unit: Unit) -> ConfigResult<Quantity> {
        let quantity = self.get_quantity(key)?;
        Ok(quantity.to(unit)?)
    }

    /// The source of the explicit value of a setting.
    pub fn source_of(&self, key: &str) -> Option<Source> {
        let spec = self.schema.resolve(key)?;
        self.entries.get(&spec.id()).map(Entry::source)
    }

    /// Whether the setting has a value that was supplied at runtime.
    pub fn is_set(&self, key: &str) -> bool {
        matches!(self.source_of(key), Some(source) if source != Source::Default)
    }

    /// Applies `(key, text)` pairs in order.
    ///
    /// # Errors
    ///
    /// Returns the first [`ConfigError`] encountered.
    pub fn apply_pairs<'a, I>(&mut self, pairs: I) -> ConfigResult<usize>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut applied = 0;
        for (key, value) in pairs {
            self.set_from(key, value, Source::Argument)?;
            applied += 1;
        }
        Ok(applied)
    }

    /// Applies `key=value` tokens as produced by a command line.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Syntax`] for tokens without `=`.
    pub fn apply_tokens(&mut self, tokens: &[String]) -> ConfigResult<usize> {
        let mut applied = 0;
        for (index, token) in tokens.iter().enumerate() {
            let (key, value) = token.split_once('=').ok_or_else(|| ConfigError::Syntax {
                location: format!("argument {}", index + 1),
                text: token.clone(),
                reason: "expected `key=value`".to_string(),
            })?;
            self.set_from(key.trim(), value.trim(), Source::Argument)?;
            applied += 1;
        }
        Ok(applied)
    }

    /// Applies a small configuration document.
    ///
    /// Blank lines and `#` comments are skipped; every other line must be
    /// `key = value`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Syntax`] for malformed lines and the first value
    /// error otherwise.
    pub fn apply_text(&mut self, text: &str) -> ConfigResult<usize> {
        let mut applied = 0;
        for (number, raw_line) in text.lines().enumerate() {
            let line = util::split_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = split_assignment(line).ok_or_else(|| ConfigError::Syntax {
                location: format!("line {}", number + 1),
                text: raw_line.trim().to_string(),
                reason: "expected `key = value`".to_string(),
            })?;
            self.set_from(key, value, Source::File)?;
            applied += 1;
        }
        Ok(applied)
    }

    /// Every effective value, in key order.
    pub fn effective(&self) -> Vec<EffectiveEntry> {
        let mut entries = Vec::new();
        for spec in self.schema.iter() {
            if let Some(entry) = self.entries.get(&spec.id()) {
                entries.push(EffectiveEntry::new(
                    spec.key().to_string(),
                    entry.value.clone(),
                    entry.source,
                ));
            } else if let Some(default) = spec.default() {
                entries.push(EffectiveEntry::new(
                    spec.key().to_string(),
                    default.clone(),
                    Source::Default,
                ));
            }
        }
        entries
    }

    /// Number of effective values per source.
    pub fn count_by_source(&self) -> BTreeMap<Source, usize> {
        let mut counts: BTreeMap<Source, usize> = BTreeMap::new();
        for entry in self.effective() {
            *counts.entry(entry.source()).or_default() += 1;
        }
        counts
    }

    /// The change log, oldest first.
    pub fn log(&self) -> &[Change] {
        &self.log
    }

    /// Removes and returns the change log.
    pub fn take_log(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.log)
    }

    /// A snapshot of every effective value.
    pub fn snapshot(&self) -> Snapshot {
        self.effective()
            .into_iter()
            .map(|entry| (entry.key().to_string(), entry.value().clone()))
            .collect()
    }

    /// The differences between `earlier` and the current state, in key order.
    pub fn diff(&self, earlier: &Snapshot) -> Vec<Diff> {
        let later = self.snapshot();
        let mut diffs = Vec::new();
        for (key, old) in earlier {
            match later.get(key) {
                Some(new) if new != old => diffs.push(Diff {
                    key: key.clone(),
                    earlier: Some(old.clone()),
                    later: Some(new.clone()),
                }),
                Some(_) => {}
                None => diffs.push(Diff {
                    key: key.clone(),
                    earlier: Some(old.clone()),
                    later: None,
                }),
            }
        }
        for (key, new) in &later {
            if !earlier.contains_key(key) {
                diffs.push(Diff {
                    key: key.clone(),
                    earlier: None,
                    later: Some(new.clone()),
                });
            }
        }
        diffs.sort_by(|left, right| left.key.cmp(&right.key));
        diffs
    }

    /// Validates the effective configuration.
    pub fn validate(&self) -> ValidationReport {
        let mut report = self.rules.evaluate(self);
        self.collect_missing(&mut report);
        report.sort();
        report
    }

    fn collect_missing(&self, report: &mut ValidationReport) {
        for spec in self.schema.iter() {
            report.add_checked(1);
            let has_value = self.entries.contains_key(&spec.id()) || spec.default().is_some();
            if spec.required() && !has_value {
                report.push(Issue::new(
                    Severity::Error,
                    spec.key(),
                    "required setting has no value",
                ));
            }
            if let Some(hint) = spec.deprecation()
                && self.entries.contains_key(&spec.id())
            {
                report.push(Issue::new(
                    Severity::Warning,
                    spec.key(),
                    format!("deprecated: {hint}"),
                ));
            }
        }
    }

    /// Validates and turns the first error into a [`ConfigError`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Validation`] when the report contains an error
    /// level issue.
    pub fn check(&self) -> ConfigResult<()> {
        let report = self.validate();
        let errors = report.errors();
        match errors.first() {
            None => Ok(()),
            Some(issue) => Err(ConfigError::Validation {
                errors: errors.len(),
                first: issue.message().to_string(),
            }),
        }
    }

    /// A table of effective values plus a provenance histogram.
    pub fn report(&self) -> String {
        let mut lines = vec![crate::render::settings_table(self)];
        lines.push(crate::render::source_histogram(self));
        lines.join("\n")
    }
}

fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once('=')?;
    let (key, value) = (key.trim(), value.trim());
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value))
}

#[cfg(test)]
mod tests {
    use super::{Engine, Source};
    use crate::error::ConfigError;
    use crate::generated::GENERATED_SETTING_COUNT;
    use crate::quantity::Quantity;
    use crate::schema::Schema;
    use crate::spec::{Kind, SettingSpec};
    use crate::unit::lookup;
    use crate::value::SettingValue;

    fn small_engine() -> Engine {
        let mut schema = Schema::new();
        schema
            .insert(
                SettingSpec::builder("gain", Kind::float(0.0, 100.0))
                    .default_text("1.5")
                    .expect("default parses")
                    .build()
                    .expect("valid"),
            )
            .expect("insert succeeds");
        schema
            .insert(
                SettingSpec::builder(
                    "interval",
                    Kind::quantity(crate::dimension::Dimension::TIME, 0.0, 60.0),
                )
                .default_text("2 s")
                .expect("default parses")
                .build()
                .expect("valid"),
            )
            .expect("insert succeeds");
        schema
            .insert(
                SettingSpec::builder("label", Kind::text(8))
                    .required()
                    .build()
                    .expect("valid"),
            )
            .expect("insert succeeds");
        Engine::new(schema)
    }

    #[test]
    fn defaults_are_applied_on_construction() {
        let engine = Engine::with_baseline();
        assert_eq!(engine.schema().len(), GENERATED_SETTING_COUNT);
        assert_eq!(engine.count_by_source()[&Source::Default], 28);
        assert!(!engine.is_set("loop.gain"));
        assert_eq!(engine.source_of("loop.gain"), Some(Source::Default));
        assert_eq!(
            engine.get("loop.gain").expect("has a default"),
            &SettingValue::Float(1.25)
        );
        assert_eq!(engine.revision(), 0);
    }

    #[test]
    fn setting_records_an_entry_and_a_change() {
        let mut engine = small_engine();
        engine.set("gain", "2.5").expect("in range");
        assert_eq!(engine.revision(), 1);
        assert_eq!(engine.get("gain").expect("set"), &SettingValue::Float(2.5));
        assert_eq!(engine.source_of("gain"), Some(Source::Argument));
        assert!(engine.is_set("gain"));
        let log = engine.log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].key(), "gain");
        assert_eq!(log[0].source(), Source::Argument);
        assert_eq!(log[0].previous(), Some(&SettingValue::Float(1.5)));
        assert_eq!(log[0].current(), Some(&SettingValue::Float(2.5)));
        assert!(!log[0].is_removal());
        assert_eq!(log[0].render(), "gain: 1.5 -> 2.5 (argument)");
        assert!(log[0].id().get() < 3);
    }

    #[test]
    fn repeated_identical_values_do_not_log() {
        let mut engine = small_engine();
        engine.set("gain", "2.5").expect("in range");
        engine.set("gain", "2.5").expect("in range");
        assert_eq!(engine.revision(), 2);
        assert_eq!(engine.log().len(), 1);
        engine
            .set_from("gain", "2.5", Source::File)
            .expect("in range");
        assert_eq!(engine.log().len(), 2);
        assert_eq!(engine.log()[1].source(), Source::File);
    }

    #[test]
    fn invalid_values_are_rejected_without_side_effects() {
        let mut engine = small_engine();
        assert!(matches!(
            engine.set("gain", "500"),
            Err(ConfigError::InvalidValue { .. })
        ));
        assert!(matches!(
            engine.set("gain", "loud"),
            Err(ConfigError::InvalidValue { .. })
        ));
        assert!(matches!(
            engine.set("nope", "1"),
            Err(ConfigError::UnknownKey { .. })
        ));
        assert_eq!(engine.revision(), 0);
        assert!(engine.log().is_empty());
        assert_eq!(
            engine.set_value("gain", SettingValue::Float(500.0), Source::Override),
            Err(ConfigError::InvalidValue {
                key: "gain".to_string(),
                reason: "`500` is outside 0..100".to_string()
            })
        );
        engine
            .set_value("gain", SettingValue::Int(3), Source::Override)
            .expect("integers are accepted for floats");
        assert_eq!(engine.get("gain").expect("set"), &SettingValue::Float(3.0));
    }

    #[test]
    fn typed_getters_check_the_kind() {
        let mut engine = small_engine();
        engine.set("gain", "2.5").expect("in range");
        engine.set("interval", "1500 ms").expect("in range");
        engine.set("label", "bay-1").expect("in range");
        assert_eq!(engine.get_float("gain").expect("float"), 2.5);
        assert_eq!(
            engine.get_int("label").unwrap_err(),
            ConfigError::InvalidValue {
                key: "label".to_string(),
                reason: "expected an int value".to_string()
            }
        );
        assert_eq!(engine.get_text("label").expect("text"), "bay-1");
        assert_eq!(
            engine.get_bool("label").unwrap_err(),
            ConfigError::InvalidValue {
                key: "label".to_string(),
                reason: "expected a bool value".to_string()
            }
        );
        let interval = engine.get_quantity("interval").expect("quantity");
        assert_eq!(interval.canonical(), 1.5);
        let milliseconds = lookup("ms").expect("ms is registered");
        let converted = engine
            .get_quantity_in("interval", milliseconds)
            .expect("compatible");
        assert_eq!(converted.value(), 1500.0);
        assert_eq!(
            engine
                .get_quantity_in("interval", lookup("s").expect("s"))
                .unwrap()
                .value(),
            1.5
        );
        assert!(matches!(
            engine.get_quantity_in("interval", lookup("kPa").expect("kPa")),
            Err(ConfigError::Unit(_))
        ));
        assert_eq!(
            engine.get_int("gain").unwrap_err(),
            ConfigError::InvalidValue {
                key: "gain".to_string(),
                reason: "expected an int value".to_string()
            }
        );
    }

    #[test]
    fn missing_values_are_reported() {
        let engine = small_engine();
        assert_eq!(
            engine.get("label").unwrap_err(),
            ConfigError::MissingValue {
                key: "label".to_string()
            }
        );
        assert_eq!(
            engine.get_bool("label").unwrap_err(),
            ConfigError::MissingValue {
                key: "label".to_string()
            }
        );
        assert!(engine.get_entry("label").expect("known key").is_none());
        assert!(engine.get_entry("nothing").is_err());
        assert!(!engine.is_set("label"));
        assert_eq!(engine.source_of("nothing"), None);
    }

    #[test]
    fn unset_and_reset_restore_defaults() {
        let mut engine = small_engine();
        engine.set("gain", "4").expect("in range");
        assert!(engine.unset("gain", Source::Override).expect("known"));
        assert!(!engine.unset("gain", Source::Override).expect("known"));
        assert_eq!(
            engine.get("gain").expect("default again"),
            &SettingValue::Float(1.5)
        );
        assert!(engine.log().last().expect("logged").is_removal());
        assert_eq!(
            engine.log().last().expect("logged").render(),
            "gain: 4 -> <unset> (override)"
        );
        engine.set("gain", "9").expect("in range");
        engine.reset("gain").expect("known key");
        assert_eq!(
            engine.get("gain").expect("default"),
            &SettingValue::Float(1.5)
        );
        assert_eq!(engine.source_of("gain"), Some(Source::Default));
        engine.reset("label").expect("known key");
        assert!(matches!(
            engine.get("label"),
            Err(ConfigError::MissingValue { .. })
        ));
        assert!(matches!(
            engine.unset("nothing", Source::File),
            Err(ConfigError::UnknownKey { ref key, .. }) if key == "nothing"
        ));
    }

    #[test]
    fn reset_all_drops_explicit_values() {
        let mut engine = small_engine();
        engine.set("gain", "4").expect("in range");
        engine.set("label", "bay").expect("in range");
        assert_eq!(engine.count_by_source()[&Source::Argument], 2);
        engine.reset_all();
        assert!(!engine.is_set("gain"));
        assert!(!engine.is_set("label"));
        let counts = engine.count_by_source();
        assert_eq!(counts[&Source::Default], 2);
        assert!(!counts.contains_key(&Source::Argument));
    }

    #[test]
    fn pairs_tokens_and_documents_apply_in_order() {
        let mut engine = small_engine();
        assert_eq!(
            engine
                .apply_pairs([("gain", "2"), ("interval", "5 s")])
                .expect("all valid"),
            2
        );
        assert_eq!(engine.get_float("gain").expect("set"), 2.0);
        assert_eq!(
            engine
                .apply_tokens(&["gain=3".to_string(), "label=bay-7".to_string()])
                .expect("all valid"),
            2
        );
        assert_eq!(engine.get_float("gain").expect("set"), 3.0);
        assert_eq!(
            engine.apply_tokens(&["gain".to_string()]).unwrap_err(),
            ConfigError::Syntax {
                location: "argument 1".to_string(),
                text: "gain".to_string(),
                reason: "expected `key=value`".to_string()
            }
        );
        let document = "\
# trimmed gains\n\
gain = 4.5   # tuned on site\n\
\n\
  interval = 750 ms\n\
label = \"bay 9\"\n";
        assert_eq!(engine.apply_text(document).expect("document applies"), 3);
        assert_eq!(engine.get_float("gain").expect("set"), 4.5);
        assert_eq!(
            engine.get_quantity("interval").expect("set").canonical(),
            0.75
        );
        assert_eq!(engine.get_text("label").expect("set"), "bay 9");
        assert_eq!(engine.source_of("label"), Some(Source::File));
        assert_eq!(
            engine.apply_text("broken line").unwrap_err(),
            ConfigError::Syntax {
                location: "line 1".to_string(),
                text: "broken line".to_string(),
                reason: "expected `key = value`".to_string()
            }
        );
        assert_eq!(
            engine.apply_text("gain =").unwrap_err(),
            ConfigError::Syntax {
                location: "line 1".to_string(),
                text: "gain =".to_string(),
                reason: "expected `key = value`".to_string()
            }
        );
    }

    #[test]
    fn snapshots_and_diffs_track_changes() {
        let mut engine = small_engine();
        engine.set("label", "bay-1").expect("in range");
        let before = engine.snapshot();
        assert_eq!(before.len(), 3);
        engine.set("gain", "2").expect("in range");
        engine.unset("interval", Source::Override).expect("known");
        let diffs = engine.diff(&before);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].key(), "gain");
        assert_eq!(diffs[0].render(), "gain: 1.5 -> 2");
        engine.set("label", "bay-2").expect("in range");
        let diffs = engine.diff(&before);
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0].key(), "gain");
        assert_eq!(diffs[1].key(), "label");
        let mut earlier = before;
        earlier.remove("label");
        let diffs = engine.diff(&earlier);
        assert!(diffs.iter().any(|diff| diff.key() == "label"
            && diff.earlier().is_none()
            && diff.later().is_some()));
        let mut with_extra = engine.snapshot();
        with_extra.insert("ghost".to_string(), SettingValue::from("x"));
        let diffs = engine.diff(&with_extra);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].later().is_none());
        assert_eq!(diffs[0].render(), "ghost: x -> <absent>");
    }

    #[test]
    fn validation_combines_rules_and_required_values() {
        let engine = small_engine();
        let report = engine.validate();
        assert_eq!(report.errors().len(), 1);
        assert_eq!(report.errors()[0].key(), "label");
        assert_eq!(report.checked(), 3);
        assert_eq!(
            engine.check().unwrap_err(),
            ConfigError::Validation {
                errors: 1,
                first: "required setting has no value".to_string()
            }
        );
        assert!(engine.rules().is_empty());
        assert_eq!(engine.revision(), 0);
    }

    #[test]
    fn deprecated_keys_warn_when_used() {
        let mut engine = Engine::with_baseline();
        assert!(engine.validate().is_ok());
        engine.set("legacy.offset", "5 m").expect("in range");
        let report = engine.validate();
        assert!(report.is_ok());
        let warnings = report.warnings();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].key(), "legacy.offset");
        assert_eq!(
            warnings[0].message(),
            "deprecated: Superseded by site.elevation."
        );
        assert!(engine.check().is_ok());
    }

    #[test]
    fn baseline_report_is_a_table() {
        let engine = Engine::with_baseline();
        let report = engine.report();
        assert!(report.contains("key"));
        assert!(report.contains("fluid.density"));
        let entries = engine.effective();
        assert_eq!(entries.len(), 28);
        assert_eq!(entries[0].key(), "fluid.density");
        assert!(matches!(
            entries[0].value(),
            SettingValue::Quantity(quantity) if quantity.unit().symbol() == "kg/m3"
        ));
        let mut engine = engine;
        let log = engine.take_log();
        assert!(log.is_empty());
        assert!(engine.log().is_empty());
        let quantities: Vec<Quantity> = engine
            .effective()
            .into_iter()
            .filter_map(|entry| entry.value().as_quantity())
            .collect();
        assert_eq!(quantities.len(), 17);
    }

    #[test]
    fn sources_carry_precedence() {
        assert!(Source::Override > Source::Argument);
        assert!(Source::Argument > Source::Environment);
        assert!(Source::Environment > Source::File);
        assert!(Source::File > Source::Default);
        assert_eq!(Source::default(), Source::Default);
        assert_eq!(Source::parse("env"), Some(Source::Environment));
        assert_eq!(Source::parse("arg"), Some(Source::Argument));
        assert_eq!(Source::parse("nowhere"), None);
        assert_eq!(Source::Override.weight(), 4);
        assert_eq!(Source::Default.as_str(), "default");
    }

    #[test]
    fn rules_can_be_replaced() {
        let rules = crate::validation::RuleSet::new(vec![crate::validation::Rule::new(
            "gain",
            crate::validation::RelOp::Greater,
            "gain",
            crate::validation::Severity::Warning,
            "gain must exceed itself",
        )]);
        let mut engine = small_engine().with_rules(rules);
        engine.set("label", "bay").expect("in range");
        let report = engine.validate();
        assert_eq!(report.warnings().len(), 1);
        assert!(report.is_ok());
        assert_eq!(report.checked(), 4);
    }
}
