//! `blastr analyze`: the changes, what they affect and which tests to run.

use blastr_core::CancelToken;

use crate::cli::{Globals, RevArgs};
use crate::exit::Exit;
use crate::output::analysis::render_analysis;
use crate::output::formats::{analysis_json, analysis_markdown};
use crate::progress::PhaseProgress;

use super::ReportFormat;

/// Runs the command and prints the analysis in the requested format.
pub fn run(globals: &Globals, args: &RevArgs) -> Result<Exit, blastr_core::Error> {
    let format = match ReportFormat::resolve("analyze", globals.format) {
        Ok(format) => format,
        Err(problem) => {
            problem.report();
            return Ok(Exit::Error);
        }
    };
    let Some(setup) = super::setup(globals)? else {
        return Ok(Exit::Error);
    };

    let progress = PhaseProgress::from_stderr();
    let cancel = CancelToken::new();
    let spec = super::rev_spec(args);
    let analysis = setup.engine.analyze(
        &setup.root,
        &spec,
        &setup.options,
        &setup.params,
        &progress,
        &cancel,
    )?;
    progress.finish();

    let text = match format {
        ReportFormat::Human => render_analysis(&analysis, &setup.options, &setup.style),
        ReportFormat::Json => {
            analysis_json(&analysis).map_err(|err| blastr_core::Error::Codec(err.to_string()))?
        }
        ReportFormat::Markdown => analysis_markdown(&analysis, &setup.options),
    };
    super::write_stdout(&text)?;
    if globals.timings {
        progress.print_timings();
    }
    Ok(super::exit_for(globals, &analysis))
}
