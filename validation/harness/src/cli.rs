//! Command line of the validation harness.

use std::path::PathBuf;

use crate::edits::EditKind;

/// What the program was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// The compiler check on one project.
    Compiler(Check),
    /// The test check on one project.
    Tests(Check),
    /// Both checks over several projects, and the report.
    Report(Report),
    /// The usage text.
    Help,
}

/// The options of one check.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Check {
    /// The project directory.
    pub project: PathBuf,
    /// How many cases to sample.
    pub limit: Option<usize>,
    /// Seed of the sample.
    pub seed: Option<u64>,
    /// Which edit kinds to plan; empty means every kind.
    pub kinds: Vec<EditKind>,
    /// Rerun one case by id.
    pub case: Option<String>,
    /// Where to write the JSON report, when anywhere.
    pub out: Option<PathBuf>,
}

/// The options of the report.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// The projects to cover; empty means every directory under
    /// `validation/projects`.
    pub projects: Vec<PathBuf>,
    /// Reuse the per-project JSON files instead of running the checks.
    pub cached: bool,
    /// How many cases per check to sample.
    pub limit: Option<usize>,
    /// Seed of the sample.
    pub seed: Option<u64>,
    /// Where the JSON files and `report.md` go.
    pub out: Option<PathBuf>,
}

/// The usage text.
pub const USAGE: &str = "\
blastr-validate — accuracy harness for Blastr

USAGE
  blastr-validate compiler <project-dir> [options]
  blastr-validate tests <project-dir> [options]
  blastr-validate report [options]

COMMANDS
  compiler   Apply one seeded edit per definition and compare the compiler's
             errors with Blastr's *needs editing* items.
  tests      Apply one mutation per function and compare the failing tests
             with the tests Blastr selects.
  report     Run both checks over several projects and write
             validation/results/<project>-<check>.json and report.md.

OPTIONS
  --limit N        Sample N cases instead of all of them.
  --seed N         Sample with this seed; the sample stays deterministic.
  --kinds a,b      Compiler check only: which edit kinds to plan
                   (add-parameter, change-return-type, remove-field,
                   retype-field, add-field, add-variant, remove-variant,
                   add-required-trait-method, remove-function,
                   narrow-visibility).
  --case ID        Rerun one case and print Blastr's evidence for it.
  --out PATH       Where to write the JSON report (default: none for a single
                   check, validation/results for the report command).
  --projects a,b   Report only: the project directories (default: every
                   directory under validation/projects).
  --cached         Report only: reuse the JSON files instead of running.
  -h, --help       This text.
";

/// Parses the arguments, without the program name.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Ok(Command::Help);
    };
    if first == "-h" || first == "--help" || first == "help" {
        return Ok(Command::Help);
    }
    let mut rest = args[1..].to_vec();
    let mut check = Check::default();
    let mut report = Report::default();
    let mut project: Option<PathBuf> = None;

    let is_report = first == "report";
    if !is_report && first != "compiler" && first != "tests" {
        return Err(format!("unknown command `{first}`; try --help"));
    }

    while let Some(argument) = rest.first().cloned() {
        rest.remove(0);
        let mut value = |name: &str| -> Result<String, String> {
            if rest.is_empty() {
                return Err(format!("`{name}` needs a value"));
            }
            Ok(rest.remove(0))
        };
        match argument.as_str() {
            "--limit" => {
                let text = value("--limit")?;
                let limit: usize = text
                    .parse()
                    .map_err(|_| format!("`--limit` needs a number, not `{text}`"))?;
                check.limit = Some(limit);
                report.limit = Some(limit);
            }
            "--seed" => {
                let text = value("--seed")?;
                let seed: u64 = text
                    .parse()
                    .map_err(|_| format!("`--seed` needs a number, not `{text}`"))?;
                check.seed = Some(seed);
                report.seed = Some(seed);
            }
            "--kinds" => {
                let text = value("--kinds")?;
                for name in text.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                    let kind = EditKind::parse(name)
                        .ok_or_else(|| format!("`{name}` is not an edit kind; try --help"))?;
                    check.kinds.push(kind);
                }
            }
            "--case" => check.case = Some(value("--case")?),
            "--out" => {
                let path = PathBuf::from(value("--out")?);
                check.out = Some(path.clone());
                report.out = Some(path);
            }
            "--projects" => {
                let text = value("--projects")?;
                for name in text.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                    report.projects.push(PathBuf::from(name));
                }
            }
            "--cached" => report.cached = true,
            "-h" | "--help" => return Ok(Command::Help),
            other if other.starts_with("--") => {
                return Err(format!("unknown option `{other}`; try --help"));
            }
            other => {
                if project.is_some() {
                    return Err(format!("only one project can be checked, not `{other}`"));
                }
                project = Some(PathBuf::from(other));
            }
        }
    }

    if is_report {
        if project.is_some() {
            return Err("`report` takes its projects through `--projects`".to_owned());
        }
        return Ok(Command::Report(report));
    }
    let Some(project) = project else {
        return Err(format!("`{first}` needs a project directory; try --help"));
    };
    check.project = project;
    Ok(if first == "compiler" {
        Command::Compiler(check)
    } else {
        Command::Tests(check)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses from a space-separated string.
    fn parse_text(text: &str) -> Result<Command, String> {
        let args: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
        parse(&args)
    }

    #[test]
    fn no_arguments_prints_the_usage() {
        assert_eq!(parse(&[]), Ok(Command::Help));
        assert_eq!(parse_text("--help"), Ok(Command::Help));
    }

    #[test]
    fn a_check_needs_a_project() {
        assert!(parse_text("compiler").is_err());
        assert_eq!(
            parse_text("compiler ../projects/shopfront"),
            Ok(Command::Compiler(Check {
                project: PathBuf::from("../projects/shopfront"),
                ..Check::default()
            }))
        );
    }

    #[test]
    fn check_options_are_read() {
        let parsed = parse_text(
            "compiler shopfront --limit 20 --seed 7 --kinds add-field,remove-field --case 002-add-field-x --out r.json",
        );
        let Ok(Command::Compiler(check)) = parsed else {
            panic!("the command parses: {parsed:?}");
        };
        assert_eq!(check.limit, Some(20));
        assert_eq!(check.seed, Some(7));
        assert_eq!(check.kinds, vec![EditKind::AddField, EditKind::RemoveField]);
        assert_eq!(check.case.as_deref(), Some("002-add-field-x"));
        assert_eq!(check.out, Some(PathBuf::from("r.json")));
    }

    #[test]
    fn the_report_takes_projects_and_can_be_cached() {
        let parsed = parse_text("report --projects a,b --cached --limit 5");
        let Ok(Command::Report(report)) = parsed else {
            panic!("the command parses: {parsed:?}");
        };
        assert_eq!(
            report.projects,
            vec![PathBuf::from("a"), PathBuf::from("b")]
        );
        assert!(report.cached);
        assert_eq!(report.limit, Some(5));
    }

    #[test]
    fn bad_options_are_refused_with_a_reason() {
        assert!(parse_text("compiler x --limit many").is_err());
        assert!(parse_text("compiler x --kinds no-such-kind").is_err());
        assert!(parse_text("compiler x --nonsense").is_err());
        assert!(parse_text("nonsense x").is_err());
        assert!(parse_text("compiler a b").is_err());
        assert!(parse_text("report a").is_err());
    }

    #[test]
    fn a_missing_value_is_reported() {
        assert!(parse_text("compiler x --limit").is_err());
    }
}
