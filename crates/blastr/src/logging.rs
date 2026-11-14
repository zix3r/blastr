//! Logging setup, always writing to stderr.

use std::str::FromStr;

use tracing::Level;

/// Installs the logger; `BLASTR_LOG` overrides the verbosity flag.
pub fn init(verbose: u8) {
    let level = match verbose {
        0 => Level::WARN,
        1 => Level::INFO,
        2 => Level::DEBUG,
        _ => Level::TRACE,
    };
    let level = std::env::var("BLASTR_LOG")
        .ok()
        .and_then(|value| Level::from_str(&value.trim().to_ascii_lowercase()).ok())
        .unwrap_or(level);
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .init();
}
