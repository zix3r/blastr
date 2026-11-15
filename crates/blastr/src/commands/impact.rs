//! `blastr impact`: what one assumed change would affect.

use blastr_core::CancelToken;

use crate::cli::{self, Globals};
use crate::exit::Exit;
use crate::output::analysis::render_analysis;
use crate::output::formats::{analysis_json, analysis_markdown};
use crate::progress::PhaseProgress;

use super::ReportFormat;

/// Runs the what-if query for `symbol` and prints what it would affect.
pub fn run(
    globals: &Globals,
    symbol: &str,
    assume: cli::Assume,
) -> Result<Exit, blastr_core::Error> {
    let format = match ReportFormat::resolve("impact", globals.format) {
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
    let assume = super::model_assume(assume);
    let analysis = setup.engine.what_if(
        &setup.root,
        symbol,
        assume,
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
