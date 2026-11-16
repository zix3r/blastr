//! `blastr watch`: the analysis of the working tree, again after every save.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use blastr_core::CancelToken;
use blastr_model::RevSpec;

use crate::cli::Globals;
use crate::exit::Exit;
use crate::output::analysis::render_analysis;
use crate::progress::PhaseProgress;

use super::Setup;

/// Runs the command until the user stops it.
///
/// The project is loaded once; afterwards each save costs a re-extraction of
/// the files that changed, which is why this exists next to `blastr analyze`.
pub fn run(
    globals: &Globals,
    rev: Option<&str>,
    interval: u64,
    clear: bool,
) -> Result<Exit, blastr_core::Error> {
    let Some(setup) = super::setup(globals)? else {
        return Ok(Exit::Error);
    };
    let spec = RevSpec {
        rev: rev.map(str::to_owned),
        worktree: true,
        staged: false,
        parent: None,
    };
    let progress = PhaseProgress::from_stderr();
    let cancel = CancelToken::new();

    let mut session = setup.engine.session(
        &setup.root,
        &spec,
        &setup.options,
        &setup.params,
        &progress,
        &cancel,
    )?;
    progress.finish();
    let reports = Reports {
        clear: clear && std::io::stdout().is_terminal(),
        timings: globals.timings,
    };
    reports.show(&setup, session.analysis())?;
    eprintln!("watching the working tree; save a file to see its impact, or press Ctrl-C to stop");

    let interval = Duration::from_millis(interval.max(50));
    loop {
        std::thread::sleep(interval);
        if !session.is_stale()? {
            if session.base_moved()? {
                let progress = PhaseProgress::from_stderr();
                session = setup.engine.session(
                    &setup.root,
                    &spec,
                    &setup.options,
                    &setup.params,
                    &progress,
                    &cancel,
                )?;
                progress.finish();
                reports.show(&setup, session.analysis())?;
            }
            continue;
        }

        let progress = PhaseProgress::from_stderr();
        let started = Instant::now();
        match session.refresh(&progress, &cancel) {
            Ok(refresh) => {
                progress.finish();
                reports.show_refresh(
                    &setup,
                    &refresh.analysis,
                    started.elapsed(),
                    refresh.changed_files,
                )?;
                if globals.timings {
                    progress.print_timings();
                }
            }
            // A file that does not parse, or a build script that fails, must not
            // end the session: the developer is about to fix it.
            Err(err) => {
                let hint = err
                    .hint()
                    .map(|hint| format!(" (hint: {hint})"))
                    .unwrap_or_default();
                eprintln!("error: {err}{hint}");
            }
        }
    }
}

/// How a report is shown between refreshes.
struct Reports {
    /// Whether to clear the screen before each report.
    clear: bool,
    /// Whether to report how long the refresh took.
    timings: bool,
}

impl Reports {
    /// Prints the report of a refresh, replacing the previous one.
    fn show(
        &self,
        setup: &Setup,
        analysis: &blastr_model::Analysis,
    ) -> Result<(), blastr_core::Error> {
        self.write(&render_analysis(analysis, &setup.options, &setup.style))
    }

    /// Prints the report of a refresh that followed a save.
    fn show_refresh(
        &self,
        setup: &Setup,
        analysis: &blastr_model::Analysis,
        elapsed: Duration,
        changed_files: usize,
    ) -> Result<(), blastr_core::Error> {
        let noun = if changed_files == 1 { "file" } else { "files" };
        if self.timings {
            eprintln!(
                "re-analysed {changed_files} changed {noun} in {:.2}s",
                elapsed.as_secs_f64()
            );
        }
        self.show(setup, analysis)
    }

    /// Replaces the screen with `text` when that is wanted.
    fn write(&self, text: &str) -> Result<(), blastr_core::Error> {
        let mut out = String::new();
        if self.clear {
            out.push_str("\u{1b}[2J\u{1b}[H");
        }
        out.push_str(text);
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(out.as_bytes())?;
        stdout.flush()?;
        Ok(())
    }
}
