//! The Blastr analysis engine.
//!
//! A console program that the window starts with no window of its own. It
//! reads requests from standard input, writes responses to standard output and
//! logs to standard error, which the window keeps in a file; it exits when its
//! input closes or the window asks it to stop.
//!
//! Everything here is a thin shell around [`blastr_core::service::server`], so
//! the engine can be tested without a process.

use std::io::BufReader;
use std::process::ExitCode;

use blastr_core::service::server;

fn main() -> ExitCode {
    init_logging();
    let stdin = BufReader::new(std::io::stdin());
    let stdout = std::io::BufWriter::new(std::io::stdout());
    match server::serve(stdin, stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("the engine stopped: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Sends the engine's log to standard error.
///
/// The window writes that stream to a file, so the level is `warn` unless
/// `BLASTR_LOG` says otherwise, and standard output stays free for responses.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("BLASTR_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
