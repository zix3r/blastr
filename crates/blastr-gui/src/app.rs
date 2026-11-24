//! Application startup: the key map, the window bounds, then the window.

use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext as _, Bounds, KeyBinding, WindowBounds, WindowOptions, point, px, size};

use crate::settings::Settings;
use crate::views::shell::{
    Back, Cancel, FocusFilter, Forward, OpenEditor, OpenPalette, OpenRepository, OpenSettings,
    Refresh, Shell, ToggleCode,
};

/// The key context the window's shortcuts are bound in.
const CONTEXT: &str = "Blastr";

/// Resolves the repository to show, then runs until the application quits.
pub(crate) fn run(start_dir: Option<PathBuf>) {
    let repository = start_dir
        .or_else(|| std::env::current_dir().ok())
        .and_then(|dir| blastr_git::discover(&dir).ok())
        .map(|root| root.path().to_path_buf());
    let placement = Settings::load(&Settings::path()).window;

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-o", OpenRepository, Some(CONTEXT)),
                KeyBinding::new("ctrl-p", OpenPalette, Some(CONTEXT)),
                KeyBinding::new("ctrl-r", Refresh, Some(CONTEXT)),
                KeyBinding::new("ctrl-f", FocusFilter, Some(CONTEXT)),
                KeyBinding::new("ctrl-m", ToggleCode, Some(CONTEXT)),
                KeyBinding::new("alt-left", Back, Some(CONTEXT)),
                KeyBinding::new("alt-right", Forward, Some(CONTEXT)),
                KeyBinding::new("ctrl-e", OpenEditor, Some(CONTEXT)),
                KeyBinding::new("ctrl-,", OpenSettings, Some(CONTEXT)),
                KeyBinding::new("escape", Cancel, Some(CONTEXT)),
            ]);

            // The last place of the window, when a screen still shows it.
            let default = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            let saved = placement.filter(|placement| {
                cx.displays().iter().any(|display| {
                    let screen = display.visible_bounds();
                    let corner = point(px(placement.x + 40.), px(placement.y + 20.));
                    screen.contains(&corner)
                })
            });
            let bounds = match saved {
                Some(placement) => {
                    let bounds = Bounds {
                        origin: point(px(placement.x), px(placement.y)),
                        size: size(
                            px(placement.width.max(900.)),
                            px(placement.height.max(600.)),
                        ),
                    };
                    if placement.maximized {
                        WindowBounds::Maximized(bounds)
                    } else {
                        WindowBounds::Windowed(bounds)
                    }
                }
                None => WindowBounds::Windowed(default),
            };
            let options = WindowOptions {
                window_bounds: Some(bounds),
                window_min_size: Some(size(px(900.), px(600.))),
                app_id: Some("blastr".to_owned()),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                crate::theme::apply(Some(window), cx);
                cx.new(|cx| Shell::new(repository, window, cx))
            })
            .expect("failed to open the Blastr window");

            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        });
}
