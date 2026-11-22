//! The result of an impact analysis: what a set of changes can affect, why,
//! and which tests exercise it.
//!
//! These types are what the JSON output contains, what the window shows and
//! what the engine process sends across its pipes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Certainty, Confidence, DiffResult, EdgeDetail, EdgeKind, ImpactClass, Location, SymbolPath,
    TargetKind,
};

/// Why the first step of an evidence path reaches its item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// Calls a function whose body changed.
    CallsChanged,
    /// Passes a changed function around as a value.
    TakesChangedAsValue,
    /// Calls a trait method that may dispatch to the changed implementation.
    DispatchesToChanged,
    /// Calls or names a function whose signature changed.
    SignatureChanged,
    /// Names a type or trait whose generics or bounds changed.
    DeclarationChanged,
    /// Refers to an item it can no longer see.
    NoLongerVisible,
    /// Refers to an item that no longer exists.
    RefersToRemoved,
    /// Refers to an item under its previous name or path.
    RefersToRenamed,
    /// Reads, writes or destructures a field that was removed or retyped.
    UsesChangedField,
    /// Removes the only use of one of its container's generic parameters.
    ParameterBecomesUnused,
    /// Builds the struct without the new field.
    ConstructsWithoutField,
    /// Destructures the struct without the new field.
    PatternMissesField,
    /// A `match` without a catch-all arm that does not handle the new variant.
    ExhaustiveMatchMissesVariant,
    /// A `match` whose catch-all arm now also catches the new variant.
    WildcardCatchesNewVariant,
    /// Constructs or matches a variant that was removed or reshaped.
    UsesChangedVariant,
    /// Implements a trait that gained or changed a required item.
    MustImplementTraitItem,
    /// Uses a trait item that changed or was removed.
    UsesChangedTraitItem,
    /// Implements the trait without overriding the changed default method.
    InheritsChangedDefault,
    /// Calls a method of an implementation that was added or removed.
    ImplResolutionChanged,
    /// Uses a constant or static whose value changed.
    UsesChangedValue,
    /// Uses a constant or static whose type changed.
    UsesRetypedValue,
    /// Uses a type alias whose target changed.
    UsesChangedAlias,
    /// Invokes a macro whose definition changed.
    InvokesChangedMacro,
    /// Calls a function whose attributes changed.
    AttributeChanged,
}

/// One step of an evidence path: `from` depends on `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceStep {
    /// The dependent symbol, the one this step reaches.
    pub from: SymbolPath,
    /// The symbol it depends on.
    pub to: SymbolPath,
    /// The kind of the reference.
    pub edge: EdgeKind,
    /// How certain the step is: the edge's certainty, or `Candidate` when
    /// the rule that followed the edge is itself uncertain.
    pub certainty: Certainty,
    /// The detail the edge carries.
    pub detail: EdgeDetail,
    /// Where the reference is written.
    pub site: Location,
}

/// A chain of references leading from a change to an affected item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EvidencePath {
    /// Index into [`DiffResult::changes`] of the change the path starts at.
    pub change: u32,
    /// Why the first step applies.
    pub reason: Reason,
    /// The class this path gives the item: the rule's class when the path
    /// has one step, `Behaviour` when it has more.
    pub class: ImpactClass,
    /// The steps, starting next to the change: `steps[0].to` is the changed
    /// symbol (or its container) and the last step's `from` is the item.
    pub steps: Vec<EvidenceStep>,
    /// The score of this path.
    pub score: f32,
}

/// Facts about an affected item worth showing next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ImpactTags {
    /// The item is a test function.
    pub is_test: bool,
    /// The item is the `main` function of a binary.
    pub is_main: bool,
    /// The item is part of a library's public interface.
    pub is_pub_api: bool,
    /// The item itself is among the changes of this comparison. For an
    /// `Adaptation` item this means it was adapted in the same diff.
    pub changed: bool,
}

/// One item affected by the changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImpactItem {
    /// The affected symbol, by its head-side identity.
    pub symbol: SymbolPath,
    /// Where the symbol is on the head side.
    pub location: Location,
    /// `Adaptation` when any of its paths is one, else `Behaviour`.
    pub class: ImpactClass,
    /// The confidence of the best path of that class.
    pub confidence: Confidence,
    /// The highest score among the paths of that class.
    pub score: f32,
    /// Number of steps of the best path of that class.
    pub distance: u32,
    /// The best paths, at most three, highest score first.
    pub paths: Vec<EvidencePath>,
    /// How many distinct paths reached the item, including the ones not kept.
    pub path_count: u32,
    /// Facts about the item.
    pub tags: ImpactTags,
}

/// Why a test was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TestReason {
    /// The test itself changed or was added.
    Changed,
    /// A change reaches the test.
    Reached,
}

/// A test worth running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SelectedTest {
    /// The test function.
    pub symbol: SymbolPath,
    /// Where it is.
    pub location: Location,
    /// Why it was selected.
    pub reason: TestReason,
    /// The name `cargo test` knows the test by: its path without the crate name.
    pub filter: String,
}

/// One `cargo test` invocation covering selected tests of one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TestCommand {
    /// The Cargo package.
    pub package: String,
    /// The kind of target the tests are in.
    pub target_kind: TargetKind,
    /// The target name.
    pub target_name: String,
    /// Test name filters. Empty when the command runs the whole target:
    /// because every test of it is selected, or because so many are that
    /// naming them would be longer than it is worth.
    pub filters: Vec<String>,
    /// How many selected tests the target holds.
    pub tests: u32,
    /// How many tests the target holds in all.
    pub total: u32,
}

/// The tests affected by the changes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct TestSelection {
    /// The selected tests, ordered by path.
    pub selected: Vec<SelectedTest>,
    /// How many tests the project has.
    pub total: u32,
    /// The commands that run exactly the selected tests, ordered by package
    /// and target.
    pub commands: Vec<TestCommand>,
}

/// What the impact result leaves out or is unsure about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ImpactLimits {
    /// Unresolved references in the head graph.
    pub unresolved_total: u32,
    /// Unresolved references written inside a changed symbol or a listed item.
    pub unresolved_in_reach: u32,
    /// Distinct candidate steps used by the listed paths.
    pub candidate_steps: u32,
    /// First steps taken by a rule that cannot tell exactly who is affected.
    pub coarse_rules: u32,
    /// Items that exist only beyond the depth limit.
    pub beyond_depth: u32,
    /// Items dropped because their score is below the floor.
    pub below_floor: u32,
    /// Items dropped because their confidence is below the requested minimum.
    pub below_confidence: u32,
    /// Items dropped because their file matches an `ignore` pattern.
    pub ignored: u32,
}

/// The weights used to score evidence paths.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RankingParams {
    /// Multiplier applied for each step after the first.
    pub decay: f32,
    /// Weight of a candidate step; a resolved step weighs 1.
    pub candidate_weight: f32,
    /// Weight of the `Adaptation` class.
    pub adaptation_weight: f32,
    /// Weight of the `Behaviour` class.
    pub behaviour_weight: f32,
    /// Items scoring below this are counted, not listed.
    pub score_floor: f32,
}

/// Everything that shapes an impact result besides the code itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnalysisParams {
    /// Scoring weights.
    pub ranking: RankingParams,
    /// Longest evidence path followed.
    pub depth: u32,
    /// Lowest confidence listed.
    pub min_confidence: Confidence,
    /// Glob patterns of files whose items are never listed.
    pub ignore: Vec<String>,
}

/// A complete impact analysis of one comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Analysis {
    /// What was compared and what changed structurally.
    pub diff: DiffResult,
    /// The affected items, best first: by score descending, then distance,
    /// then path.
    pub items: Vec<ImpactItem>,
    /// The affected tests.
    pub tests: TestSelection,
    /// What the result leaves out.
    pub limits: ImpactLimits,
    /// The parameters the result was computed with.
    pub params: AnalysisParams,
}

/// The version of the JSON output format.
pub const SCHEMA_VERSION: u32 = 1;

/// The JSON Schema of the JSON output: an [`Analysis`] object with a leading `schema_version`.
pub fn analysis_schema() -> Value {
    let mut schema = schemars::schema_for!(Analysis).to_value();
    let describes_fields = schema
        .as_object()
        .and_then(|object| object.get("properties"))
        .and_then(Value::as_object)
        .is_some();
    if !describes_fields {
        return schema;
    }
    if let Some(object) = schema.as_object_mut() {
        if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
            properties.insert(
                "schema_version".to_owned(),
                json!({ "type": "integer", "const": SCHEMA_VERSION }),
            );
        }
        if let Some(required) = object.get_mut("required").and_then(Value::as_array_mut) {
            required.push(Value::String("schema_version".to_owned()));
        }
        object.insert(
            "title".to_owned(),
            Value::String("Blastr analysis".to_owned()),
        );
    }
    schema
}

/// A change to assume for a what-if query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Assume {
    /// The body or value changes.
    Body,
    /// The signature or type changes.
    Signature,
    /// The item is removed.
    Removed,
    /// The item becomes private.
    Visibility,
    /// The field is removed.
    FieldRemoved,
    /// The field changes its type.
    FieldRetyped,
    /// The struct gains a field.
    FieldAdded,
    /// The enum gains a variant.
    VariantAdded,
    /// The variant is removed.
    VariantRemoved,
}

impl Assume {
    /// The kebab-case name of the assumed change, as a configuration file writes it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Assume::Body => "body",
            Assume::Signature => "signature",
            Assume::Removed => "removed",
            Assume::Visibility => "visibility",
            Assume::FieldRemoved => "field-removed",
            Assume::FieldRetyped => "field-retyped",
            Assume::FieldAdded => "field-added",
            Assume::VariantAdded => "variant-added",
            Assume::VariantRemoved => "variant-removed",
        }
    }
}

impl Default for RankingParams {
    fn default() -> Self {
        RankingParams {
            decay: 0.6,
            candidate_weight: 0.4,
            adaptation_weight: 1.0,
            behaviour_weight: 0.8,
            score_floor: 0.05,
        }
    }
}

impl Default for AnalysisParams {
    fn default() -> Self {
        AnalysisParams {
            ranking: RankingParams::default(),
            depth: 6,
            min_confidence: Confidence::Possible,
            ignore: Vec::new(),
        }
    }
}

impl TestCommand {
    /// The command line that runs these tests.
    pub fn command_line(&self) -> String {
        let mut line = format!("cargo test -p {}", self.package);
        match self.target_kind {
            TargetKind::Lib | TargetKind::ProcMacro => line.push_str(" --lib"),
            TargetKind::Bin => line.push_str(&format!(" --bin {}", self.target_name)),
            TargetKind::Test => line.push_str(&format!(" --test {}", self.target_name)),
            TargetKind::Example => line.push_str(&format!(" --example {}", self.target_name)),
            TargetKind::Bench => line.push_str(&format!(" --bench {}", self.target_name)),
            TargetKind::BuildScript | TargetKind::Other => {}
        }
        if !self.filters.is_empty() {
            line.push_str(" --");
            for filter in &self.filters {
                line.push(' ');
                line.push_str(filter);
            }
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(kind: TargetKind, filters: &[&str]) -> TestCommand {
        TestCommand {
            package: "billing".to_owned(),
            target_kind: kind,
            target_name: "smoke".to_owned(),
            filters: filters.iter().map(|filter| (*filter).to_owned()).collect(),
            tests: filters.len() as u32,
            total: filters.len() as u32,
        }
    }

    #[test]
    fn library_tests_use_the_lib_flag() {
        let line = command(TargetKind::Lib, &["tax::", "invoice::tests::totals"]).command_line();
        assert_eq!(
            line,
            "cargo test -p billing --lib -- tax:: invoice::tests::totals"
        );
    }

    #[test]
    fn integration_tests_name_their_target() {
        let line = command(TargetKind::Test, &["checkout"]).command_line();
        assert_eq!(line, "cargo test -p billing --test smoke -- checkout");
    }

    #[test]
    fn no_filters_runs_the_whole_target() {
        let line = command(TargetKind::Bin, &[]).command_line();
        assert_eq!(line, "cargo test -p billing --bin smoke");
    }

    #[test]
    fn defaults_are_the_provisional_ones() {
        let params = AnalysisParams::default();
        assert_eq!(params.depth, 6);
        assert_eq!(params.min_confidence, Confidence::Possible);
        assert!(params.ranking.decay > 0.59 && params.ranking.decay < 0.61);
    }
}
