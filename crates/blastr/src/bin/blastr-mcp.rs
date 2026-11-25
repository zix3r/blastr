//! The Blastr MCP server.
//!
//! An AI agent's MCP client starts this program and speaks the Model Context
//! Protocol with it over standard input and output. It asks the open Blastr
//! window for everything, over the per-user socket the window listens on, so
//! the window must be running. Standard output carries only the protocol; the
//! log goes to standard error.
//!
//! Everything here is a thin shell around [`blastr_mcp::serve_stdio`].

use std::process::ExitCode;

use blastr_model::bridge;

#[tokio::main]
async fn main() -> ExitCode {
    init_logging();
    match blastr_mcp::serve_stdio(bridge::socket_path()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("the MCP server stopped: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Sends the log to standard error, at `warn` unless `BLASTR_LOG` says
/// otherwise, so standard output stays free for the protocol.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("BLASTR_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}
