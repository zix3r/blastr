//! `blastr index`: builds and caches the graph of one revision.

use blastr_core::{CancelToken, Engine};

use crate::cli::Globals;
use crate::exit::Exit;
use crate::progress::PhaseProgress;

/// Runs the command and prints what was indexed.
pub fn run(globals: &Globals, rev: Option<&str>) -> Result<Exit, blastr_core::Error> {
    let root = super::repository_root()?;
    let options = super::analysis_options(globals, &root);
    let progress = PhaseProgress::from_stderr();
    let cancel = CancelToken::new();

    let engine = Engine::open()?;
    let report = engine.index(&root, rev, &options, &progress, &cancel)?;
    progress.finish();

    let label = rev.unwrap_or("HEAD");
    let cached = if report.from_cache {
        " (from cache)"
    } else {
        ""
    };
    let symbols = report.symbols;
    let edges = report.edges;
    let unresolved = report.unresolved;
    let line = format!(
        "indexed {label}: {symbols} symbols, {edges} edges, {unresolved} unresolved references{cached}\n"
    );
    super::write_stdout(&line)?;
    if globals.timings {
        progress.print_timings();
    }
    Ok(Exit::Success)
}
