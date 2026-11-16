//! The commands that do real work, and what they share.
//!
//! Every command that analyses a comparison resolves the same things first: the
//! repository, the configuration files, the parameters the command line
//! overrides and how a human-readable report should look. That resolution lives
//! here, together with the checks a command applies before it spends time on an
//! analysis.

pub mod analyze;
pub mod cache;
pub mod diff;
pub mod explain;
pub mod impact;
pub mod index;
pub mod tests;
pub mod watch;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use blastr_core::Engine;
use blastr_core::config::Config;
use blastr_model::{
    Analysis, AnalysisOptions, AnalysisParams, Assume, Confidence, ExecMode, ImpactClass, RelPath,
    RevSpec,
};

use crate::cli::{self, FailOn, Format, Globals, RevArgs};
use crate::exit::Exit;
use crate::output::analysis::Style;

/// Shortest evidence path `--depth` may ask for.
const MIN_DEPTH: u32 = 1;

/// Longest evidence path `--depth` may ask for.
const MAX_DEPTH: u32 = 64;

/// A problem with how the command was asked to run, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// What is wrong.
    pub message: String,
    /// What to do instead.
    pub hint: String,
}

impl Problem {
    /// Prints the problem and its hint to stderr.
    pub fn report(&self) {
        let Problem { message, hint } = self;
        eprintln!("error: {message}");
        eprintln!("hint: {hint}");
    }
}

/// The formats `blastr analyze` and `blastr impact` can print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    /// The human-readable report.
    Human,
    /// Pretty JSON, for other tools.
    Json,
    /// Markdown, for a pull request.
    Markdown,
}

impl ReportFormat {
    /// The format to print for `command`, or why it cannot print `format`.
    ///
    /// The two commands accept the same formats; `tests` adds the `cargo` one,
    /// which needs a test selection to describe.
    pub fn resolve(command: &str, format: Format) -> Result<ReportFormat, Problem> {
        match format {
            Format::Human => Ok(ReportFormat::Human),
            Format::Json => Ok(ReportFormat::Json),
            Format::Markdown => Ok(ReportFormat::Markdown),
            Format::Cargo => Err(Problem {
                message: "the cargo format is only available for `blastr tests`".to_owned(),
                hint: format!(
                    "use `--format human`, `--format json` or `--format markdown` with `blastr {command}`"
                ),
            }),
        }
    }
}

/// Checks that `blastr explain` was asked for the one format it prints.
///
/// Its report describes a single item, so there is no JSON or Markdown
/// rendering of it; `Err` holds the lines to print for anything else.
pub fn explain_format(format: Format) -> Result<(), Problem> {
    match format {
        Format::Human => Ok(()),
        other => Err(Problem {
            message: format!(
                "the {} format is not available for `blastr explain`",
                format_name(other)
            ),
            hint: "use `--format human`, or run `blastr analyze` for the whole result".to_owned(),
        }),
    }
}

/// How a format is named in a message.
fn format_name(format: Format) -> &'static str {
    match format {
        Format::Human => "human",
        Format::Json => "json",
        Format::Markdown => "markdown",
        Format::Cargo => "cargo",
    }
}

/// Checks `--depth`, returning what to print when it is out of range.
pub fn depth_problem(depth: Option<u32>) -> Option<Problem> {
    let depth = depth?;
    if (MIN_DEPTH..=MAX_DEPTH).contains(&depth) {
        return None;
    }
    Some(Problem {
        message: format!("--depth must be between {MIN_DEPTH} and {MAX_DEPTH}, not {depth}"),
        hint: format!(
            "pass `--depth {}` for the default, or a larger number to follow longer chains",
            AnalysisParams::default().depth
        ),
    })
}

/// Everything one analysis command needs.
pub struct Setup {
    /// The engine, which owns the cache and knows where the user configuration lives.
    pub engine: Engine,
    /// The repository to analyse.
    pub root: PathBuf,
    /// How to load the project.
    pub options: AnalysisOptions,
    /// What shapes the result.
    pub params: AnalysisParams,
    /// How to decorate a human-readable report.
    pub style: Style,
}

/// Resolves the repository, the configuration and the parameters of one run.
///
/// Returns `Ok(None)` after printing a problem when the command line cannot be
/// used as it was given.
pub fn setup(globals: &Globals) -> Result<Option<Setup>, blastr_core::Error> {
    if let Some(problem) = depth_problem(globals.depth) {
        problem.report();
        return Ok(None);
    }

    let root = repository_root()?;
    let engine = Engine::open()?;
    let config = Config::load(&root, engine.config_dir())?;
    let params = effective_params(&config, globals.depth, globals.min_confidence);
    let mut options = analysis_options(globals, &root);
    options.features = effective_features(&globals.features, config.features.as_deref());
    let style = style(globals.all);
    Ok(Some(Setup {
        engine,
        root,
        options,
        params,
        style,
    }))
}

/// The parameters a run uses: the configuration's, with the command line on top.
pub fn effective_params(
    config: &Config,
    depth: Option<u32>,
    min_confidence: Option<cli::Confidence>,
) -> AnalysisParams {
    let mut params = config.params();
    if let Some(depth) = depth {
        params.depth = depth;
    }
    if let Some(confidence) = min_confidence {
        params.min_confidence = model_confidence(confidence);
    }
    params
}

/// The features to load the project with: the command line's, else the
/// configuration's, else none.
///
/// A configured list of exactly `["default"]` names the default features, which
/// is what enabling no feature does, so it counts as no list at all.
pub fn effective_features(command_line: &[String], configured: Option<&[String]>) -> Vec<String> {
    if !command_line.is_empty() {
        return command_line.to_vec();
    }
    let Some(configured) = configured else {
        return Vec::new();
    };
    if configured.len() == 1 && configured[0] == "default" {
        return Vec::new();
    }
    configured.to_vec()
}

/// How to render a human-readable report in this run.
///
/// Colour needs a terminal and no `NO_COLOR`; the line width is read from
/// `COLUMNS`, and only on a terminal.
pub fn style(list_all: bool) -> Style {
    let terminal = std::io::stdout().is_terminal();
    let no_color = std::env::var("NO_COLOR").ok();
    let columns = std::env::var("COLUMNS").ok();
    style_from(terminal, no_color.as_deref(), columns.as_deref(), list_all)
}

/// The style a terminal, `NO_COLOR`, `COLUMNS` and `--all` describe.
fn style_from(
    terminal: bool,
    no_color: Option<&str>,
    columns: Option<&str>,
    list_all: bool,
) -> Style {
    let colour = terminal && no_color.is_none_or(str::is_empty);
    let width = if terminal {
        columns
            .and_then(|text| text.trim().parse::<usize>().ok())
            .unwrap_or(0)
    } else {
        0
    };
    Style {
        colour,
        width,
        list_all,
    }
}

/// The model's confidence for a command-line level.
pub fn model_confidence(confidence: cli::Confidence) -> Confidence {
    match confidence {
        cli::Confidence::Definite => Confidence::Definite,
        cli::Confidence::Likely => Confidence::Likely,
        cli::Confidence::Possible => Confidence::Possible,
    }
}

/// The model's assumption for a kind of change named on the command line.
pub fn model_assume(assume: cli::Assume) -> Assume {
    match assume {
        cli::Assume::Body => Assume::Body,
        cli::Assume::Signature => Assume::Signature,
        cli::Assume::Removed => Assume::Removed,
        cli::Assume::Visibility => Assume::Visibility,
        cli::Assume::FieldRemoved => Assume::FieldRemoved,
        cli::Assume::FieldRetyped => Assume::FieldRetyped,
        cli::Assume::FieldAdded => Assume::FieldAdded,
        cli::Assume::VariantAdded => Assume::VariantAdded,
        cli::Assume::VariantRemoved => Assume::VariantRemoved,
    }
}

/// Whether the analysis lists an item of `class` the diff did not already edit.
///
/// An item the same diff changed was adapted by that diff already, so it is not
/// work left for the reader.
pub fn has_finding(analysis: &Analysis, class: FailOn) -> bool {
    let wanted = match class {
        FailOn::Adaptation => ImpactClass::Adaptation,
        FailOn::Behaviour => ImpactClass::Behaviour,
    };
    analysis
        .items
        .iter()
        .any(|item| item.class == wanted && !item.tags.changed)
}

/// The exit code a run ends with once its analysis is printed.
pub fn exit_for(globals: &Globals, analysis: &Analysis) -> Exit {
    match globals.fail_on {
        Some(class) if has_finding(analysis, class) => Exit::Findings,
        _ => Exit::Success,
    }
}

/// Builds the engine options from the global command-line options.
///
/// The repository path is the current directory, so `--manifest-path` only
/// contributes a directory when it lies below the current directory.
pub fn analysis_options(globals: &Globals, cwd: &Path) -> AnalysisOptions {
    AnalysisOptions {
        features: globals.features.clone(),
        all_features: globals.all_features,
        target: globals.target.clone(),
        exec: if globals.no_exec {
            ExecMode::Restricted
        } else {
            ExecMode::Full
        },
        manifest_dir: manifest_dir(globals.manifest_path.as_deref(), cwd),
    }
}

/// The directory of `--manifest-path`, relative to the current directory when
/// it is below it and as written otherwise.
fn manifest_dir(manifest_path: Option<&Path>, cwd: &Path) -> Option<RelPath> {
    let manifest_path = manifest_path?;
    let dir = match manifest_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => return None,
    };
    if let Ok(relative) = dir.strip_prefix(cwd) {
        if relative.as_os_str().is_empty() {
            return None;
        }
        return Some(
            RelPath::from_path(relative)
                .unwrap_or_else(|| RelPath::new(relative.to_string_lossy())),
        );
    }
    Some(RelPath::new(dir.to_string_lossy()))
}

/// Builds the revision specification from the revision arguments.
pub fn rev_spec(args: &RevArgs) -> RevSpec {
    RevSpec {
        rev: args.rev.clone(),
        worktree: args.worktree,
        staged: args.staged,
        parent: args.parent,
    }
}

/// The repository the commands work on: the current directory.
pub fn repository_root() -> Result<PathBuf, blastr_core::Error> {
    Ok(std::env::current_dir()?)
}

/// Writes text to stdout, where the result and nothing else goes.
pub fn write_stdout(text: &str) -> Result<(), blastr_core::Error> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
// The command module for `blastr tests` owns the name `tests` here.
mod setup_tests {
    use blastr_model::{
        Analysis, AnalysisParams, Comparison, DiffResult, ImpactItem, ImpactLimits, ImpactTags,
        Location, RelPath, Source, Span, SymbolKind, SymbolPath, TestSelection,
    };

    use super::*;

    /// An item with the given class and whether the diff changed it too.
    fn item(class: ImpactClass, changed: bool) -> ImpactItem {
        ImpactItem {
            symbol: SymbolPath::new("crate::item", SymbolKind::Function),
            location: Location {
                file: RelPath::new("src/lib.rs"),
                span: Span {
                    start: 0,
                    end: 1,
                    start_line: 1,
                    end_line: 1,
                },
            },
            class,
            confidence: Confidence::Definite,
            score: 1.0,
            distance: 1,
            paths: Vec::new(),
            path_count: 1,
            tags: ImpactTags {
                changed,
                ..ImpactTags::default()
            },
        }
    }

    /// An analysis of an empty comparison that lists `items`.
    fn analysis_with(items: Vec<ImpactItem>) -> Analysis {
        Analysis {
            diff: DiffResult {
                comparison: Comparison {
                    base: Source::Tree {
                        id: "aaaa".to_owned(),
                        label: "4f2a91c".to_owned(),
                    },
                    head: Source::Tree {
                        id: "bbbb".to_owned(),
                        label: "8be07d3".to_owned(),
                    },
                },
                base_key: None,
                head_key: None,
                changed_files: Vec::new(),
                changes: Vec::new(),
                limits: Vec::new(),
            },
            items,
            tests: TestSelection::default(),
            limits: ImpactLimits::default(),
            params: AnalysisParams::default(),
        }
    }

    fn features(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn defaults_apply_when_neither_the_command_line_nor_the_configuration_says_anything() {
        let params = effective_params(&Config::default(), None, None);
        assert_eq!(params, AnalysisParams::default());
    }

    #[test]
    fn the_configuration_applies_over_the_defaults() {
        let config = Config {
            depth: Some(3),
            min_confidence: Some(Confidence::Definite),
            ignore: Some(vec!["vendor/**".to_owned()]),
            ..Config::default()
        };
        let params = effective_params(&config, None, None);
        assert_eq!(params.depth, 3);
        assert_eq!(params.min_confidence, Confidence::Definite);
        assert_eq!(params.ignore, vec!["vendor/**".to_owned()]);
        assert_eq!(params.ranking, AnalysisParams::default().ranking);
    }

    #[test]
    fn the_command_line_applies_over_the_configuration() {
        let config = Config {
            depth: Some(3),
            min_confidence: Some(Confidence::Definite),
            ..Config::default()
        };
        let params = effective_params(&config, Some(12), Some(cli::Confidence::Possible));
        assert_eq!(params.depth, 12);
        assert_eq!(params.min_confidence, Confidence::Possible);
    }

    #[test]
    fn no_features_are_named_by_default() {
        assert!(effective_features(&[], None).is_empty());
        assert!(effective_features(&[], Some(&[])).is_empty());
    }

    #[test]
    fn the_configuration_supplies_the_features_when_the_command_line_names_none() {
        let configured = features(&["fast", "report"]);
        assert_eq!(
            effective_features(&[], Some(&configured)),
            features(&["fast", "report"])
        );
    }

    #[test]
    fn a_configured_default_feature_list_means_no_explicit_features() {
        assert!(effective_features(&[], Some(&features(&["default"]))).is_empty());
        assert_eq!(
            effective_features(&[], Some(&features(&["default", "fast"]))),
            features(&["default", "fast"])
        );
    }

    #[test]
    fn the_command_line_supplies_the_features_when_it_names_some() {
        let configured = features(&["fast"]);
        assert_eq!(
            effective_features(&features(&["report"]), Some(&configured)),
            features(&["report"])
        );
        assert_eq!(
            effective_features(&features(&["report"]), None),
            features(&["report"])
        );
    }

    #[test]
    fn a_terminal_without_no_color_and_with_columns_gets_colour_and_a_width() {
        let style = style_from(true, None, Some("100"), true);
        assert_eq!(
            style,
            Style {
                colour: true,
                width: 100,
                list_all: true,
            }
        );
        assert!(style_from(true, Some(""), Some(" 80 "), false).colour);
        assert_eq!(style_from(true, Some(""), Some(" 80 "), false).width, 80);
    }

    #[test]
    fn no_color_turns_colour_off() {
        assert!(!style_from(true, Some("1"), Some("100"), false).colour);
    }

    #[test]
    fn a_pipe_gets_no_colour_and_no_width() {
        let style = style_from(false, None, Some("100"), false);
        assert_eq!(
            style,
            Style {
                colour: false,
                width: 0,
                list_all: false,
            }
        );
    }

    #[test]
    fn an_unreadable_column_count_means_no_limit() {
        assert_eq!(style_from(true, None, Some("wide"), false).width, 0);
        assert_eq!(style_from(true, None, None, false).width, 0);
    }

    #[test]
    fn the_report_formats_are_the_three_that_describe_a_result() {
        for format in [Format::Human, Format::Json, Format::Markdown] {
            assert!(
                ReportFormat::resolve("analyze", format).is_ok(),
                "{format:?}"
            );
        }
        assert_eq!(
            ReportFormat::resolve("analyze", Format::Json),
            Ok(ReportFormat::Json)
        );
    }

    #[test]
    fn the_cargo_format_belongs_to_the_tests_command_alone() {
        for command in ["analyze", "impact"] {
            let problem = ReportFormat::resolve(command, Format::Cargo)
                .expect_err("the cargo format is rejected");
            assert_eq!(
                problem.message,
                "the cargo format is only available for `blastr tests`"
            );
            let hint = &problem.hint;
            assert!(hint.contains("--format human"), "{hint}");
        }
    }

    #[test]
    fn explain_prints_the_human_format_only() {
        assert_eq!(explain_format(Format::Human), Ok(()));
        for format in [Format::Json, Format::Markdown, Format::Cargo] {
            let problem = explain_format(format).expect_err("every other format is rejected");
            assert_eq!(
                problem.message,
                format!(
                    "the {} format is not available for `blastr explain`",
                    format_name(format)
                )
            );
            let hint = &problem.hint;
            assert!(hint.contains("--format human"), "{hint}");
        }
    }

    #[test]
    fn a_depth_inside_the_range_is_accepted() {
        assert_eq!(depth_problem(None), None);
        assert_eq!(depth_problem(Some(MIN_DEPTH)), None);
        assert_eq!(depth_problem(Some(MAX_DEPTH)), None);
    }

    #[test]
    fn a_depth_outside_the_range_is_reported() {
        for depth in [0, MAX_DEPTH + 1] {
            let problem = depth_problem(Some(depth)).expect("the depth is rejected");
            let message = &problem.message;
            let hint = &problem.hint;
            assert!(message.contains(&depth.to_string()), "{message}");
            assert!(message.contains("between 1 and 64"), "{message}");
            assert!(hint.contains("--depth"), "{hint}");
        }
    }

    #[test]
    fn an_edited_item_is_not_a_finding() {
        let analysis = analysis_with(vec![
            item(ImpactClass::Adaptation, true),
            item(ImpactClass::Behaviour, true),
        ]);
        assert!(!has_finding(&analysis, FailOn::Adaptation));
        assert!(!has_finding(&analysis, FailOn::Behaviour));
    }

    #[test]
    fn a_listed_adaptation_is_a_finding() {
        let analysis = analysis_with(vec![
            item(ImpactClass::Behaviour, false),
            item(ImpactClass::Adaptation, false),
        ]);
        assert!(has_finding(&analysis, FailOn::Adaptation));
        assert!(has_finding(&analysis, FailOn::Behaviour));
    }

    #[test]
    fn each_class_of_finding_matches_its_own_items() {
        let adaptation = analysis_with(vec![item(ImpactClass::Adaptation, false)]);
        assert!(has_finding(&adaptation, FailOn::Adaptation));
        assert!(!has_finding(&adaptation, FailOn::Behaviour));

        let behaviour = analysis_with(vec![item(ImpactClass::Behaviour, false)]);
        assert!(!has_finding(&behaviour, FailOn::Adaptation));
        assert!(has_finding(&behaviour, FailOn::Behaviour));
    }

    #[test]
    fn an_empty_result_has_no_findings() {
        assert!(!has_finding(&analysis_with(Vec::new()), FailOn::Adaptation));
        assert!(!has_finding(&analysis_with(Vec::new()), FailOn::Behaviour));
    }

    #[test]
    fn every_assumed_kind_maps_to_the_model() {
        let pairs = [
            (cli::Assume::Body, Assume::Body),
            (cli::Assume::Signature, Assume::Signature),
            (cli::Assume::Removed, Assume::Removed),
            (cli::Assume::Visibility, Assume::Visibility),
            (cli::Assume::FieldRemoved, Assume::FieldRemoved),
            (cli::Assume::FieldRetyped, Assume::FieldRetyped),
            (cli::Assume::FieldAdded, Assume::FieldAdded),
            (cli::Assume::VariantAdded, Assume::VariantAdded),
            (cli::Assume::VariantRemoved, Assume::VariantRemoved),
        ];
        for (from_cli, expected) in pairs {
            let mapped = model_assume(from_cli);
            assert_eq!(mapped, expected);
            assert_eq!(mapped.as_str(), from_cli_name(from_cli));
        }
    }

    /// The kebab-case name clap accepts for a kind of assumption.
    fn from_cli_name(assume: cli::Assume) -> &'static str {
        match assume {
            cli::Assume::Body => "body",
            cli::Assume::Signature => "signature",
            cli::Assume::Removed => "removed",
            cli::Assume::Visibility => "visibility",
            cli::Assume::FieldRemoved => "field-removed",
            cli::Assume::FieldRetyped => "field-retyped",
            cli::Assume::FieldAdded => "field-added",
            cli::Assume::VariantAdded => "variant-added",
            cli::Assume::VariantRemoved => "variant-removed",
        }
    }

    #[test]
    fn every_confidence_level_maps_to_the_model() {
        assert_eq!(
            model_confidence(cli::Confidence::Definite),
            Confidence::Definite
        );
        assert_eq!(
            model_confidence(cli::Confidence::Likely),
            Confidence::Likely
        );
        assert_eq!(
            model_confidence(cli::Confidence::Possible),
            Confidence::Possible
        );
    }
}
