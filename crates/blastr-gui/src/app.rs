//! Application startup: the window bounds, the key map, then the window.

use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::{AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions, point, px, size};

use crate::settings::{ScreenRect, Settings};
use crate::views::shell::{
    Back, Dismiss, FocusFilter, Forward, OpenEditor, OpenPalette, OpenRepository, OpenSettings,
    Refresh, Shell, ShowCode, ShowDiff, ShowGraph,
};

/// The application name a desktop environment groups windows and the launcher
/// entry (`assets/linux/blastr.desktop`) by.
const APP_ID: &str = "blastr";

/// The key context the window's own shortcuts are bound in.
///
/// Every shortcut but `Esc` matches only while no overlay declares
/// [`MODAL_CONTEXT`]: the pressed key then belongs to the palette, the notice
/// or the settings panel, and the window behind them must not act on it. The
/// handlers of those shortcuts ask [`Shell::modal_open`] as well, because the
/// key context follows the focused element and an overlay without a field of
/// its own may not have taken the keyboard yet.
const SHORTCUT_CONTEXT: &str = "!BlastrModal";

/// Resolves the repository to show, then runs until the application quits.
pub(crate) fn run(start_dir: Option<PathBuf>) {
    let repository = discover_repository(start_dir);
    let settings = load_settings();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-o", OpenRepository, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new("ctrl-p", OpenPalette, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new("ctrl-r", Refresh, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new("ctrl-1", ShowGraph, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new("ctrl-2", ShowCode, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new("ctrl-3", ShowDiff, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new("ctrl-f", FocusFilter, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new("alt-left", Back, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new("alt-right", Forward, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new("ctrl-e", OpenEditor, Some(SHORTCUT_CONTEXT)),
                KeyBinding::new(
                    "ctrl-s",
                    crate::views::code::SaveFile,
                    Some(SHORTCUT_CONTEXT),
                ),
                KeyBinding::new("ctrl-,", OpenSettings, Some(SHORTCUT_CONTEXT)),
                // `Esc` is dispatched everywhere: it is what closes an overlay.
                KeyBinding::new("escape", Dismiss, None),
            ]);

            let bounds = restored_bounds(&settings, cx);
            let options = WindowOptions {
                window_bounds: Some(bounds),
                window_min_size: Some(size(px(900.), px(600.))),
                // The name a desktop environment matches `blastr.desktop` by,
                // so the window gets the packaged icon and launcher entry.
                app_id: Some(APP_ID.to_string()),
                ..TitleBar::window_options()
            };

            gpui_kit::open_window(options, cx, move |window, cx| {
                crate::theme::apply(Some(window), cx);
                let repository = repository.clone();
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

/// The settings of the window, when the configuration directory is known.
fn load_settings() -> Settings {
    Settings::load(&Settings::path_in(&crate::settings::config_dir()))
}

/// Where the window opens: where it was last time, made to fit the screens
/// that exist now, else centred at the default size.
///
/// A placement is only useful while the screen it was made on is still there,
/// so it is fitted: a window saved on a larger monitor, or on a monitor that
/// has since been unplugged, opens on the current one instead of off the
/// desktop.
fn restored_bounds(settings: &Settings, cx: &mut gpui_kit::App) -> WindowBounds {
    let default = Bounds::centered(None, size(px(1280.), px(800.)), cx);
    let Some(placement) = settings.window else {
        return WindowBounds::Windowed(default);
    };
    let placement = placement.fitted(&screen_rects(cx));
    let bounds = Bounds {
        origin: point(px(placement.x), px(placement.y)),
        size: size(px(placement.width), px(placement.height)),
    };
    if placement.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

/// The usable part of every screen, largest first as the platform lists them.
fn screen_rects(cx: &gpui_kit::App) -> Vec<ScreenRect> {
    cx.displays()
        .iter()
        .map(|display| {
            let bounds = display.visible_bounds();
            ScreenRect {
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
            }
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{KeyBindingContextPredicate, KeyContext};

    /// The context stack of an element inside an overlay, from the window
    /// down to what holds the keyboard.
    fn stack(contexts: &[&str]) -> Vec<KeyContext> {
        contexts
            .iter()
            .map(|context| KeyContext::parse(context).expect("a key context parses"))
            .collect()
    }

    #[test]
    fn the_shortcut_context_names_the_modal_context() {
        assert_eq!(
            SHORTCUT_CONTEXT,
            format!("!{}", crate::views::shell::MODAL_CONTEXT)
        );
    }

    #[test]
    fn the_window_shortcuts_stop_at_an_overlay() {
        let predicate =
            KeyBindingContextPredicate::parse(SHORTCUT_CONTEXT).expect("the context parses");
        // The window itself, and a field of it: the shortcuts match.
        assert!(predicate.eval(&stack(&["Blastr"])));
        assert!(predicate.eval(&stack(&["Blastr", "Input"])));
        // The palette, the notice and the settings panel each declare the
        // modal context, and no shortcut of the window matches inside them.
        assert!(!predicate.eval(&stack(&["Blastr", "BlastrModal"])));
        assert!(!predicate.eval(&stack(&["Blastr", "BlastrModal", "Input"])));
    }

    #[test]
    fn the_modal_context_is_alone_enough_to_stop_every_shortcut() {
        let predicate =
            KeyBindingContextPredicate::parse(SHORTCUT_CONTEXT).expect("the context parses");
        // The palette declares the modal context above its own input, and the
        // notice declares it above its buttons: both are covered.
        assert!(!predicate.eval(&stack(&["Blastr", "BlastrModal", "Input"])));
        assert!(!predicate.eval(&stack(&["Blastr", "BlastrModal", "Button"])));
    }
}
