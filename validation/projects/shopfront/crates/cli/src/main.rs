//! The `shopfront` binary. All behaviour lives in the library target.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(cli::run(args))
}
