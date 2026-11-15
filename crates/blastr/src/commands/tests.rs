//! `blastr tests`: only the tests a change reaches.

use blastr_core::CancelToken;

use crate::cli::{Format, Globals, RevArgs};
use crate::exit::Exit;
use crate::output::analysis::render_tests;
use crate::output::formats::{analysis_json, tests_cargo, tests_markdown};
use crate::progress::PhaseProgress;

/// Runs the analysis and prints the tests worth running.
///
/// Every format is offered: the `cargo` one turns the selection into the `cargo
/// test` commands that run exactly those tests.
pub fn run(globals: &Globals, args: &RevArgs) -> Result<Exit, blastr_core::Error> {
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

    let text = match globals.format {
        Format::Human => render_tests(&analysis, &setup.style),
        Format::Json => {
            analysis_json(&analysis).map_err(|err| blastr_core::Error::Codec(err.to_string()))?
        }
        Format::Markdown => tests_markdown(&analysis),
        Format::Cargo => tests_cargo(&analysis),
    };
    super::write_stdout(&text)?;
    if globals.timings {
        progress.print_timings();
    }
    Ok(super::exit_for(globals, &analysis))
}
