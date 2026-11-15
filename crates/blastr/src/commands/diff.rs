//! `blastr diff`: the structural changes between two revisions.

use blastr_core::{CancelToken, Engine};
use blastr_model::{AnalysisOptions, DiffResult};

use crate::cli::{Format, Globals, RevArgs};
use crate::exit::Exit;
use crate::output;
use crate::progress::PhaseProgress;

/// Runs the command and prints the result in the requested format.
pub fn run(globals: &Globals, args: &RevArgs) -> Result<Exit, blastr_core::Error> {
    if let Some(name) = unavailable_format(globals.format) {
        eprintln!("error: the {name} format is not available for `blastr diff`");
        eprintln!("hint: use `--format human` or `--format json`");
        return Ok(Exit::Error);
    }

    let root = super::repository_root()?;
    let options = super::analysis_options(globals, &root);
    let spec = super::rev_spec(args);
    let progress = PhaseProgress::from_stderr();
    let cancel = CancelToken::new();

    let engine = Engine::open()?;
    let result = engine.diff(&root, &spec, &options, &progress, &cancel)?;
    progress.finish();
    let text = render(&result, globals, &options)?;
    super::write_stdout(&text)?;
    if globals.timings {
        progress.print_timings();
    }
    Ok(Exit::Success)
}

/// The name of the format when `diff` cannot print it.
fn unavailable_format(format: Format) -> Option<&'static str> {
    match format {
        Format::Human | Format::Json => None,
        Format::Markdown => Some("markdown"),
        Format::Cargo => Some("cargo"),
    }
}

/// Renders the result in the requested format.
fn render(
    result: &DiffResult,
    globals: &Globals,
    options: &AnalysisOptions,
) -> Result<String, blastr_core::Error> {
    match globals.format {
        Format::Json => {
            output::render_json(result).map_err(|err| blastr_core::Error::Codec(err.to_string()))
        }
        _ => Ok(output::render_diff(result, options, globals.all)),
    }
}
