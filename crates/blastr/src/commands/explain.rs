//! `blastr explain`: why one item is in the result.

use blastr_core::CancelToken;

use crate::cli::{Globals, RevArgs};
use crate::exit::Exit;
use crate::output::analysis::render_explain;
use crate::progress::PhaseProgress;

/// Runs the same analysis as `blastr analyze` and explains one item of it.
///
/// The analysis is served from the result cache when `blastr analyze` already
/// computed it. A query that names no item stops the run with the message the
/// renderer gives.
pub fn run(globals: &Globals, symbol: &str, args: &RevArgs) -> Result<Exit, blastr_core::Error> {
    if let Err(problem) = super::explain_format(globals.format) {
        problem.report();
        return Ok(Exit::Error);
    }
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

    let text = match render_explain(&analysis, symbol, &setup.style) {
        Ok(text) => text,
        Err(message) => {
            eprintln!("error: {message}");
            let hint = if message.contains("matches several") {
                "write more of the path, such as `module::item`"
            } else {
                "run `blastr analyze` to see every affected item of this comparison"
            };
            eprintln!("hint: {hint}");
            return Ok(Exit::Error);
        }
    };
    super::write_stdout(&text)?;
    if globals.timings {
        progress.print_timings();
    }
    Ok(Exit::Success)
}
