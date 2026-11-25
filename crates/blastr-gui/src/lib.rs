//! Blastr's desktop interface.
//!
//! `workspace::Workspace` holds the state every view reads: the session, the
//! settings and the connection to the analysis engine. The views in `views`
//! are separate entities that read it and call its transitions.

mod agents;
mod app;
mod editor;
mod engine;
mod settings;
mod state;
mod theme;
mod views;
mod workspace;

/// Opens the main window and blocks until the application quits.
///
/// `start_dir` is the directory to look for a repository in; `None` means
/// the current directory.
pub fn run(start_dir: Option<std::path::PathBuf>) {
    app::run(start_dir)
}
