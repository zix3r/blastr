//! Application startup: resolve the repository, then open the main window.

use std::path::PathBuf;

use gpui_kit::{AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

use crate::views::shell::Shell;

/// Resolves the repository to show, then runs until the application quits.
pub(crate) fn run(start_dir: Option<PathBuf>) {
    let repository = discover_repository(start_dir);

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);

            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Blastr".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1200.), px(800.)),
                    cx,
                ))),
                window_min_size: Some(size(px(800.), px(500.))),
                ..Default::default()
            };

            gpui_kit::open_window(options, cx, move |_, cx| cx.new(|_| Shell::new(repository)))
                .expect("failed to open the Blastr window");

            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        });
}

/// Finds the working-tree root for `start_dir`, or for the current directory
/// when no start directory is given. Any failure means no repository.
fn discover_repository(start_dir: Option<PathBuf>) -> Option<PathBuf> {
    let dir = match start_dir {
        Some(dir) => dir,
        None => std::env::current_dir().ok()?,
    };

    blastr_git::discover(&dir)
        .ok()
        .map(|root| root.path().to_path_buf())
}
