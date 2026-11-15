//! Logging setup, always writing to stderr.
//!
//! By default only warnings from Blastr itself are shown. The rust-analyzer
//! libraries log warnings of their own while they work around a missing
//! toolchain option, which is not the user's problem, so their output is kept
//! to errors unless the user asks for more.

use std::str::FromStr;

use tracing::Level;

/// Installs the logger.
///
/// Verbosity: nothing (`0`) shows Blastr's warnings, `-v` adds its progress
/// information, `-vv` adds debug output and `-vvv` adds traces from every
/// library. `BLASTR_LOG` overrides all of it with a `tracing` filter
/// directive, for example `BLASTR_LOG=blastr_core=debug`.
pub fn init(verbose: u8) {
    let level = match verbose {
        0 => Level::WARN,
        1 => Level::INFO,
        2 => Level::DEBUG,
        _ => Level::TRACE,
    };
    let filter = match std::env::var("BLASTR_LOG") {
        Ok(value) if !value.trim().is_empty() => value,
        // Library warnings are noise at the default verbosity; our own are not.
        _ if verbose == 0 => format!("{level},ra_ap_=error"),
        _ => level.to_string(),
    };
    let filter = tracing_subscriber::EnvFilter::from_str(&filter)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.to_string()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
