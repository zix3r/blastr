//! The MCP server that lets AI agents use Blastr.
//!
//! An MCP client (Claude Code, Claude Desktop, Cursor, VS Code, …) starts
//! `blastr-mcp` and talks to it over standard input and output. The server has
//! no analysis of its own: it forwards every question to the open Blastr
//! window over the bridge in [`blastr_model::bridge`], and shapes the answers
//! as a short text plus the same data as JSON. The window does the work where
//! the user can see it.

#![forbid(unsafe_code)]

mod server;
mod shape;
mod window;

use std::path::PathBuf;

use rmcp::ServiceExt;

pub use server::Server;
pub use window::NOT_RUNNING;

/// Serves one MCP client on standard input and output until it disconnects,
/// asking the window that listens on `socket`.
pub async fn serve_stdio(socket: PathBuf) -> std::io::Result<()> {
    let running = Server::new(socket)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(std::io::Error::other)?;
    running.waiting().await.map_err(std::io::Error::other)?;
    Ok(())
}
