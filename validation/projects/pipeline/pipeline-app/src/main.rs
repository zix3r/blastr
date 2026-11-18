//! Command-line entry point of the demo pipeline.

use std::process::ExitCode;

use pipeline_app::{DEFAULT_SPEC, SAMPLE_LOG, run_spec};

fn main() -> ExitCode {
    let spec = match std::env::args().nth(1) {
        Some(spec) => spec,
        None => DEFAULT_SPEC.to_string(),
    };
    match run_spec(&spec, SAMPLE_LOG) {
        Ok(report) => {
            print!("{}", report.render());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("pipeline failed ({}): {error}", error.kind());
            ExitCode::FAILURE
        }
    }
}
