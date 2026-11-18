//! Blastr's desktop interface.

mod app;
mod editor;
mod settings;
mod state;
mod theme;
mod views;

/// Opens the main window and blocks until the application quits.
///
/// `start_dir` is the directory to look for a repository in; `None` means
/// the current directory.
pub fn run(start_dir: Option<std::path::PathBuf>) {
    app::run(start_dir)
}
