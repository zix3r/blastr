//! Parameter tuning: re-score a stored run's ground truth under many settings.
//!
//! Both checks spend most of their time in Cargo, but Cargo's verdict of a case
//! — the symbols it rejects, the tests it makes fail — does not depend on
//! Blastr's parameters. Only Blastr's own analysis does. This command therefore
//! reads the per-case results of an earlier run as the ground truth, replans the
//! same cases from the projects as they are on disk, and re-runs the impact
//! analysis of every case under every setting of a parameter grid: no Cargo, no
//! compiler, one project load per project.
//!
//! A case pays for its extraction once — the graph of the edited revision does
//! not depend on the parameters — and the impact analysis, which does, runs once
//! per setting on that graph. The stored result also says which targets the
//! compiler built, so the same items are left out of both sides as in the check.
//!
//! The selection rule is the one the plan states: prefer the setting that loses
//! no true item while listing the fewest, then the fewest selected tests, then
//! the setting closest to the current defaults. The whole Pareto front is
//! reported next to the winner, because "no loss" and "fewest items" trade
//! against each other.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::Instant;

use blastr_core::config::IgnoreSet;
use blastr_core::impact::{self, ImpactInput, ImpactOutput};
use blastr_core::{Engine, FileEdit, ProjectBase, Variant, build_id};
use blastr_model::{
    AnalysisOptions, AnalysisParams, CancelToken, Confidence, ImpactClass, NoProgress, RelPath,
    RevisionGraph,
};
use blastr_store::{Paths, Store};
use serde::Serialize;
use serde::ser::SerializeStruct as _;

use crate::edits::EDIT_KINDS;
use crate::plan::{self, PlannedCase, PlannedMutant};
use crate::results::{self, CaseResult};
use crate::{Error, Result, projects_dir, results_dir};

/// Decay values of the default grid.
pub const DECAYS: [f32; 4] = [0.5, 0.6, 0.7, 0.8];
/// Candidate-step weights of the default grid.
pub const CANDIDATE_WEIGHTS: [f32; 4] = [0.3, 0.4, 0.6, 0.8];
/// Score floors of the default grid.
pub const SCORE_FLOORS: [f32; 3] = [0.01, 0.02, 0.05];
/// Depth limits of the default grid.
pub const DEPTHS: [u32; 3] = [6, 8, 10];

/// Cases per extraction round: the graphs held in memory while the workers
/// score them. Small enough to stay in cache, large enough that the hand-off
/// between the extractor and the workers is not the cost.
const CHUNK: usize = 16;

/// What the command line asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// The projects to tune on; empty means `shopfront` and `pipeline`.
    pub projects: Vec<PathBuf>,
    /// The projects to confirm the winner on; they must not be tuning projects.
    pub confirm: Vec<PathBuf>,
    /// The grid, as `key=value,...;key=...`; `None` takes the default grid.
    pub grid: Option<String>,
    /// Where the stored results are read from; `None` takes `validation/results`.
    pub results: Option<PathBuf>,
    /// Where `tuning.json` and `tuning.md` go; `None` takes `validation/results`.
    pub out: Option<PathBuf>,
    /// How many cases per check to score: a smoke test, not a measurement.
    pub limit: Option<usize>,
}

/// One point of the grid: every parameter an analysis runs with.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    /// Longest evidence path followed.
    pub depth: u32,
    /// Multiplier applied for each step after the first.
    pub decay: f32,
    /// Weight of a candidate step; a resolved step weighs 1.
    pub candidate_weight: f32,
    /// Items scoring below this are counted, not listed.
    pub score_floor: f32,
    /// Weight of the `Adaptation` class; not searched.
    pub adaptation_weight: f32,
    /// Weight of the `Behaviour` class; not searched.
    pub behaviour_weight: f32,
    /// Lowest confidence listed; not searched.
    pub min_confidence: Confidence,
    /// Glob patterns of files whose items are never listed; not searched.
    pub ignore: Vec<String>,
}

impl Setting {
    /// The parameters Blastr's checks run with.
    pub fn defaults() -> Setting {
        Setting::from_params(&AnalysisParams::default())
    }

    /// The same parameters as a grid point.
    pub fn from_params(params: &AnalysisParams) -> Setting {
        Setting {
            depth: params.depth,
            decay: params.ranking.decay,
            candidate_weight: params.ranking.candidate_weight,
            score_floor: params.ranking.score_floor,
            adaptation_weight: params.ranking.adaptation_weight,
            behaviour_weight: params.ranking.behaviour_weight,
            min_confidence: params.min_confidence,
            ignore: params.ignore.clone(),
        }
    }

    /// The parameters an analysis of this grid point runs with.
    pub fn params(&self) -> AnalysisParams {
        AnalysisParams {
            ranking: blastr_model::RankingParams {
                decay: self.decay,
                candidate_weight: self.candidate_weight,
                adaptation_weight: self.adaptation_weight,
                behaviour_weight: self.behaviour_weight,
                score_floor: self.score_floor,
            },
            depth: self.depth,
            min_confidence: self.min_confidence,
            ignore: self.ignore.clone(),
        }
    }

    /// The short form a table shows: `depth/decay/candidate/floor`.
    pub fn label(&self) -> String {
        format!(
            "{}/{:.2}/{:.2}/{:.2}",
            self.depth, self.decay, self.candidate_weight, self.score_floor
        )
    }

    /// How far one setting is from another, as the sum of the relative changes
    /// of the four searched parameters. `0` is the same setting.
    pub fn distance(&self, other: &Setting) -> f64 {
        fn relative(left: f64, right: f64) -> f64 {
            (left - right).abs() / left.abs().max(right.abs()).max(f64::EPSILON)
        }
        relative(f64::from(self.depth), f64::from(other.depth))
            + relative(f64::from(self.decay), f64::from(other.decay))
            + relative(
                f64::from(self.candidate_weight),
                f64::from(other.candidate_weight),
            )
            + relative(f64::from(self.score_floor), f64::from(other.score_floor))
    }
}

impl Serialize for Setting {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Setting", 8)?;
        state.serialize_field("depth", &self.depth)?;
        state.serialize_field("decay", &rounded(self.decay))?;
        state.serialize_field("candidate_weight", &rounded(self.candidate_weight))?;
        state.serialize_field("score_floor", &rounded(self.score_floor))?;
        state.serialize_field("adaptation_weight", &rounded(self.adaptation_weight))?;
        state.serialize_field("behaviour_weight", &rounded(self.behaviour_weight))?;
        state.serialize_field("min_confidence", &self.min_confidence)?;
        state.serialize_field("ignore", &self.ignore)?;
        state.end()
    }
}

/// A float as JSON shows it: three decimals are what a parameter has, and
/// `0.06f32` as an `f64` would otherwise print as `0.05999999865889549`.
fn rounded(value: f32) -> f64 {
    (f64::from(value) * 1000.0).round() / 1000.0
}

/// The default grid: decay, score floor and depth crossed in full, plus the
/// candidate weight swept at the default rest.
///
/// The score floor and the depth are the two cut-offs this search is about, and
/// the decay scales every score the floor is compared with, so those three are
/// crossed in full (4 × 3 × 3 = 36 settings); the candidate weight only reaches
/// the scores of candidate steps and is swept at the default floor and depth
/// (3 more). A full four-way cross can be asked for with `--grid`.
pub fn default_grid() -> Vec<Setting> {
    let base = Setting::defaults();
    let mut settings = Vec::new();
    for decay in DECAYS {
        for score_floor in SCORE_FLOORS {
            for depth in DEPTHS {
                settings.push(Setting {
                    depth,
                    decay,
                    score_floor,
                    ..base.clone()
                });
            }
        }
    }
    for candidate_weight in CANDIDATE_WEIGHTS {
        settings.push(Setting {
            candidate_weight,
            ..base.clone()
        });
    }
    normalised(settings)
}

/// Removes the duplicates and puts the settings nearest the defaults first, so
/// that a table reads from the current parameters outwards.
fn normalised(settings: Vec<Setting>) -> Vec<Setting> {
    let defaults = Setting::defaults();
    let mut kept: Vec<Setting> = Vec::with_capacity(settings.len());
    for setting in settings {
        if !kept.contains(&setting) {
            kept.push(setting);
        }
    }
    kept.sort_by(|left, right| {
        let order = left
            .distance(&defaults)
            .partial_cmp(&right.distance(&defaults))
            .unwrap_or(std::cmp::Ordering::Equal);
        order.then_with(|| left.label().cmp(&right.label()))
    });
    kept
}

/// Parses a grid spec: `decay=0.5,0.7;candidate=0.3,0.6;floor=0.02;depth=6,10`.
///
/// The settings are the cartesian product of the values given per parameter; a
/// parameter the spec does not name keeps its default value. `floor` may also be
/// written `score_floor` and `candidate` `candidate_weight`.
pub fn parse_grid(spec: &str) -> Result<Vec<Setting>> {
    let base = Setting::defaults();
    let mut decays = vec![base.decay];
    let mut candidates = vec![base.candidate_weight];
    let mut floors = vec![base.score_floor];
    let mut depths = vec![base.depth];
    let mut named: HashSet<&str> = HashSet::new();

    for part in spec
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let (key, values) = part
            .split_once('=')
            .ok_or_else(|| Error::Harness(format!("`{part}` is not `parameter=value,...`")))?;
        let key = key.trim();
        let canonical = canonical_parameter(key)?;
        if !named.insert(canonical) {
            return Err(Error::Harness(format!("`{key}` is given twice")));
        }
        let given: Vec<&str> = values
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect();
        if given.is_empty() {
            return Err(Error::Harness(format!("`{key}` has no value")));
        }
        match canonical {
            "decay" => {
                decays = given
                    .iter()
                    .map(|value| float(key, value))
                    .collect::<Result<_>>()?
            }
            "candidate" => {
                candidates = given
                    .iter()
                    .map(|value| float(key, value))
                    .collect::<Result<_>>()?;
            }
            "floor" => {
                floors = given
                    .iter()
                    .map(|value| float(key, value))
                    .collect::<Result<_>>()?
            }
            _ => {
                depths = given
                    .iter()
                    .map(|value| {
                        value
                            .parse::<u32>()
                            .map_err(|_| Error::Harness(format!("`{key}={value}` is not a number")))
                    })
                    .collect::<Result<_>>()?;
            }
        }
    }
    if named.is_empty() {
        return Err(Error::Harness(
            "the grid names no parameter; use decay, candidate, floor or depth".to_owned(),
        ));
    }

    let mut settings = Vec::new();
    for decay in &decays {
        for candidate_weight in &candidates {
            for score_floor in &floors {
                for depth in &depths {
                    settings.push(Setting {
                        depth: *depth,
                        decay: *decay,
                        candidate_weight: *candidate_weight,
                        score_floor: *score_floor,
                        ..base.clone()
                    });
                }
            }
        }
    }
    Ok(normalised(settings))
}

/// Parses one setting: `depth=8,decay=0.8,candidate=0.4,floor=0.05`.
///
/// The form is one `parameter=value` pair per searched parameter, separated by
/// commas; a parameter the spec does not name keeps its default value, and a
/// parameter may be named once. The aliases are the grid's (`score_floor` for
/// `floor`, `candidate_weight` for `candidate`).
///
/// A check runs with these parameters instead of the defaults, so a case can be
/// rerun under a tuned setting and the selection it causes can be inspected.
pub fn parse_params(spec: &str) -> Result<AnalysisParams> {
    let mut setting = Setting::defaults();
    let mut named: HashSet<&'static str> = HashSet::new();

    for part in spec
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| Error::Harness(format!("`{part}` is not `parameter=value`")))?;
        let key = key.trim();
        let value = value.trim();
        let canonical = canonical_parameter(key)?;
        if !named.insert(canonical) {
            return Err(Error::Harness(format!("`{key}` is given twice")));
        }
        if value.contains(',') {
            return Err(Error::Harness(format!(
                "`{key}={value}` names more than one value; `--params` takes one per parameter"
            )));
        }
        match canonical {
            "decay" => setting.decay = float(key, value)?,
            "candidate" => setting.candidate_weight = float(key, value)?,
            "floor" => setting.score_floor = float(key, value)?,
            _ => {
                setting.depth = value
                    .parse::<u32>()
                    .map_err(|_| Error::Harness(format!("`{key}={value}` is not a number")))?;
            }
        }
    }
    if named.is_empty() {
        return Err(Error::Harness(
            "`--params` names no parameter; use decay, candidate, floor or depth".to_owned(),
        ));
    }
    Ok(setting.params())
}

/// The name a parameter is known by in a spec.
fn canonical_parameter(key: &str) -> Result<&'static str> {
    match key {
        "decay" => Ok("decay"),
        "candidate" | "candidate_weight" => Ok("candidate"),
        "floor" | "score_floor" => Ok("floor"),
        "depth" => Ok("depth"),
        other => Err(Error::Harness(format!(
            "`{other}` is not a tunable parameter; use decay, candidate, floor or depth"
        ))),
    }
}

/// One value of a grid spec as a float.
fn float(key: &str, value: &str) -> Result<f32> {
    let parsed: f32 = value
        .parse()
        .map_err(|_| Error::Harness(format!("`{key}={value}` is not a number")))?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err(Error::Harness(format!(
            "`{key}={value}` is not a positive number"
        )));
    }
    Ok(parsed)
}

/// Which check a stored case comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// The compiler check: listed symbols against the compiler's errors.
    Compiler,
    /// The test check: selected tests against the failing ones.
    Tests,
}

impl Check {
    /// The name the checks and the reports use.
    pub fn as_str(self) -> &'static str {
        match self {
            Check::Compiler => "compiler",
            Check::Tests => "tests",
        }
    }
}

/// The counts of one check under one setting, over every case scored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    /// Cases scored.
    pub cases: u32,
    /// Entries that had to be found: compiler errors, or failing tests.
    pub expected: u32,
    /// Entries Blastr listed: symbols needing editing, or tests selected.
    pub listed: u32,
    /// Expected entries Blastr did not list.
    pub misses: u32,
    /// Listed symbols the compiler accepted. Always zero for the test check.
    pub false_alarms: u32,
    /// Every item of Blastr's result, whatever its class.
    pub blastr_items: u32,
    /// Tests in the suite, over the scored cases of the test check.
    pub suite_total: u32,
    /// Listed items left out because the compiler did not build their target.
    pub unconfirmed: u32,
    /// Items the analysis counted but did not list: below the score floor.
    pub below_floor: u32,
    /// Items below the requested confidence.
    pub below_confidence: u32,
    /// Items that exist only beyond the depth limit.
    pub beyond_depth: u32,
    /// Items dropped because their file matches an ignore pattern.
    pub ignored: u32,
}

impl Totals {
    /// Adds the counts of another case, worker or project.
    pub fn add(&mut self, other: &Totals) {
        self.cases = self.cases.saturating_add(other.cases);
        self.expected = self.expected.saturating_add(other.expected);
        self.listed = self.listed.saturating_add(other.listed);
        self.misses = self.misses.saturating_add(other.misses);
        self.false_alarms = self.false_alarms.saturating_add(other.false_alarms);
        self.blastr_items = self.blastr_items.saturating_add(other.blastr_items);
        self.suite_total = self.suite_total.saturating_add(other.suite_total);
        self.unconfirmed = self.unconfirmed.saturating_add(other.unconfirmed);
        self.below_floor = self.below_floor.saturating_add(other.below_floor);
        self.below_confidence = self.below_confidence.saturating_add(other.below_confidence);
        self.beyond_depth = self.beyond_depth.saturating_add(other.beyond_depth);
        self.ignored = self.ignored.saturating_add(other.ignored);
    }

    /// The share of the whole suite the selection covers, in per cent.
    pub fn selection_percent(&self) -> f32 {
        if self.suite_total == 0 {
            return 0.0;
        }
        let selected = f64::from(self.listed);
        let total = f64::from(self.suite_total);
        (selected * 100.0 / total) as f32
    }
}

/// The totals of one grid point, over every project and both checks.
#[derive(Debug, Clone, Serialize)]
pub struct SettingTotals {
    /// The grid point.
    pub setting: Setting,
    /// The compiler check.
    pub compiler: Totals,
    /// The test check.
    pub tests: Totals,
}

impl SettingTotals {
    /// An empty row for one grid point.
    pub fn new(setting: Setting) -> SettingTotals {
        SettingTotals {
            setting,
            compiler: Totals::default(),
            tests: Totals::default(),
        }
    }

    /// Adds the row of another project or worker.
    pub fn add(&mut self, other: &SettingTotals) {
        self.compiler.add(&other.compiler);
        self.tests.add(&other.tests);
    }

    /// The row as the selection rule sees it.
    pub fn rank(&self) -> Rank {
        Rank {
            misses: self.compiler.misses.saturating_add(self.tests.misses),
            items: self.compiler.blastr_items,
            selected: self.tests.listed,
            distance: self.setting.distance(&Setting::defaults()),
        }
    }
}

/// What the selection rule compares, smallest first.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rank {
    /// True items lost: misses of the compiler check plus missed failing tests.
    pub misses: u32,
    /// The size of Blastr's list, over the compiler check.
    pub items: u32,
    /// Tests selected, over the test check.
    pub selected: u32,
    /// Relative distance to the current defaults.
    pub distance: f64,
}

impl Rank {
    /// The order of the selection rule: fewest losses first, then the fewest
    /// items, then the fewest selected tests, then the closest to the defaults.
    fn key(&self) -> (u32, u32, u32, f64) {
        (self.misses, self.items, self.selected, self.distance)
    }
}

/// The winner of the grid: the row the selection rule picks.
///
/// Fewest losses first: a row that lists fewer items but loses a true one is not
/// preferred, which is the plan's rule — lose no true item to the cut-offs, then
/// list the fewest.
pub fn select(totals: &[SettingTotals]) -> Option<usize> {
    let mut best: Option<(usize, Rank)> = None;
    for (index, row) in totals.iter().enumerate() {
        let rank = row.rank();
        let better = match &best {
            Some((_, current)) => {
                rank.key().partial_cmp(&current.key()) == Some(std::cmp::Ordering::Less)
            }
            None => true,
        };
        if better {
            best = Some((index, rank));
        }
    }
    best.map(|(index, _)| index)
}

/// The rows no other row beats: every row that is not worse in losses, list size
/// and selected tests at once, with at least one of them strictly better
/// somewhere.
pub fn pareto(totals: &[SettingTotals]) -> Vec<usize> {
    let objectives = |row: &SettingTotals| {
        let rank = row.rank();
        (rank.misses, rank.items, rank.selected)
    };
    let mut front = Vec::new();
    for (index, row) in totals.iter().enumerate() {
        let mine = objectives(row);
        let dominated = totals.iter().enumerate().any(|(other_index, other)| {
            if other_index == index {
                return false;
            }
            let theirs = objectives(other);
            theirs.0 <= mine.0
                && theirs.1 <= mine.1
                && theirs.2 <= mine.2
                && (theirs.0 < mine.0 || theirs.1 < mine.1 || theirs.2 < mine.2)
        });
        if !dominated {
            front.push(index);
        }
    }
    front
}

/// The misses and the false alarms of one case, from what had to be found and
/// what was listed.
///
/// `alarms` is false for the test check: a test Blastr selects that does not
/// fail is not an error, only a cost, which `Totals::listed` records.
pub fn score_case(
    expected: &BTreeSet<String>,
    listed: &BTreeSet<&str>,
    alarms: bool,
) -> (u32, u32) {
    let misses = expected
        .iter()
        .filter(|entry| !listed.contains(entry.as_str()))
        .count();
    let false_alarms = if alarms {
        listed
            .iter()
            .filter(|entry| !expected.contains(**entry))
            .count()
    } else {
        0
    };
    (
        u32::try_from(misses).unwrap_or(u32::MAX),
        u32::try_from(false_alarms).unwrap_or(u32::MAX),
    )
}

/// What a stored case says had to be found.
#[derive(Debug, Clone)]
struct Truth {
    /// The symbols with compiler errors, or the tests that failed.
    expected: BTreeSet<String>,
    /// What Blastr listed in the stored run: everything there was in a target the
    /// compiler built, so an item in no such target stays out of both sides.
    stored_listed: BTreeSet<String>,
    /// Tests in the suite, for the selection share.
    suite_total: u32,
}

/// One case to score: a stored ground truth and the edit to re-apply.
#[derive(Debug, Clone)]
struct Work {
    /// Which check the case belongs to.
    check: Check,
    /// The symbol the case changes; the compiler check excuses it.
    changed: String,
    /// The edit, as an in-memory replacement of one file.
    edit: FileEdit,
    /// What the stored run found.
    truth: Truth,
}

/// One case whose variant has been extracted: the graph the settings run on.
struct Prepared {
    /// Which check the case belongs to.
    check: Check,
    /// The symbol the case changes.
    changed: String,
    /// What the stored run found.
    truth: Truth,
    /// The base graph, the edited graph and the diff between them.
    variant: Variant,
}

/// A grid point with its ignore set compiled, ready to run.
struct Ready {
    /// The parameters.
    params: AnalysisParams,
    /// The ignore patterns, compiled once per run.
    ignore: IgnoreSet,
}

/// How many stored cases a check contributed, and why some did not.
#[derive(Debug, Clone, Default, Serialize)]
struct Coverage {
    /// Stored cases the new plan gives back, with the same edit.
    matched: u32,
    /// Stored cases the new plan no longer produces.
    not_planned: u32,
    /// Cases the new plan produces that the stored run did not score.
    no_ground_truth: u32,
    /// Matched cases whose analysis failed while tuning.
    blastr_failed: u32,
    /// Matched cases a `--limit` left out.
    sampled_out: u32,
    /// Misses the stored run reports over the matched cases.
    stored_misses: u32,
    /// False alarms the stored run reports over the matched cases.
    stored_false_alarms: u32,
    /// The stored run's list size over the matched cases.
    stored_blastr_items: u32,
    /// The stored run's listed entries over the matched cases.
    stored_listed: u32,
}

/// What one project contributed.
#[derive(Debug, Clone, Serialize)]
struct ProjectInfo {
    /// The project, by directory name.
    project: String,
    /// The git revision of the tree the project sits in.
    revision: String,
    /// The Blastr version the stored results came from.
    stored_version: String,
    /// The revision the stored results were produced at.
    stored_revision: String,
    /// True when the stored run used parameters other than the defaults.
    stored_params_differ: bool,
    /// How long loading the project took, in milliseconds.
    load_ms: u64,
    /// How long extracting the variants of the compiler cases took, in
    /// milliseconds.
    compiler_extract_ms: u64,
    /// How long the impact analyses of the compiler cases took, in total, in
    /// microseconds.
    compiler_impact_us: u64,
    /// The same for the extraction of the test cases.
    tests_extract_ms: u64,
    /// The same for the impact analyses of the test cases.
    tests_impact_us: u64,
    /// The number of worker threads.
    threads: usize,
    /// The compiler cases.
    compiler: Coverage,
    /// The test cases.
    tests: Coverage,
}

/// Everything one project evaluation produced.
struct Evaluation {
    /// The row of every setting, in grid order.
    totals: Vec<SettingTotals>,
    /// What the project contributed.
    info: ProjectInfo,
}

/// Runs the tuning sweep and writes `tuning.json` and `tuning.md`.
pub fn run(options: &Options) -> Result<()> {
    let stored_dir = options.results.clone().unwrap_or_else(results_dir);
    let out_dir = options.out.clone().unwrap_or_else(results_dir);
    let (settings, source) = match &options.grid {
        Some(spec) => (parse_grid(spec)?, format!("explicit: {spec}")),
        None => (default_grid(), "default".to_owned()),
    };
    if settings.is_empty() {
        return Err(Error::Harness("the grid has no setting".to_owned()));
    }
    let projects = resolve(&options.projects, &["shopfront", "pipeline"])?;
    let confirm = resolve(&options.confirm, &[])?;
    for held_out in &confirm {
        if projects.contains(held_out) {
            return Err(Error::Harness(format!(
                "`{}` is both a tuning project and a confirmation project",
                name_of(held_out)
            )));
        }
    }

    println!(
        "tune: {} setting(s) ({source}) on {}, reading {}",
        settings.len(),
        names_of(&projects),
        stored_dir.display()
    );

    let mut totals: Vec<SettingTotals> = settings.iter().cloned().map(SettingTotals::new).collect();
    let mut infos = Vec::new();
    let mut revision = String::new();
    for dir in &projects {
        let evaluation = evaluate(dir, &settings, &stored_dir, options.limit)?;
        for (index, row) in evaluation.totals.iter().enumerate() {
            totals[index].add(row);
        }
        if revision.is_empty() {
            revision = evaluation.info.revision.clone();
        }
        println!(
            "{}: {} compiler case(s), {} test case(s) scored",
            evaluation.info.project,
            evaluation.totals[0].compiler.cases,
            evaluation.totals[0].tests.cases
        );
        infos.push(evaluation.info);
    }

    let winner = select(&totals);
    let front = pareto(&totals);
    let defaults = Setting::defaults();
    let defaults_index = totals.iter().position(|row| row.setting == defaults);

    let mut confirmations = Vec::new();
    if let Some(winner_index) = winner {
        let chosen = totals[winner_index].setting.clone();
        for dir in &confirm {
            let evaluation = evaluate(
                dir,
                &[chosen.clone(), defaults.clone()],
                &stored_dir,
                options.limit,
            )?;
            println!(
                "{}: winner loses {} true item(s), the defaults lose {}",
                evaluation.info.project,
                evaluation.totals[0].rank().misses,
                evaluation.totals[1].rank().misses
            );
            confirmations.push(Confirmation {
                project: evaluation.info.project.clone(),
                revision: evaluation.info.revision.clone(),
                rows: vec![
                    Verdict::of(&chosen, &evaluation.totals[0]),
                    Verdict::of(&defaults, &evaluation.totals[1]),
                ],
            });
        }
    }

    let notes = notes(
        &infos,
        &totals,
        defaults_index,
        &confirmations,
        options.limit,
    );
    let report = TuningReport {
        blastr_version: blastr_core::VERSION.to_owned(),
        blastr_build: build_id().to_owned(),
        rust_analyzer: blastr_core::RUST_ANALYZER_VERSION.to_owned(),
        revision,
        stored_dir: stored_dir.display().to_string(),
        limit: options.limit,
        grid: GridInfo {
            source,
            settings: settings.clone(),
        },
        projects: infos,
        settings: totals.clone(),
        pareto: front,
        default_index: defaults_index,
        winner: winner.map(|index| Verdict::of(&totals[index].setting, &totals[index])),
        confirmation: confirmations,
        notes,
    };

    std::fs::create_dir_all(&out_dir)?;
    let json = out_dir.join("tuning.json");
    let text = serde_json::to_string_pretty(&report)
        .map_err(|error| Error::Harness(format!("the tuning report cannot be written: {error}")))?;
    std::fs::write(&json, format!("{text}\n"))?;
    println!("written to {}", json.display());
    let markdown_path = out_dir.join("tuning.md");
    std::fs::write(&markdown_path, markdown(&report))?;
    println!("written to {}", markdown_path.display());

    if let Some(index) = winner {
        let rank = totals[index].rank();
        println!(
            "\nwinner: {} ({} miss(es), {} item(s), {} selected test(s))",
            totals[index].setting.label(),
            rank.misses,
            rank.items,
            rank.selected
        );
    }
    Ok(())
}

/// One row of the report: a grid point and its totals.
#[derive(Debug, Clone, Serialize)]
struct Verdict {
    /// The grid point.
    setting: Setting,
    /// The compiler check.
    compiler: Totals,
    /// The test check.
    tests: Totals,
    /// What the selection rule compares.
    rank: Rank,
}

impl Verdict {
    /// The report row of one setting.
    fn of(setting: &Setting, row: &SettingTotals) -> Verdict {
        Verdict {
            setting: setting.clone(),
            compiler: row.compiler.clone(),
            tests: row.tests.clone(),
            rank: row.rank(),
        }
    }
}

/// A confirmation run of the winner and the defaults on a held-out project.
#[derive(Debug, Clone, Serialize)]
struct Confirmation {
    /// The held-out project.
    project: String,
    /// The git revision of the tree it sits in.
    revision: String,
    /// The winner first, the current defaults second.
    rows: Vec<Verdict>,
}

/// The grid of one run.
#[derive(Debug, Clone, Serialize)]
struct GridInfo {
    /// `default` or the spec given on the command line.
    source: String,
    /// The settings, nearest the defaults first.
    settings: Vec<Setting>,
}

/// The whole tuning run, as `tuning.json` holds it.
#[derive(Debug, Clone, Serialize)]
struct TuningReport {
    blastr_version: String,
    blastr_build: String,
    rust_analyzer: String,
    revision: String,
    stored_dir: String,
    /// How many cases per check a smoke run scored.
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    grid: GridInfo,
    projects: Vec<ProjectInfo>,
    settings: Vec<SettingTotals>,
    pareto: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    winner: Option<Verdict>,
    confirmation: Vec<Confirmation>,
    notes: Vec<String>,
}

/// Whether two parameter sets are the same point of the grid.
fn same_params(left: &AnalysisParams, right: &AnalysisParams) -> bool {
    left.ranking.decay == right.ranking.decay
        && left.ranking.candidate_weight == right.ranking.candidate_weight
        && left.ranking.score_floor == right.ranking.score_floor
        && left.ranking.adaptation_weight == right.ranking.adaptation_weight
        && left.ranking.behaviour_weight == right.ranking.behaviour_weight
        && left.depth == right.depth
        && left.min_confidence == right.min_confidence
        && left.ignore == right.ignore
}

/// Evaluates every setting on one project.
fn evaluate(
    dir: &Path,
    settings: &[Setting],
    stored_dir: &Path,
    limit: Option<usize>,
) -> Result<Evaluation> {
    let dir = dir
        .canonicalize()
        .map_err(|error| format!("`{}` cannot be read: {error}", dir.display()))?;
    let project = name_of(&dir);
    let compiler = results::read(&stored_dir.join(format!("{project}-compiler.json")))
        .map_err(|error| Error::Harness(format!("the compiler results: {error}")))?;
    let tests = results::read(&stored_dir.join(format!("{project}-tests.json")))
        .map_err(|error| Error::Harness(format!("the test results: {error}")))?;

    let mut loaded = Loaded::open(&dir)?;
    let load_ms = loaded.load_ms;
    let graph = loaded.base.graph().clone();
    let (planned, _) = plan::compiler_cases(&graph, &dir, &EDIT_KINDS)?;
    let (mutants, _) = plan::mutant_cases(&graph, &dir)?;

    let mut compiler_coverage = Coverage::default();
    let mut compiler_work = compiler_work(&planned, &compiler, limit, &mut compiler_coverage);
    let mut tests_coverage = Coverage::default();
    let mut tests_work = mutants_work(&mutants, &tests, limit, &mut tests_coverage);
    let mut work = Vec::with_capacity(compiler_work.len() + tests_work.len());
    work.append(&mut compiler_work);
    work.append(&mut tests_work);

    let mut totals: Vec<SettingTotals> = settings.iter().cloned().map(SettingTotals::new).collect();
    let run = run_cases(&mut loaded, work, settings, &mut totals)?;
    compiler_coverage.blastr_failed = run.compiler_failed;
    tests_coverage.blastr_failed = run.tests_failed;

    let info = ProjectInfo {
        project,
        revision: crate::project::revision_of(&dir),
        stored_version: compiler.blastr_version.clone(),
        stored_revision: compiler.revision.clone(),
        stored_params_differ: !same_params(&compiler.params, &AnalysisParams::default()),
        load_ms,
        compiler_extract_ms: run.compiler_extract_ms,
        compiler_impact_us: run.compiler_impact_us,
        tests_extract_ms: run.tests_extract_ms,
        tests_impact_us: run.tests_impact_us,
        threads: run.threads,
        compiler: compiler_coverage,
        tests: tests_coverage,
    };
    Ok(Evaluation { totals, info })
}

/// The compiler cases of a project, matched with the stored ground truth.
fn compiler_work(
    planned: &[PlannedCase],
    stored: &results::CheckReport,
    limit: Option<usize>,
    coverage: &mut Coverage,
) -> Vec<Work> {
    let scored = scored_cases(stored);
    coverage.not_planned = unplanned(stored, planned.iter().map(|case| case.id.as_str()));

    let mut work = Vec::new();
    for case in planned {
        let Some(result) = scored.get(case.id.as_str()) else {
            coverage.no_ground_truth = coverage.no_ground_truth.saturating_add(1);
            continue;
        };
        if let Some(limit) = limit
            && work.len() >= limit
        {
            coverage.sampled_out = coverage.sampled_out.saturating_add(1);
            continue;
        }
        coverage.matched = coverage.matched.saturating_add(1);
        count_stored(coverage, result);
        work.push(Work {
            check: Check::Compiler,
            changed: case.symbol.clone(),
            edit: FileEdit {
                path: case.file.clone(),
                content: Some(case.text.clone()),
            },
            truth: Truth {
                expected: result.expected.iter().cloned().collect(),
                stored_listed: result.listed.iter().cloned().collect(),
                suite_total: 0,
            },
        });
    }
    work
}

/// The mutant cases of a project, matched with the stored ground truth.
fn mutants_work(
    planned: &[PlannedMutant],
    stored: &results::CheckReport,
    limit: Option<usize>,
    coverage: &mut Coverage,
) -> Vec<Work> {
    let scored = scored_cases(stored);
    coverage.not_planned = unplanned(stored, planned.iter().map(|case| case.id.as_str()));

    let mut work = Vec::new();
    for case in planned {
        let Some(result) = scored.get(case.id.as_str()) else {
            coverage.no_ground_truth = coverage.no_ground_truth.saturating_add(1);
            continue;
        };
        if let Some(limit) = limit
            && work.len() >= limit
        {
            coverage.sampled_out = coverage.sampled_out.saturating_add(1);
            continue;
        }
        coverage.matched = coverage.matched.saturating_add(1);
        count_stored(coverage, result);
        work.push(Work {
            check: Check::Tests,
            changed: case.symbol.clone(),
            edit: FileEdit {
                path: case.file.clone(),
                content: Some(case.text.clone()),
            },
            truth: Truth {
                expected: result.expected.iter().cloned().collect(),
                stored_listed: result.listed.iter().cloned().collect(),
                suite_total: result.suite_total,
            },
        });
    }
    work
}

/// The scored cases of a stored report, by id.
fn scored_cases(stored: &results::CheckReport) -> HashMap<&str, &CaseResult> {
    stored
        .cases
        .iter()
        .filter(|case| case.scored())
        .map(|case| (case.id.as_str(), case))
        .collect()
}

/// How many stored cases the new plan does not produce.
fn unplanned<'a>(stored: &results::CheckReport, planned: impl Iterator<Item = &'a str>) -> u32 {
    let planned: HashSet<&str> = planned.collect();
    let count = stored
        .cases
        .iter()
        .filter(|case| !planned.contains(case.id.as_str()))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Adds what the stored run found for one matched case.
fn count_stored(coverage: &mut Coverage, result: &CaseResult) {
    coverage.stored_misses = coverage
        .stored_misses
        .saturating_add(u32::try_from(result.misses.len()).unwrap_or(u32::MAX));
    coverage.stored_false_alarms = coverage
        .stored_false_alarms
        .saturating_add(u32::try_from(result.false_alarms.len()).unwrap_or(u32::MAX));
    coverage.stored_blastr_items = coverage
        .stored_blastr_items
        .saturating_add(result.blastr_items);
    coverage.stored_listed = coverage
        .stored_listed
        .saturating_add(u32::try_from(result.listed.len()).unwrap_or(u32::MAX));
}

/// A project loaded for analysis: no Cargo copy, no target directory.
struct Loaded {
    /// The loaded project, which extracts the variants.
    base: ProjectBase,
    /// How long loading and extracting the base graph took, in milliseconds.
    load_ms: u64,
    /// Holds the cache the engine uses.
    _scratch: tempfile::TempDir,
}

impl Loaded {
    /// Loads the project in `dir`.
    fn open(dir: &Path) -> Result<Loaded> {
        let scratch = tempfile::tempdir()?;
        let cache = scratch.path().join("cache");
        std::fs::create_dir_all(&cache)?;
        let paths = Paths {
            cache_dir: cache.clone(),
            config_dir: cache.join("config"),
        };
        let store = Store::open_in_memory()?;
        let engine = Engine::with_store_and_paths(store, paths);
        let cancel = CancelToken::new();
        let started = Instant::now();
        let base = engine.project_base(dir, &AnalysisOptions::default(), &NoProgress, &cancel)?;
        let load_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(Loaded {
            base,
            load_ms,
            _scratch: scratch,
        })
    }
}

/// What one run over the cases of a project cost and how many cases failed.
#[derive(Debug, Clone, Copy, Default)]
struct Cost {
    /// Cases per check whose extraction failed.
    compiler_failed: u32,
    /// The same for the test check.
    tests_failed: u32,
    /// Extraction of the edited variants, compiler cases, in milliseconds.
    compiler_extract_ms: u64,
    /// Impact analyses of the compiler cases, in total, in microseconds.
    compiler_impact_us: u64,
    /// Extraction of the edited variants of the test cases, in milliseconds.
    tests_extract_ms: u64,
    /// Impact analyses of the test cases, in total, in microseconds.
    tests_impact_us: u64,
    /// Worker threads used.
    threads: usize,
}

/// Extracts every case once and scores it under every setting.
///
/// Extraction is sequential — one project, one workspace, one in-memory file
/// system — and hands its chunks to worker threads that run the impact analysis
/// of every setting on each extracted graph. One case's graph does not depend on
/// the parameters, so it is paid for once; the workers never touch the workspace,
/// and the extractor never waits for them once a chunk is handed over.
fn run_cases(
    loaded: &mut Loaded,
    work: Vec<Work>,
    settings: &[Setting],
    totals: &mut [SettingTotals],
) -> Result<Cost> {
    if work.is_empty() {
        return Ok(Cost::default());
    }
    let ready: Vec<Ready> = settings
        .iter()
        .map(|setting| {
            Ok(Ready {
                params: setting.params(),
                ignore: IgnoreSet::new(&setting.ignore).map_err(|error| {
                    Error::Harness(format!("an ignore pattern is not usable: {error}"))
                })?,
            })
        })
        .collect::<Result<_>>()?;

    let threads = workers().min(work.len());
    let (sender, receiver) = mpsc::sync_channel::<Vec<Prepared>>(2);
    let receiver = Mutex::new(receiver);
    let collected: Mutex<Vec<(Vec<SettingTotals>, Cost)>> = Mutex::new(Vec::new());
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let receiver = &receiver;
    let collected = &collected;
    let failures = &failures;
    let ready = &ready;

    std::thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            handles.push(scope.spawn(move || -> Result<()> {
                let mut local: Vec<SettingTotals> =
                    settings.iter().cloned().map(SettingTotals::new).collect();
                let mut cost = Cost::default();
                let cancel = CancelToken::new();
                loop {
                    let chunk = {
                        let queue = receiver
                            .lock()
                            .map_err(|_| Error::Harness("the case queue is poisoned".to_owned()))?;
                        queue.recv()
                    };
                    let Ok(chunk) = chunk else {
                        break;
                    };
                    for prepared in chunk {
                        let index = root_index(&prepared.variant.head);
                        let judged = judged_roots(&index, &prepared.truth, &prepared.changed);
                        for (row, ready) in local.iter_mut().zip(ready) {
                            let started = Instant::now();
                            let output = impact::analyze(
                                &ImpactInput {
                                    base: &prepared.variant.base,
                                    head: &prepared.variant.head,
                                    changes: &prepared.variant.analysis.diff.changes,
                                    params: &ready.params,
                                    ignore: &ready.ignore,
                                },
                                &cancel,
                            )
                            .map_err(|error| {
                                Error::Harness(format!("the impact analysis failed: {error}"))
                            })?;
                            let micros =
                                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                            match prepared.check {
                                Check::Compiler => {
                                    cost.compiler_impact_us += micros;
                                    score_compiler(&prepared, &output, &index, &judged, row);
                                }
                                Check::Tests => {
                                    cost.tests_impact_us += micros;
                                    score_tests(&prepared, &output, row);
                                }
                            }
                        }
                    }
                }
                let mut collected = collected
                    .lock()
                    .map_err(|_| Error::Harness("the case results are poisoned".to_owned()))?;
                collected.push((local, cost));
                Ok(())
            }));
        }

        let mut cost = Cost {
            threads,
            ..Cost::default()
        };
        let cancel = CancelToken::new();
        for chunk in work.chunks(CHUNK) {
            let mut prepared = Vec::with_capacity(chunk.len());
            for case in chunk {
                let started = Instant::now();
                let extracted = loaded.base.variant(
                    std::slice::from_ref(&case.edit),
                    &AnalysisParams::default(),
                    &NoProgress,
                    &cancel,
                );
                let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let failed = extracted.is_err();
                match (case.check, failed) {
                    (Check::Compiler, _) => cost.compiler_extract_ms += millis,
                    (Check::Tests, _) => cost.tests_extract_ms += millis,
                }
                match extracted {
                    Ok(variant) => prepared.push(Prepared {
                        check: case.check,
                        changed: case.changed.clone(),
                        truth: case.truth.clone(),
                        variant,
                    }),
                    Err(error) => {
                        match case.check {
                            Check::Compiler => cost.compiler_failed += 1,
                            Check::Tests => cost.tests_failed += 1,
                        }
                        if let Ok(mut notes) = failures.lock()
                            && notes.len() < 3
                        {
                            notes.push(format!(
                                "{} {}: {error}",
                                case.check.as_str(),
                                case.changed
                            ));
                        }
                    }
                }
            }
            if !prepared.is_empty() && sender.send(prepared).is_err() {
                break;
            }
        }
        drop(sender);
        for handle in handles {
            match handle.join() {
                Ok(result) => result?,
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        collected
            .lock()
            .map_err(|_| Error::Harness("the case results are poisoned".to_owned()))?
            .push((Vec::new(), cost));
        Ok(())
    })?;

    consume(totals, collected, failures, threads)
}

/// The costs and failures of one run over the cases of a project.
fn consume(
    totals: &mut [SettingTotals],
    collected: &Mutex<Vec<(Vec<SettingTotals>, Cost)>>,
    failures: &Mutex<Vec<String>>,
    threads: usize,
) -> Result<Cost> {
    let mut cost = Cost::default();
    let mut gathered = collected
        .lock()
        .map_err(|_| Error::Harness("the case results are poisoned".to_owned()))?;
    for (local, worker) in std::mem::take(&mut *gathered) {
        for (index, row) in local.iter().enumerate() {
            totals[index].add(row);
        }
        cost.threads = cost.threads.max(worker.threads);
        cost.compiler_failed += worker.compiler_failed;
        cost.tests_failed += worker.tests_failed;
        cost.compiler_extract_ms += worker.compiler_extract_ms;
        cost.compiler_impact_us += worker.compiler_impact_us;
        cost.tests_extract_ms += worker.tests_extract_ms;
        cost.tests_impact_us += worker.tests_impact_us;
    }
    if cost.compiler_failed + cost.tests_failed > 0 {
        let mut reasons = failures
            .lock()
            .map_err(|_| Error::Harness("the case notes are poisoned".to_owned()))?;
        eprintln!(
            "{threads} case(s) could not be analysed and are not scored: {}",
            std::mem::take(&mut *reasons).join("; ")
        );
    }
    Ok(cost)
}

/// Outer paths to the crate roots that hold them, for the judged-target rule.
type RootIndex<'a> = HashMap<&'a str, Vec<RelPath>>;

/// Which crate roots hold a symbol of the edited graph.
fn root_index(graph: &RevisionGraph) -> RootIndex<'_> {
    let mut index: RootIndex<'_> = HashMap::new();
    for symbol in &graph.symbols {
        let Some(info) = graph.crates.get(symbol.krate.0 as usize) else {
            continue;
        };
        index
            .entry(symbol.path.path.as_str())
            .or_default()
            .push(info.root_file.clone());
    }
    index
}

/// The targets the stored run confirmed: the crate roots of everything it
/// expected or listed. The compiler built those, so an item in one of them is
/// judged; an item anywhere else stays out of both sides, as in the checks.
fn judged_roots(index: &RootIndex<'_>, truth: &Truth, changed: &str) -> BTreeSet<RelPath> {
    let mut roots = BTreeSet::new();
    let paths = truth
        .expected
        .iter()
        .chain(truth.stored_listed.iter())
        .map(String::as_str)
        .chain(std::iter::once(changed));
    for path in paths {
        if let Some(found) = index.get(path) {
            roots.extend(found.iter().cloned());
        }
    }
    roots
}

/// Whether the compiler judged an item of that path: true for an item in a
/// confirmed target, and for one whose crate the graph does not name.
fn judged(index: &RootIndex<'_>, roots: &BTreeSet<RelPath>, path: &str) -> bool {
    match index.get(path) {
        Some(found) => found.is_empty() || found.iter().any(|root| roots.contains(root)),
        None => true,
    }
}

/// The compiler check's arithmetic for one case and one setting.
fn score_compiler(
    prepared: &Prepared,
    output: &ImpactOutput,
    index: &RootIndex<'_>,
    roots: &BTreeSet<RelPath>,
    into: &mut SettingTotals,
) {
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    let mut unconfirmed = 0_u32;
    for item in &output.items {
        if item.class != ImpactClass::Adaptation {
            continue;
        }
        let path = item.symbol.path.as_str();
        // The changed definition is excused: it may break inside itself.
        if path == prepared.changed {
            continue;
        }
        if judged(index, roots, path) {
            listed.insert(path);
        } else {
            unconfirmed = unconfirmed.saturating_add(1);
        }
    }
    let (misses, false_alarms) = score_case(&prepared.truth.expected, &listed, true);
    let totals = &mut into.compiler;
    totals.cases = totals.cases.saturating_add(1);
    totals.expected = totals
        .expected
        .saturating_add(u32::try_from(prepared.truth.expected.len()).unwrap_or(u32::MAX));
    totals.listed = totals
        .listed
        .saturating_add(u32::try_from(listed.len()).unwrap_or(u32::MAX));
    totals.misses = totals.misses.saturating_add(misses);
    totals.false_alarms = totals.false_alarms.saturating_add(false_alarms);
    totals.blastr_items = totals
        .blastr_items
        .saturating_add(u32::try_from(output.items.len()).unwrap_or(u32::MAX));
    totals.unconfirmed = totals.unconfirmed.saturating_add(unconfirmed);
    count_limits(output, totals);
}

/// The test check's arithmetic for one case and one setting.
fn score_tests(prepared: &Prepared, output: &ImpactOutput, into: &mut SettingTotals) {
    let selected: Vec<&str> = output
        .tests
        .selected
        .iter()
        .map(|test| test.filter.as_str())
        .collect();
    let unique: BTreeSet<&str> = selected.iter().copied().collect();
    let (misses, _) = score_case(&prepared.truth.expected, &unique, false);
    let totals = &mut into.tests;
    totals.cases = totals.cases.saturating_add(1);
    totals.expected = totals
        .expected
        .saturating_add(u32::try_from(prepared.truth.expected.len()).unwrap_or(u32::MAX));
    totals.listed = totals
        .listed
        .saturating_add(u32::try_from(selected.len()).unwrap_or(u32::MAX));
    totals.misses = totals.misses.saturating_add(misses);
    totals.blastr_items = totals
        .blastr_items
        .saturating_add(u32::try_from(output.items.len()).unwrap_or(u32::MAX));
    totals.suite_total = totals
        .suite_total
        .saturating_add(prepared.truth.suite_total);
    count_limits(output, totals);
}

/// Adds what the analysis says it dropped at its cut-offs.
fn count_limits(output: &ImpactOutput, into: &mut Totals) {
    into.below_floor = into.below_floor.saturating_add(output.limits.below_floor);
    into.below_confidence = into
        .below_confidence
        .saturating_add(output.limits.below_confidence);
    into.beyond_depth = into.beyond_depth.saturating_add(output.limits.beyond_depth);
    into.ignored = into.ignored.saturating_add(output.limits.ignored);
}

/// How many worker threads to use.
fn workers() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4)
        .clamp(1, 16)
}

/// Resolves project names against `validation/projects`.
fn resolve(given: &[PathBuf], default: &[&str]) -> Result<Vec<PathBuf>> {
    let names: Vec<PathBuf> = if given.is_empty() {
        default.iter().map(PathBuf::from).collect()
    } else {
        given.to_vec()
    };
    let mut resolved = Vec::with_capacity(names.len());
    for name in names {
        let candidate = if name.exists() {
            name.clone()
        } else {
            projects_dir().join(&name)
        };
        if !candidate.join("Cargo.toml").is_file() {
            return Err(Error::Harness(format!(
                "`{}` is not a Cargo project",
                candidate.display()
            )));
        }
        resolved.push(candidate);
    }
    Ok(resolved)
}

/// The directory name of a project.
fn name_of(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// The directory names of projects, for a line of output.
fn names_of(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|dir| name_of(dir))
        .collect::<Vec<String>>()
        .join(", ")
}

/// What a report should say about the runs behind it.
fn notes(
    infos: &[ProjectInfo],
    totals: &[SettingTotals],
    defaults_index: Option<usize>,
    confirmations: &[Confirmation],
    limit: Option<usize>,
) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(limit) = limit {
        notes.push(format!(
            "this is a smoke run: at most {limit} case(s) per check and project were scored, so it says nothing about accuracy"
        ));
    }
    let matched: u32 = infos
        .iter()
        .map(|info| info.compiler.matched + info.tests.matched)
        .sum();
    let unplanned: u32 = infos
        .iter()
        .map(|info| info.compiler.not_planned + info.tests.not_planned)
        .sum();
    let untruth: u32 = infos
        .iter()
        .map(|info| info.compiler.no_ground_truth + info.tests.no_ground_truth)
        .sum();
    let failed: u32 = infos
        .iter()
        .map(|info| info.compiler.blastr_failed + info.tests.blastr_failed)
        .sum();
    notes.push(format!(
        "{matched} stored case(s) re-scored, {failed} of them without a result because the analysis failed; {unplanned} stored case(s) are no longer planned for their project, and {untruth} planned case(s) have no scored result in the stored run, so all of those stay out"
    ));
    if infos.iter().any(|info| info.stored_params_differ) {
        notes.push(
            "the stored run used parameters other than the current defaults, so its rows are not a reproduction of it".to_owned(),
        );
    }
    if let Some(index) = defaults_index
        && let Some(row) = totals.get(index)
    {
        let stored_misses: u32 = infos
            .iter()
            .map(|info| info.compiler.stored_misses + info.tests.stored_misses)
            .sum();
        let stored_alarms: u32 = infos
            .iter()
            .map(|info| info.compiler.stored_false_alarms)
            .sum();
        let stored_items: u32 = infos
            .iter()
            .map(|info| info.compiler.stored_blastr_items)
            .sum();
        notes.push(format!(
            "at the defaults this run scores {} miss(es), {} false alarm(s) and {} item(s) where the stored run reports {stored_misses}, {stored_alarms} and {stored_items} over the same cases; a difference comes from the Blastr build the stored results are from, not from the sweep",
            row.compiler.misses.saturating_add(row.tests.misses),
            row.compiler.false_alarms,
            row.compiler.blastr_items
        ));
    }
    notes.push(
        "the compiler's verdict is read from the stored run and only Blastr's analysis is recomputed; the stored run's excluded and unconfirmed items stay excluded, which is why a new item in a target the stored run never built is counted as unconfirmed instead of as a false alarm".to_owned(),
    );
    notes.push(
        "the rename threshold is `blastr_core::diff::MatchParams::rename_similarity` (0.7). It is not tuned here: `ProjectBase::variant` compares with `MatchParams::default()` and no edit kind the harness plans renames a symbol, so no stored case exercises it".to_owned(),
    );
    notes.push(
        "the class weights and the minimum confidence stay at their defaults: `possible` is already the widest confidence, and a class weight only scales the score that the floor is compared with, which is what the decay, candidate weight and floor sweep covers".to_owned(),
    );
    if confirmations.is_empty() {
        notes.push(
            "no held-out project: pass `--confirm macrokit` to check the winner on a project the grid did not see".to_owned(),
        );
    }
    notes
}

/// The Markdown report of a tuning run.
fn markdown(report: &TuningReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Blastr parameter tuning\n");
    let _ = writeln!(
        out,
        "Blastr {} (build `{}`), rust-analyzer {}. Tree `{}`. Ground truth read from `{}`.\n",
        report.blastr_version,
        report.blastr_build,
        report.rust_analyzer,
        report.revision,
        report.stored_dir
    );
    let _ = writeln!(
        out,
        "Both checks spend most of their time in Cargo, and Cargo's verdict of a case — which symbols it rejects, which tests it makes fail — does not depend on Blastr's parameters. This run reuses the stored per-case results as the ground truth and recomputes only Blastr's analysis: one extraction per case, then the impact analysis once per grid point, on the project loaded once.\n"
    );
    let _ = writeln!(
        out,
        "## Grid\n\n{} setting(s), {}.\n",
        report.grid.settings.len(),
        match report.grid.source.as_str() {
            "default" => "the default grid: decay, score floor and depth crossed in full, plus the candidate weight swept at the default rest".to_owned(),
            other => format!("`{other}`"),
        }
    );
    let _ = writeln!(
        out,
        "| depth | decay | candidate weight | score floor | adaptation weight | behaviour weight | min confidence |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|");
    for setting in &report.grid.settings {
        let _ = writeln!(
            out,
            "| {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:?} |",
            setting.depth,
            setting.decay,
            setting.candidate_weight,
            setting.score_floor,
            setting.adaptation_weight,
            setting.behaviour_weight,
            setting.min_confidence
        );
    }

    let _ = writeln!(out, "\n## Cases\n");
    let _ = writeln!(
        out,
        "| project | revision | check | scored | matched | not planned now | no ground truth | analyse failed | stored misses | stored false alarms | load | extract per case (ms) | impact per case and setting (µs) | threads |"
    );
    let _ = writeln!(
        out,
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for info in &report.projects {
        for (check, coverage, extract, impact) in [
            (
                "compiler",
                &info.compiler,
                info.compiler_extract_ms,
                info.compiler_impact_us,
            ),
            (
                "tests",
                &info.tests,
                info.tests_extract_ms,
                info.tests_impact_us,
            ),
        ] {
            let cases = u64::from(coverage.matched);
            let scored = coverage.matched.saturating_sub(coverage.blastr_failed);
            let analyses = cases.saturating_mul(report.grid.settings.len() as u64);
            let _ = writeln!(
                out,
                "| {} | `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} ms | {} ms | {} µs | {} |",
                info.project,
                info.revision,
                check,
                scored,
                coverage.matched,
                coverage.not_planned,
                coverage.no_ground_truth,
                coverage.blastr_failed,
                coverage.stored_misses,
                coverage.stored_false_alarms,
                info.load_ms,
                extract.checked_div(cases).unwrap_or(0),
                impact.checked_div(analyses).unwrap_or(0),
                info.threads
            );
        }
    }

    let _ = writeln!(out, "\n## Every setting\n");
    let _ = writeln!(
        out,
        "`misses` counts the compiler errors Blastr did not list plus the failing tests it did not select; `items` is the size of its list, both classes; `selected` is the tests it would run; `cut` is what the analysis says the cut-offs dropped, as below-floor / beyond-depth / below-confidence items.\n"
    );
    let _ = writeln!(
        out,
        "| depth | decay | candidate | floor | misses | false alarms | items | unconfirmed | selected | suite | selection | cut |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (index, row) in report.settings.iter().enumerate() {
        let SettingTotals {
            setting,
            compiler,
            tests,
        } = row;
        let marks = verdict_marks(report, index);
        let _ = writeln!(
            out,
            "| {}{marks} | {:.2} | {:.2} | {:.2} | {} | {} | {} | {} | {} | {} | {:.0}% | {} / {} / {} |",
            setting.depth,
            setting.decay,
            setting.candidate_weight,
            setting.score_floor,
            compiler.misses.saturating_add(tests.misses),
            compiler.false_alarms,
            compiler.blastr_items,
            compiler.unconfirmed,
            tests.listed,
            tests.suite_total,
            tests.selection_percent(),
            compiler.below_floor.saturating_add(tests.below_floor),
            compiler.beyond_depth.saturating_add(tests.beyond_depth),
            compiler
                .below_confidence
                .saturating_add(tests.below_confidence)
        );
    }
    let _ = writeln!(
        out,
        "\n`*` marks the Pareto front — no other row is at least as good in losses, list size and selected tests with one of them strictly better — `+` the winner of the selection rule, `~` the current defaults."
    );

    let _ = writeln!(out, "\n## Pareto front\n");
    if report.pareto.is_empty() {
        let _ = writeln!(out, "None.");
    } else {
        let _ = writeln!(
            out,
            "| depth | decay | candidate | floor | misses | items | selected |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|");
        for index in &report.pareto {
            let Some(row) = report.settings.get(*index) else {
                continue;
            };
            let rank = row.rank();
            let _ = writeln!(
                out,
                "| {} | {:.2} | {:.2} | {:.2} | {} | {} | {} |",
                row.setting.depth,
                row.setting.decay,
                row.setting.candidate_weight,
                row.setting.score_floor,
                rank.misses,
                rank.items,
                rank.selected
            );
        }
    }

    let _ = writeln!(out, "\n## Winner\n");
    match (&report.winner, &report.default_index) {
        (Some(winner), defaults) => {
            let _ = writeln!(
                out,
                "**`{}`** — {} miss(es), {} item(s) of the compiler check, {} selected test(s), {:.0}% of the suite.\n",
                winner.setting.label(),
                winner.rank.misses,
                winner.rank.items,
                winner.rank.selected,
                winner.tests.selection_percent()
            );
            let _ = writeln!(
                out,
                "Rule: fewest true items lost to the cut-offs, then the fewest items listed, then the fewest selected tests, then the setting closest to the current defaults.\n"
            );
            if let Some(index) = defaults
                && let Some(row) = report.settings.get(*index)
            {
                let base = row.rank();
                let _ = writeln!(
                    out,
                    "Against the defaults (`{}`: {} miss(es), {} item(s), {} selected): {:+} miss(es), {:+} item(s), {:+} selected test(s).\n",
                    row.setting.label(),
                    base.misses,
                    base.items,
                    base.selected,
                    i64::from(winner.rank.misses) - i64::from(base.misses),
                    i64::from(winner.rank.items) - i64::from(base.items),
                    i64::from(winner.rank.selected) - i64::from(base.selected)
                );
            } else {
                let _ = writeln!(
                    out,
                    "The default setting is not in this grid, so it has no row here.\n"
                );
            }
            let edge = edges(&report.grid.settings, &winner.setting);
            if !edge.is_empty() {
                let _ = writeln!(
                    out,
                    "The winner is at the end of the range scanned for {}: a value outside the grid may do better, so widen the sweep before the value is taken as final.\n",
                    edge.join(", ")
                );
            }
        }
        (None, _) => {
            let _ = writeln!(out, "No winner: the grid is empty.");
        }
    }

    let _ = writeln!(out, "\n## Held-out confirmation\n");
    if report.confirmation.is_empty() {
        let _ = writeln!(
            out,
            "None: the grid was not checked on a project it did not see. Pass `--confirm <project>`."
        );
    } else {
        let _ = writeln!(
            out,
            "| project | revision | row | misses | false alarms | items | selected | selection |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
        for confirmation in &report.confirmation {
            for (position, row) in confirmation.rows.iter().enumerate() {
                let kind = if position == 0 { "winner" } else { "defaults" };
                let _ = writeln!(
                    out,
                    "| {} | `{}` | {} {kind} | {} | {} | {} | {} | {:.0}% |",
                    confirmation.project,
                    confirmation.revision,
                    row.setting.label(),
                    row.rank.misses,
                    row.compiler.false_alarms,
                    row.compiler.blastr_items,
                    row.tests.listed,
                    row.tests.selection_percent()
                );
            }
        }
    }

    let _ = writeln!(out, "\n## What this run did and did not measure\n");
    for note in &report.notes {
        let _ = writeln!(out, "- {note}");
    }
    out
}

/// The parameters of the winner that sit at an end of the swept range.
///
/// A winner at the edge says the search stopped where the answer might still be
/// improving, which a reader of the table should know before the value is fixed.
fn edges(grid: &[Setting], winner: &Setting) -> Vec<String> {
    let mut found = Vec::new();
    let mut ends = |name: &str, values: Vec<f32>, value: f32| {
        let low = values.iter().copied().fold(f32::INFINITY, f32::min);
        let high = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if values.len() > 1 && (value <= low || value >= high) {
            found.push(format!("{name} {value}"));
        }
    };
    ends(
        "depth",
        grid.iter().map(|setting| setting.depth as f32).collect(),
        winner.depth as f32,
    );
    ends(
        "decay",
        grid.iter().map(|setting| setting.decay).collect(),
        winner.decay,
    );
    ends(
        "candidate weight",
        grid.iter()
            .map(|setting| setting.candidate_weight)
            .collect(),
        winner.candidate_weight,
    );
    ends(
        "score floor",
        grid.iter().map(|setting| setting.score_floor).collect(),
        winner.score_floor,
    );
    found
}

/// The marks a table row carries: winner, Pareto front, defaults.
fn verdict_marks(report: &TuningReport, index: usize) -> &'static str {
    let winner = report
        .winner
        .as_ref()
        .is_some_and(|winner| winner.setting == report.grid.settings[index]);
    let front = report.pareto.contains(&index);
    let defaults = report.default_index == Some(index);
    match (winner, front, defaults) {
        (true, _, true) => " +~",
        (true, _, false) => " +",
        (false, true, true) => " *~",
        (false, true, false) => " *",
        (false, false, true) => " ~",
        (false, false, false) => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A setting with the given searched parameters.
    fn setting(depth: u32, decay: f32, candidate: f32, floor: f32) -> Setting {
        Setting {
            depth,
            decay,
            candidate_weight: candidate,
            score_floor: floor,
            ..Setting::defaults()
        }
    }

    /// A row with the given losses, list size and selected tests.
    fn row(misses: u32, items: u32, selected: u32) -> SettingTotals {
        let mut row = SettingTotals::new(Setting::defaults());
        row.compiler.misses = misses;
        row.compiler.blastr_items = items;
        row.tests.listed = selected;
        row
    }

    /// An entry set of the ground truth.
    fn set(entries: &[&str]) -> BTreeSet<String> {
        entries.iter().map(|entry| (*entry).to_owned()).collect()
    }

    #[test]
    fn a_case_without_losses_has_no_misses() {
        let expected = set(&["a", "b"]);
        let listed: BTreeSet<&str> = ["a", "b"].into_iter().collect();
        assert_eq!(score_case(&expected, &listed, true), (0, 0));
    }

    #[test]
    fn a_miss_is_an_expected_entry_that_was_not_listed() {
        let expected = set(&["a", "b"]);
        let listed: BTreeSet<&str> = ["a"].into_iter().collect();
        assert_eq!(score_case(&expected, &listed, true), (1, 0));
    }

    #[test]
    fn a_false_alarm_is_a_listed_entry_that_was_not_expected() {
        let expected = set(&["a"]);
        let listed: BTreeSet<&str> = ["a", "z"].into_iter().collect();
        assert_eq!(score_case(&expected, &listed, true), (0, 1));
    }

    #[test]
    fn the_test_check_counts_no_false_alarms() {
        // A test Blastr selects that does not fail is a cost, not an error.
        let expected = set(&["t::one"]);
        let listed: BTreeSet<&str> = ["t::one", "t::two"].into_iter().collect();
        assert_eq!(score_case(&expected, &listed, false), (0, 0));
        let missed: BTreeSet<&str> = ["t::two"].into_iter().collect();
        assert_eq!(score_case(&expected, &missed, false), (1, 0));
    }

    #[test]
    fn totals_add_up_and_keep_the_selection_share() {
        let mut left = Totals {
            cases: 1,
            listed: 3,
            suite_total: 10,
            ..Totals::default()
        };
        let right = Totals {
            cases: 2,
            listed: 2,
            suite_total: 10,
            ..Totals::default()
        };
        left.add(&right);
        assert_eq!(left.cases, 3);
        assert_eq!(left.listed, 5);
        assert!((left.selection_percent() - 25.0).abs() < 0.01);
        assert_eq!(Totals::default().selection_percent(), 0.0);
    }

    #[test]
    fn the_selection_rule_prefers_the_fewest_losses() {
        let totals = vec![row(0, 30, 5), row(1, 4, 1), row(0, 20, 6)];
        // The two rows that lose nothing are preferred; of them, the shorter
        // list wins.
        assert_eq!(select(&totals), Some(2));
    }

    #[test]
    fn a_setting_that_lists_more_but_loses_nothing_wins_over_a_short_lossy_one() {
        let totals = vec![row(1, 2, 1), row(0, 9, 1)];
        assert_eq!(select(&totals), Some(1));
    }

    #[test]
    fn ties_go_to_the_fewest_selected_tests() {
        let totals = vec![row(0, 5, 9), row(0, 5, 3)];
        assert_eq!(select(&totals), Some(1));
    }

    #[test]
    fn the_last_tie_break_is_the_distance_to_the_defaults() {
        let mut close = row(0, 5, 3);
        close.setting = setting(6, 0.6, 0.4, 0.05);
        let mut far = row(0, 5, 3);
        far.setting = setting(10, 0.8, 0.4, 0.05);
        assert_eq!(select(&[far, close]), Some(1));
    }

    #[test]
    fn the_rank_combines_both_checks() {
        let mut totals = SettingTotals::new(Setting::defaults());
        totals.compiler.misses = 2;
        totals.compiler.blastr_items = 7;
        totals.tests.misses = 3;
        totals.tests.listed = 4;
        let rank = totals.rank();
        assert_eq!(rank.misses, 5);
        assert_eq!(rank.items, 7);
        assert_eq!(rank.selected, 4);
        assert!(rank.distance < f64::EPSILON);
    }

    #[test]
    fn an_empty_grid_has_no_winner() {
        assert_eq!(select(&[]), None);
        assert!(pareto(&[]).is_empty());
    }

    #[test]
    fn the_pareto_front_keeps_the_undominated_rows() {
        let totals = vec![
            row(0, 30, 5), // 0: fewest losses, longest list
            row(1, 4, 1),  // 1: shortest list, one loss
            row(2, 4, 1),  // 2: worse than 1 on every count
            row(1, 9, 2),  // 3: worse than 1 on every count
            row(0, 30, 4), // 4: same losses and items as 0, fewer tests
        ];
        let front = pareto(&totals);
        assert_eq!(front, vec![1, 4]);
    }

    #[test]
    fn a_row_that_is_no_better_anywhere_is_not_on_the_front() {
        let totals = vec![row(1, 5, 2), row(1, 5, 3)];
        // Same losses and items: only the fewer selected tests are undominated.
        assert_eq!(pareto(&totals), vec![0]);
    }

    #[test]
    fn the_default_grid_covers_every_value_and_stays_small() {
        let grid = default_grid();
        assert!(grid.len() <= 60, "{} settings", grid.len());
        assert!(grid.contains(&Setting::defaults()));
        for decay in DECAYS {
            assert!(grid.iter().any(|setting| setting.decay == decay));
        }
        for floor in SCORE_FLOORS {
            assert!(grid.iter().any(|setting| setting.score_floor == floor));
        }
        for depth in DEPTHS {
            assert!(grid.iter().any(|setting| setting.depth == depth));
        }
        for candidate in CANDIDATE_WEIGHTS {
            assert!(
                grid.iter()
                    .any(|setting| setting.candidate_weight == candidate)
            );
        }
        assert_eq!(grid[0], Setting::defaults(), "the defaults come first");
        let unique: HashSet<String> = grid.iter().map(Setting::label).collect();
        assert_eq!(unique.len(), grid.len(), "no duplicates");
    }

    #[test]
    fn a_grid_spec_is_a_cartesian_product() {
        let grid = parse_grid("decay=0.5,0.7;floor=0.02;depth=6,10").expect("a valid spec");
        assert_eq!(grid.len(), 4);
        assert!(grid.iter().all(|setting| setting.score_floor == 0.02));
        assert!(
            grid.iter()
                .all(|setting| setting.candidate_weight == Setting::defaults().candidate_weight)
        );
        assert_eq!(grid.iter().filter(|setting| setting.depth == 10).count(), 2);
    }

    #[test]
    fn a_full_four_way_grid_is_what_the_parameters_multiply_to() {
        let grid = parse_grid(
            "decay=0.5,0.6,0.7,0.8;candidate=0.3,0.4,0.6,0.8;floor=0.01,0.02,0.05;depth=6,8,10",
        )
        .expect("a valid spec");
        assert_eq!(grid.len(), 4 * 4 * 3 * 3);
    }

    #[test]
    fn a_grid_spec_that_names_nothing_or_the_wrong_thing_is_refused() {
        assert!(parse_grid("").is_err());
        assert!(parse_grid("nonsense=1").is_err());
        assert!(parse_grid("decay").is_err());
        assert!(parse_grid("decay=abc").is_err());
        assert!(parse_grid("decay=-0.5").is_err());
        assert!(parse_grid("depth=6;depth=8").is_err());
        assert!(parse_grid("depth=x").is_err());
    }

    #[test]
    fn one_setting_parses_from_a_pairs_list() {
        let params =
            parse_params("depth=8,decay=0.8,candidate=0.4,floor=0.05").expect("a valid spec");
        assert_eq!(params.depth, 8);
        assert!((params.ranking.decay - 0.8).abs() < f32::EPSILON);
        assert!((params.ranking.candidate_weight - 0.4).abs() < f32::EPSILON);
        assert!((params.ranking.score_floor - 0.05).abs() < f32::EPSILON);
        assert_eq!(params, Setting::from_params(&params).params());
        assert_eq!(Setting::from_params(&params).label(), "8/0.80/0.40/0.05");
    }

    #[test]
    fn a_pairs_list_keeps_the_parameters_it_does_not_name() {
        let params = parse_params("depth=10").expect("a valid spec");
        assert_eq!(params.depth, 10);
        assert_eq!(
            params.ranking.score_floor,
            Setting::defaults().params().ranking.score_floor
        );
        assert_eq!(
            parse_params("score_floor=0.02")
                .map(|params| params.depth)
                .ok(),
            Some(Setting::defaults().params().depth)
        );
    }

    #[test]
    fn a_pairs_list_that_names_nothing_or_the_wrong_thing_is_refused() {
        assert!(parse_params("").is_err());
        assert!(parse_params("nonsense=1").is_err());
        assert!(parse_params("depth").is_err());
        assert!(parse_params("depth=x").is_err());
        assert!(parse_params("decay=-0.5").is_err());
        assert!(parse_params("depth=8;decay=0.8").is_err());
        assert!(parse_params("depth=8,depth=10").is_err());
    }

    #[test]
    fn a_setting_round_trips_into_parameters() {
        let setting = setting(10, 0.5, 0.8, 0.01);
        let params = setting.params();
        assert_eq!(params.depth, 10);
        assert!((params.ranking.decay - 0.5).abs() < f32::EPSILON);
        assert!((params.ranking.score_floor - 0.01).abs() < f32::EPSILON);
        assert_eq!(Setting::from_params(&params), setting);
        assert_eq!(setting.label(), "10/0.50/0.80/0.01");
        assert!(same_params(&params, &setting.params()));
    }

    #[test]
    fn the_distance_to_the_same_setting_is_zero() {
        let defaults = Setting::defaults();
        assert!(defaults.distance(&defaults) < f64::EPSILON);
        let other = setting(10, 0.8, 0.8, 0.01);
        assert!(other.distance(&defaults) > defaults.distance(&setting(8, 0.6, 0.4, 0.05)));
    }

    #[test]
    fn the_edge_of_the_grid_is_reported() {
        let grid = default_grid();
        // The deepest, slowest-decaying setting is at the end of three sweeps.
        let winner = Setting {
            depth: 10,
            decay: 0.8,
            score_floor: 0.01,
            ..Setting::defaults()
        };
        let found = edges(&grid, &winner);
        assert!(found.contains(&"depth 10".to_owned()), "{found:?}");
        assert!(found.contains(&"decay 0.8".to_owned()), "{found:?}");
        assert!(found.contains(&"score floor 0.01".to_owned()), "{found:?}");
        assert!(
            !found.iter().any(|edge| edge.starts_with("candidate")),
            "the default candidate weight is not an end of its sweep: {found:?}"
        );
        // The defaults are the smallest depth and the largest floor scanned.
        assert_eq!(
            edges(&grid, &Setting::defaults()),
            vec!["depth 6".to_owned(), "score floor 0.05".to_owned()]
        );
    }

    #[test]
    fn a_setting_serializes_with_readable_numbers() {
        let text =
            serde_json::to_string(&setting(6, 0.6, 0.4, 0.05)).expect("a setting serializes");
        assert!(text.contains("\"decay\":0.6"), "{text}");
        assert!(text.contains("\"score_floor\":0.05"), "{text}");
        assert!(text.contains("\"min_confidence\":\"possible\""), "{text}");
    }
}
