//! The blastr binary: a desktop GUI without a command, a CLI with one.

mod cli;
mod commands;
#[cfg(feature = "gui")]
mod console;
mod exit;
mod logging;
mod output;
mod progress;

use clap::{CommandFactory, Parser};

use crate::cli::{CacheCommand, Cli, Command};
use crate::exit::Exit;

fn main() -> std::process::ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return if err.use_stderr() {
                Exit::Error.into()
            } else {
                Exit::Success.into()
            };
        }
    };

    logging::init(cli.globals.verbose);

    match run(cli) {
        Ok(exit) => exit.into(),
        Err(err) => report(&err),
    }
}

/// Dispatches the parsed command line.
fn run(cli: Cli) -> Result<Exit, blastr_core::Error> {
    let Cli { globals, command } = cli;
    match command {
        #[cfg(feature = "gui")]
        None => {
            console::detach_if_owned();
            blastr_gui::run(None);
            Ok(Exit::Success)
        }
        #[cfg(not(feature = "gui"))]
        None => {
            let _ = Cli::command().print_long_help();
            Ok(Exit::Success)
        }
        #[cfg(feature = "gui")]
        Some(Command::Gui { path }) => {
            console::detach_if_owned();
            blastr_gui::run(path);
            Ok(Exit::Success)
        }
        #[cfg(not(feature = "gui"))]
        Some(Command::Gui { .. }) => {
            eprintln!("error: this build of blastr has no GUI");
            eprintln!("hint: install the full build with `cargo install blastr`");
            Ok(Exit::Error)
        }
        Some(Command::Completions { shell }) => {
            let mut stdout = std::io::stdout();
            clap_complete::generate(shell, &mut Cli::command(), "blastr", &mut stdout);
            Ok(Exit::Success)
        }
        Some(Command::Diff(args)) => commands::diff::run(&globals, &args),
        Some(Command::Index { rev }) => commands::index::run(&globals, rev.as_deref()),
        Some(Command::Cache {
            command: CacheCommand::Status,
        }) => commands::cache::status(),
        Some(Command::Cache {
            command: CacheCommand::Clear,
        }) => commands::cache::clear(),
        Some(Command::Analyze(_)) => Err(blastr_core::Error::NotImplemented("analyze")),
        Some(Command::Tests(_)) => Err(blastr_core::Error::NotImplemented("tests")),
        Some(Command::Impact { .. }) => Err(blastr_core::Error::NotImplemented("impact")),
        Some(Command::Explain { .. }) => Err(blastr_core::Error::NotImplemented("explain")),
    }
}

/// Prints an error and its hint, then exits with the error code.
fn report(err: &blastr_core::Error) -> std::process::ExitCode {
    eprintln!("error: {err}");
    if let Some(hint) = err.hint() {
        eprintln!("hint: {hint}");
    }
    Exit::Error.into()
}
