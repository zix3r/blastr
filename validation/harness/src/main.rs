//! `blastr-validate`: the accuracy harness for Blastr.
//!
//! Two checks answer "does Blastr miss things, and does it cry wolf?" on a
//! sample project. The compiler check applies one seeded edit to one definition
//! and compares the errors the compiler reports with the items Blastr lists as
//! *needs editing*. The test check seeds one mutation into one function body and
//! compares the tests that then fail with the tests Blastr selects.
//!
//! The harness has a workspace of its own (the root `Cargo.toml` of Blastr
//! excludes this directory), so building it never costs Blastr's own builds
//! anything. Point `CARGO_TARGET_DIR` at Blastr's `target/` to reuse the
//! dependencies it has already compiled.

mod cargo;
mod cli;
mod compiler;
mod edits;
mod map;
mod mutate;
mod plan;
mod project;
mod reach;
mod results;
mod tests_check;

use std::fmt;
use std::path::{Path, PathBuf};

use cli::{Check, Command, Report};

/// The result type of the harness.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong.
#[derive(Debug)]
pub enum Error {
    /// The harness cannot do what it was asked: a bad case, an unusable
    /// project, a report that cannot be written.
    Harness(String),
    /// The file system reported an error.
    Io(std::io::Error),
    /// Blastr itself failed.
    Blastr(blastr_core::Error),
    /// The cache failed.
    Store(blastr_store::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Harness(message) => f.write_str(message),
            Error::Io(error) => write!(f, "I/O error: {error}"),
            Error::Blastr(error) => write!(f, "Blastr failed: {error}"),
            Error::Store(error) => write!(f, "the cache failed: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::Io(error)
    }
}

impl From<String> for Error {
    fn from(message: String) -> Error {
        Error::Harness(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Error {
        Error::Harness(message.to_owned())
    }
}

impl From<blastr_core::Error> for Error {
    fn from(error: blastr_core::Error) -> Error {
        Error::Blastr(error)
    }
}

impl From<blastr_store::Error> for Error {
    fn from(error: blastr_store::Error) -> Error {
        Error::Store(error)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&args) {
        eprintln!("blastr-validate: {error}");
        std::process::exit(1);
    }
}

/// Runs one command.
fn run(args: &[String]) -> Result<()> {
    match cli::parse(args).map_err(Error::Harness)? {
        Command::Help => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        Command::Compiler(options) => run_check(options, true),
        Command::Tests(options) => run_check(options, false),
        Command::Report(options) => run_report(&options),
    }
}

/// Runs one check on one project and writes the report when asked to.
fn run_check(options: Check, compiler: bool) -> Result<()> {
    let mut project = project::Project::open(&options.project)?;
    let check_options = compiler::Options {
        limit: options.limit,
        seed: options.seed,
        kinds: options.kinds.clone(),
        case: options.case.clone(),
    };
    let report = if compiler {
        compiler::run(&mut project, &check_options)?
    } else {
        tests_check::run(&mut project, &check_options)?
    };
    print_totals(&report);
    if let Some(path) = &options.out {
        results::write(path, &report)?;
        println!("written to {}", path.display());
    }
    Ok(())
}

/// Prints the closing numbers of a check.
fn print_totals(report: &results::CheckReport) {
    let totals = &report.totals;
    println!(
        "\n{} {}: {} cases ({} excluded), {} miss(es), {} false alarm(s), {} unconfirmed item(s)",
        report.project,
        report.check,
        totals.cases,
        totals.excluded,
        totals.misses,
        totals.false_alarms,
        totals.unconfirmed
    );
    for kind in &report.kinds {
        println!(
            "  {:28} {:4} cases {:3} excluded {:4} miss(es) {:4} false alarm(s) reachable {:4} listed {:4}",
            kind.kind,
            kind.cases,
            kind.excluded,
            kind.misses,
            kind.false_alarms,
            kind.reachable,
            kind.listed
        );
    }
    println!(
        "  blastr {} ms per case, cargo {} ms per case, load {} ms",
        report.mean_blastr_ms, report.mean_cargo_ms, report.base_load_ms
    );
}

/// Runs both checks over the projects and writes the report.
fn run_report(options: &Report) -> Result<()> {
    let out_dir = options
        .out
        .clone()
        .unwrap_or_else(|| results_dir().join(""));
    let out_dir = if out_dir.extension().is_some() {
        out_dir.parent().map(Path::to_path_buf).unwrap_or(out_dir)
    } else {
        out_dir
    };
    std::fs::create_dir_all(&out_dir)?;

    let projects = if options.projects.is_empty() {
        default_projects()?
    } else {
        options.projects.clone()
    };
    if projects.is_empty() {
        return Err(Error::Harness(format!(
            "no project to check: `{}` holds no directory",
            projects_dir().display()
        )));
    }

    let mut reports = Vec::new();
    for dir in &projects {
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        for compiler in [true, false] {
            let check = if compiler { "compiler" } else { "tests" };
            let path = out_dir.join(format!("{name}-{check}.json"));
            if options.cached && path.exists() {
                println!("{} {}: reusing {}", name, check, path.display());
                reports.push(results::read(&path)?);
                continue;
            }
            let mut project = project::Project::open(dir)?;
            let check_options = compiler::Options {
                limit: options.limit,
                seed: options.seed,
                kinds: Vec::new(),
                case: None,
            };
            let report = if compiler {
                compiler::run(&mut project, &check_options)?
            } else {
                tests_check::run(&mut project, &check_options)?
            };
            print_totals(&report);
            results::write(&path, &report)?;
            println!("written to {}", path.display());
            reports.push(report);
        }
    }

    let markdown = results::markdown(&reports);
    let path = out_dir.join("report.md");
    std::fs::write(&path, markdown)?;
    println!("written to {}", path.display());
    Ok(())
}

/// The `validation/results` directory next to the harness.
pub fn results_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|parent| parent.join("results"))
        .unwrap_or_else(|| PathBuf::from("results"))
}

/// The `validation/projects` directory next to the harness.
pub fn projects_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|parent| parent.join("projects"))
        .unwrap_or_else(|| PathBuf::from("projects"))
}

/// Every directory under `validation/projects`, sorted by name.
fn default_projects() -> Result<Vec<PathBuf>> {
    let dir = projects_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("Cargo.toml").is_file())
        .collect();
    found.sort();
    Ok(found)
}
