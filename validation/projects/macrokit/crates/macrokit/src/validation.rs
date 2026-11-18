//! Cross-field validation rules.
//!
//! A [`Rule`] compares two settings of an [`Engine`] and reports an [`Issue`]
//! when the comparison fails. The baseline table below is the one the
//! `validate` command runs.

use std::cmp::Ordering;

use crate::engine::Engine;
use crate::value::SettingValue;

/// How serious an [`Issue`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    /// Reported, but the configuration is still usable.
    Warning,
    /// The configuration must not be used.
    Error,
}

impl Severity {
    /// The lower case name used in reports.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    /// Whether this severity blocks the configuration.
    pub fn is_error(&self) -> bool {
        *self == Self::Error
    }
}

/// A comparison between two settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RelOp {
    /// `left < right`.
    Less,
    /// `left <= right`.
    LessOrEqual,
    /// `left >= right`.
    GreaterOrEqual,
    /// `left > right`.
    Greater,
}

impl RelOp {
    /// Whether the operator is satisfied by `ordering`.
    pub fn holds(&self, ordering: Ordering) -> bool {
        match self {
            Self::Less => ordering == Ordering::Less,
            Self::LessOrEqual => ordering != Ordering::Greater,
            Self::GreaterOrEqual => ordering != Ordering::Less,
            Self::Greater => ordering == Ordering::Greater,
        }
    }
}

/// One row of the baseline rule table: left, comparison, right, severity, message.
pub type RuleSeed = (&'static str, RelOp, &'static str, Severity, &'static str);

/// The rules `macrokit` ships and the station inherits.
pub const BASELINE: [RuleSeed; 6] = [
    (
        "fluid.flow.maximum",
        RelOp::GreaterOrEqual,
        "fluid.flow.nominal",
        Severity::Error,
        "maximum flow must be at least the nominal flow",
    ),
    (
        "io.timeout",
        RelOp::GreaterOrEqual,
        "sample.interval",
        Severity::Warning,
        "timeout should span at least one sample interval",
    ),
    (
        "loop.integral",
        RelOp::GreaterOrEqual,
        "sample.interval",
        Severity::Warning,
        "integral time should be longer than the sample interval",
    ),
    (
        "power.limit",
        RelOp::GreaterOrEqual,
        "heat.design.duty",
        Severity::Error,
        "power budget must cover the design duty",
    ),
    (
        "sensor.supply.temperature",
        RelOp::Greater,
        "sensor.return.temperature",
        Severity::Error,
        "supply temperature must exceed the return temperature",
    ),
    (
        "vessel.rating.pressure",
        RelOp::GreaterOrEqual,
        "sensor.supply.pressure",
        Severity::Error,
        "vessel rating must cover the supply pressure",
    ),
];

/// A comparison between two settings that must hold.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Rule {
    left: String,
    operator: RelOp,
    right: String,
    severity: Severity,
    message: String,
}

impl Rule {
    /// Builds a rule.
    pub fn new(
        left: impl Into<String>,
        operator: RelOp,
        right: impl Into<String>,
        severity: Severity,
        message: impl Into<String>,
    ) -> Self {
        Self {
            left: left.into(),
            operator,
            right: right.into(),
            severity,
            message: message.into(),
        }
    }

    /// Builds a rule from one baseline row.
    pub fn from_seed((left, operator, right, severity, message): &RuleSeed) -> Self {
        Self::new(*left, *operator, *right, *severity, *message)
    }

    /// Key of the left hand side.
    pub fn left(&self) -> &str {
        &self.left
    }

    /// Key of the right hand side.
    pub fn right(&self) -> &str {
        &self.right
    }

    /// The comparison.
    pub fn operator(&self) -> RelOp {
        self.operator
    }

    /// Severity of a violation.
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// Message of a violation.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Checks the rule, returning the [`Issue`] describing a violation.
    ///
    /// # Errors
    ///
    /// Returns an error level issue when a setting has no value or the two
    /// values cannot be compared, and a rule level issue when the comparison
    /// fails.
    pub fn evaluate(&self, engine: &Engine) -> Result<(), Issue> {
        let left = engine.get(&self.left).map_err(|error| {
            Issue::new(
                Severity::Error,
                self.left.clone(),
                format!("cannot evaluate rule: {error}"),
            )
        })?;
        let right = engine.get(&self.right).map_err(|error| {
            Issue::new(
                Severity::Error,
                self.right.clone(),
                format!("cannot evaluate rule: {error}"),
            )
        })?;
        match compare_values(left, right) {
            Some(ordering) if self.operator.holds(ordering) => Ok(()),
            Some(_) => Err(Issue::new(
                self.severity,
                self.left.clone(),
                self.message.clone(),
            )),
            None => Err(Issue::new(
                Severity::Error,
                self.left.clone(),
                format!("cannot compare `{}` with `{}`", self.left, self.right),
            )),
        }
    }
}

/// Compares two setting values, widening integers and comparing quantities in
/// canonical units.
pub fn compare_values(left: &SettingValue, right: &SettingValue) -> Option<Ordering> {
    match (left, right) {
        (SettingValue::Bool(left), SettingValue::Bool(right)) => Some(left.cmp(right)),
        (SettingValue::Text(left), SettingValue::Text(right)) => Some(left.cmp(right)),
        (SettingValue::Quantity(left), SettingValue::Quantity(right)) => {
            if left.dimension() != right.dimension() {
                return None;
            }
            left.canonical().partial_cmp(&right.canonical())
        }
        _ => left.as_float()?.partial_cmp(&right.as_float()?),
    }
}

/// One finding of a validation run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Issue {
    severity: Severity,
    key: String,
    message: String,
}

impl Issue {
    /// Builds an issue.
    pub fn new(severity: Severity, key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity,
            key: key.into(),
            message: message.into(),
        }
    }

    /// How serious the issue is.
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// Setting the issue is about.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// What went wrong.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// `severity: key: message`.
    pub fn render(&self) -> String {
        format!("{}: {}: {}", self.severity.as_str(), self.key, self.message)
    }
}

/// The set of rules the engine evaluates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleSet {
    rules: Vec<Rule>,
}

impl RuleSet {
    /// Builds a rule set.
    pub fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    /// The baseline rule set defined by [`BASELINE`].
    pub fn baseline() -> Self {
        Self::new(BASELINE.iter().map(Rule::from_seed).collect())
    }

    /// Iterates over the rules.
    pub fn iter(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter()
    }

    /// Number of rules.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether there are no rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Evaluates every rule against `engine`.
    pub fn evaluate(&self, engine: &Engine) -> ValidationReport {
        let mut report = ValidationReport::default();
        for rule in &self.rules {
            report.add_checked(1);
            if let Err(issue) = rule.evaluate(engine) {
                report.push(issue);
            }
        }
        report
    }
}

/// The result of a validation run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    issues: Vec<Issue>,
    checked: usize,
}

impl ValidationReport {
    /// Records an issue.
    pub fn push(&mut self, issue: Issue) {
        self.issues.push(issue);
    }

    /// Records how many checks were performed.
    pub fn add_checked(&mut self, count: usize) {
        self.checked += count;
    }

    /// Every issue.
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }

    /// Number of checks performed.
    pub fn checked(&self) -> usize {
        self.checked
    }

    /// Number of issues.
    pub fn len(&self) -> usize {
        self.issues.len()
    }

    /// Whether no issue was reported.
    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }

    /// The error level issues.
    pub fn errors(&self) -> Vec<&Issue> {
        self.issues
            .iter()
            .filter(|issue| issue.severity().is_error())
            .collect()
    }

    /// The warning level issues.
    pub fn warnings(&self) -> Vec<&Issue> {
        self.issues
            .iter()
            .filter(|issue| !issue.severity().is_error())
            .collect()
    }

    /// Whether no error level issue was reported.
    pub fn is_ok(&self) -> bool {
        self.errors().is_empty()
    }

    /// Sorts the issues by severity (errors first), then key, then message.
    pub fn sort(&mut self) {
        self.issues.sort_by(|left, right| {
            right
                .severity
                .cmp(&left.severity)
                .then_with(|| left.key.cmp(&right.key))
                .then_with(|| left.message.cmp(&right.message))
        });
    }

    /// One line per issue, plus a summary line.
    pub fn render(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.issues.iter().map(Issue::render).collect();
        if lines.is_empty() {
            lines.push(format!(
                "ok: {}, no issues",
                crate::util::plural(self.checked, "check", "checks")
            ));
        } else {
            lines.push(format!(
                "{} issue(s) after {} checks: {} error(s), {} warning(s)",
                self.issues.len(),
                self.checked,
                self.errors().len(),
                self.warnings().len()
            ));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BASELINE, Issue, RelOp, Rule, RuleSet, Severity, ValidationReport, compare_values,
    };
    use crate::dimension::Dimension;
    use crate::engine::Engine;
    use crate::quantity::Quantity;
    use crate::schema::Schema;
    use crate::spec::{Kind, SettingSpec};
    use crate::value::SettingValue;

    #[test]
    fn baseline_rules_pass_on_defaults() {
        let report = RuleSet::baseline().evaluate(&Engine::with_baseline());
        assert!(report.is_empty(), "defaults must satisfy every rule");
        assert_eq!(report.checked(), BASELINE.len());
        assert_eq!(report.render(), vec!["ok: 6 checks, no issues".to_string()]);
    }

    #[test]
    fn a_violation_carries_the_rule_message() {
        let mut engine = Engine::with_baseline();
        engine
            .set("fluid.flow.maximum", "0.001 m3/s")
            .expect("in range");
        let report = RuleSet::baseline().evaluate(&engine);
        assert!(!report.is_ok());
        assert_eq!(report.errors().len(), 1);
        assert_eq!(report.errors()[0].key(), "fluid.flow.maximum");
        assert_eq!(
            report.errors()[0].render(),
            "error: fluid.flow.maximum: maximum flow must be at least the nominal flow"
        );
    }

    #[test]
    fn warnings_do_not_block() {
        let mut engine = Engine::with_baseline();
        engine.set("io.timeout", "0.5 s").expect("in range");
        let report = RuleSet::baseline().evaluate(&engine);
        assert!(report.is_ok());
        assert_eq!(report.warnings().len(), 1);
        assert_eq!(report.errors().len(), 0);
        assert_eq!(report.warnings()[0].severity(), Severity::Warning);
        assert!(!report.is_empty());
    }

    #[test]
    fn reports_sort_errors_before_warnings() {
        let mut report = ValidationReport::default();
        report.push(Issue::new(Severity::Warning, "z.key", "late warning"));
        report.push(Issue::new(Severity::Error, "b.key", "second error"));
        report.push(Issue::new(Severity::Error, "a.key", "first error"));
        report.sort();
        let order: Vec<&str> = report.issues().iter().map(Issue::key).collect();
        assert_eq!(order, vec!["a.key", "b.key", "z.key"]);
        assert_eq!(report.len(), 3);
        assert!(
            report
                .render()
                .last()
                .is_some_and(|line| line.contains("2 error(s)"))
        );
    }

    #[test]
    fn operators_hold_for_the_expected_orderings() {
        use std::cmp::Ordering;
        assert!(RelOp::Less.holds(Ordering::Less));
        assert!(!RelOp::Less.holds(Ordering::Equal));
        assert!(RelOp::GreaterOrEqual.holds(Ordering::Equal));
        assert!(RelOp::Greater.holds(Ordering::Greater));
        assert!(!RelOp::Greater.holds(Ordering::Equal));
        assert!(RelOp::LessOrEqual.holds(Ordering::Less));
        assert!(!RelOp::LessOrEqual.holds(Ordering::Greater));
    }

    #[test]
    fn values_are_compared_with_widening() {
        let kelvin = Quantity::new(273.15, crate::unit::lookup("K").expect("K is registered"));
        let celsius = Quantity::new(
            0.0,
            crate::unit::lookup("degC").expect("degC is registered"),
        );
        assert_eq!(
            compare_values(
                &SettingValue::Quantity(kelvin),
                &SettingValue::Quantity(celsius)
            ),
            Some(std::cmp::Ordering::Equal)
        );
        assert_eq!(
            compare_values(&SettingValue::Int(3), &SettingValue::Float(3.5)),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            compare_values(&SettingValue::Bool(true), &SettingValue::Int(1)),
            None
        );
        assert_eq!(
            compare_values(&SettingValue::from("a"), &SettingValue::from("b")),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn incomparable_and_missing_values_are_hard_errors() {
        let mut schema = Schema::new();
        for (key, kind, default) in [
            ("flag", Kind::bool(), SettingValue::Bool(true)),
            ("count", Kind::int(0, 10), SettingValue::Int(2)),
            ("other", Kind::int(0, 10), SettingValue::Int(2)),
        ] {
            schema
                .insert(
                    SettingSpec::builder(key, kind)
                        .default_value(default)
                        .build()
                        .expect("valid"),
                )
                .expect("insert succeeds");
        }
        let engine = Engine::new(schema);
        let rules = RuleSet::new(vec![
            Rule::new("flag", RelOp::Greater, "count", Severity::Warning, "broken"),
            Rule::new("count", RelOp::Greater, "gone", Severity::Warning, "broken"),
        ]);
        let report = rules.evaluate(&engine);
        assert_eq!(report.errors().len(), 2);
        assert_eq!(
            report.errors()[0].message(),
            "cannot compare `flag` with `count`"
        );
        assert!(
            report.errors()[1]
                .message()
                .contains("unknown setting `gone`")
        );
    }

    #[test]
    fn rule_accessors_and_baseline_rows() {
        let rule = Rule::from_seed(&(
            "sensor.supply.temperature",
            RelOp::Greater,
            "sensor.return.temperature",
            Severity::Warning,
            "supply must be warmer",
        ));
        assert_eq!(rule.left(), "sensor.supply.temperature");
        assert_eq!(rule.right(), "sensor.return.temperature");
        assert_eq!(rule.operator(), RelOp::Greater);
        assert_eq!(rule.severity(), Severity::Warning);
        assert_eq!(rule.message(), "supply must be warmer");
        let set = RuleSet::baseline();
        assert_eq!(set.len(), 6);
        assert_eq!(set.iter().count(), 6);
        assert!(!set.is_empty());
        let pressure = Quantity::new(1.0, crate::unit::lookup("bar").expect("bar"));
        assert_eq!(pressure.dimension(), Dimension::PRESSURE);
    }
}
