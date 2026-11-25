//! The one dark theme: surface and text colours, and one colour per meaning.
//!
//! Orange is a change (and the brand), amber is *needs editing*, blue is *may
//! behave differently*, green is tests, violet is a hypothetical change.
//! Meaning is never carried by colour alone; badges and icons say it in words.

use std::sync::{Arc, OnceLock};

use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, Fill, Hsla, Pixels, Rgba, SharedString, Window, px};

/// A colour of the palette, as `0xRRGGBBAA`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tone(u32);

impl Tone {
    /// An opaque colour from `0xRRGGBB`.
    pub(crate) const fn rgb(hex: u32) -> Tone {
        Tone((hex << 8) | 0xff)
    }

    /// A colour from `0xRRGGBB` and an alpha between 0 and 255.
    pub(crate) const fn rgba(hex: u32, alpha: u8) -> Tone {
        Tone((hex << 8) | alpha as u32)
    }

    /// The colour as GPUI's HSLA.
    pub(crate) fn hsla(self) -> Hsla {
        Rgba::from(self).into()
    }
}

impl From<Tone> for Rgba {
    fn from(tone: Tone) -> Rgba {
        gpui_kit::rgba(tone.0)
    }
}

impl From<Tone> for Hsla {
    fn from(tone: Tone) -> Hsla {
        tone.hsla()
    }
}

impl From<Tone> for Fill {
    fn from(tone: Tone) -> Fill {
        tone.hsla().into()
    }
}

/// The colours of one meaning: a solid mark, text, a tinted fill and a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Accent {
    /// Marks, bars and selected borders.
    pub(crate) solid: Tone,
    /// Text on a dark surface.
    pub(crate) fg: Tone,
    /// A tinted fill behind text.
    pub(crate) bg: Tone,
    /// A border around a tinted fill.
    pub(crate) line: Tone,
}

/// Title bar and status bar.
pub(crate) const BG_BAR: Tone = Tone::rgb(0x0d0f12);
/// Side panels.
pub(crate) const BG_PANEL: Tone = Tone::rgb(0x111317);
/// The centre: graph, code and diff.
pub(crate) const BG_CANVAS: Tone = Tone::rgb(0x0b0c0f);
/// Chips, graph nodes, menus and dialogs.
pub(crate) const BG_RAISED: Tone = Tone::rgb(0x171a1f);
/// A step above [`BG_RAISED`]: count badges and tooltips.
pub(crate) const BG_RAISED_2: Tone = Tone::rgb(0x1c2026);
/// Text inputs.
pub(crate) const BG_INPUT: Tone = Tone::rgb(0x0c0e11);
/// A hovered row.
pub(crate) const BG_HOVER: Tone = Tone::rgb(0x191d23);
/// A pressed or active control.
pub(crate) const BG_ACTIVE: Tone = Tone::rgb(0x1e232b);
/// A selected row.
pub(crate) const BG_SELECTED: Tone = Tone::rgb(0x1c2330);
/// Ordinary rules between areas.
pub(crate) const LINE: Tone = Tone::rgb(0x20242b);
/// Rules that separate blocks.
pub(crate) const LINE_STRONG: Tone = Tone::rgb(0x2b313a);
/// The faintest rules.
pub(crate) const LINE_SOFT: Tone = Tone::rgb(0x181b20);

/// Primary text.
pub(crate) const FG: Tone = Tone::rgb(0xe7eaee);
/// Secondary text.
pub(crate) const FG_2: Tone = Tone::rgb(0xb8c0ca);
/// Muted text.
pub(crate) const FG_3: Tone = Tone::rgb(0x929ca8);
/// The quietest text that is still readable.
pub(crate) const FG_4: Tone = Tone::rgb(0x77818d);

/// A change, and the brand.
pub(crate) const CHANGED: Accent = Accent {
    solid: Tone::rgb(0xff7a33),
    fg: Tone::rgb(0xff8d4f),
    bg: Tone::rgba(0xff7a33, 28),
    line: Tone::rgba(0xff7a33, 128),
};
/// Needs editing.
pub(crate) const EDIT: Accent = Accent {
    solid: Tone::rgb(0xf2c14e),
    fg: Tone::rgb(0xf2c14e),
    bg: Tone::rgba(0xf2c14e, 26),
    line: Tone::rgba(0xf2c14e, 115),
};
/// May behave differently.
pub(crate) const BEHAVE: Accent = Accent {
    solid: Tone::rgb(0x5aa7ff),
    fg: Tone::rgb(0x6fb2ff),
    bg: Tone::rgba(0x5aa7ff, 28),
    line: Tone::rgba(0x5aa7ff, 128),
};
/// Tests.
pub(crate) const TEST: Accent = Accent {
    solid: Tone::rgb(0x4cc38a),
    fg: Tone::rgb(0x56c993),
    bg: Tone::rgba(0x4cc38a, 26),
    line: Tone::rgba(0x4cc38a, 115),
};
/// A hypothetical change.
pub(crate) const WHATIF: Accent = Accent {
    solid: Tone::rgb(0xa78bfa),
    fg: Tone::rgb(0xb9a3fb),
    bg: Tone::rgba(0xa78bfa, 28),
    line: Tone::rgba(0xa78bfa, 128),
};
/// A symbol nothing reaches: neutral.
pub(crate) const NEUTRAL: Accent = Accent {
    solid: Tone::rgb(0x8b93a1),
    fg: Tone::rgb(0xb8c0ca),
    bg: Tone::rgb(0x1c2026),
    line: Tone::rgb(0x2b313a),
};

/// Candidate edges and `possible` confidence: muted, and dashed where drawn.
pub(crate) const POSSIBLE: Tone = Tone::rgb(0x8b93a1);

/// Removed lines and failures.
pub(crate) const DEL_FG: Tone = Tone::rgb(0xf0616d);

/// The fill of the one primary button of a view.
pub(crate) const PRIMARY_BG: Tone = Tone::rgb(0xe9ecf0);
/// Text on the primary button.
pub(crate) const PRIMARY_FG: Tone = Tone::rgb(0x101216);
/// The primary button under the pointer.
pub(crate) const PRIMARY_HOVER: Tone = Tone::rgb(0xffffff);
/// What dims the window behind a popup.
pub(crate) const SCRIM: Tone = Tone::rgba(0x050608, 158);

/// Size of ordinary interface text.
pub(crate) const TEXT_UI: Pixels = px(13.);
/// Size of secondary text.
pub(crate) const TEXT_SMALL: Pixels = px(12.);
/// Size of labels and counts.
pub(crate) const TEXT_MICRO: Pixels = px(11.);
/// Size of symbol names.
pub(crate) const TEXT_MONO: Pixels = px(12.5);
/// Size of source text in the code pane at 100% zoom.
pub(crate) const CODE_TEXT: Pixels = px(15.);
/// The code pane's zoom steps, in percent.
pub(crate) const CODE_ZOOMS: [u16; 11] = [60, 70, 80, 90, 100, 110, 125, 150, 175, 200, 250];

/// The zoom step nearest to `percent`; 0 (never chosen) is 100%.
pub(crate) fn code_zoom(percent: u16) -> u16 {
    if percent == 0 {
        return 100;
    }
    CODE_ZOOMS
        .into_iter()
        .min_by_key(|step| step.abs_diff(percent))
        .unwrap_or(100)
}

/// The zoom step after `percent`, one step bigger or smaller.
pub(crate) fn step_code_zoom(percent: u16, bigger: bool) -> u16 {
    let current = code_zoom(percent);
    let at = CODE_ZOOMS
        .iter()
        .position(|step| *step == current)
        .unwrap_or(4);
    let next = if bigger {
        (at + 1).min(CODE_ZOOMS.len() - 1)
    } else {
        at.saturating_sub(1)
    };
    CODE_ZOOMS[next]
}

/// Sizes the code pane's text. The diff view reads the library's mono size,
/// which nothing else on screen uses.
pub(crate) fn set_code_zoom(percent: u16, cx: &mut App) {
    let scale = f32::from(code_zoom(percent)) / 100.;
    Theme::update(cx, |theme| theme.mono_font_size = CODE_TEXT * scale);
    cx.refresh_windows();
}
/// Corner radius of small tags.
pub(crate) const RADIUS_SM: Pixels = px(4.);
/// Corner radius of chips, buttons and inputs.
pub(crate) const RADIUS_MD: Pixels = px(6.);
/// Corner radius of graph nodes and cards.
pub(crate) const RADIUS_LG: Pixels = px(8.);

/// The families the interface is set in, in the order they are tried.
const UI_FAMILIES: &[&str] = if cfg!(windows) {
    &["Segoe UI", "Inter"]
} else {
    &["Inter", "Noto Sans", "DejaVu Sans"]
};

/// The families source text is set in, in the order they are tried.
const MONO_FAMILIES: &[&str] = if cfg!(windows) {
    &["Cascadia Mono", "Consolas"]
} else {
    &["JetBrains Mono", "DejaVu Sans Mono", "Liberation Mono"]
};

/// The family GPUI resolves to the platform's own interface font.
const SYSTEM_UI_FONT: &str = ".SystemUIFont";

/// The monospace family, resolved from the fonts of the machine once.
static MONO_FAMILY: OnceLock<SharedString> = OnceLock::new();

/// The monospace family code, line numbers and symbol names are set in.
///
/// This is the family [`apply`] chose, which the machine is known to have. A
/// view drawn before the theme is applied gets the platform's own family
/// rather than a name GPUI might not find.
pub(crate) fn mono() -> SharedString {
    MONO_FAMILY
        .get()
        .cloned()
        .unwrap_or_else(|| SYSTEM_UI_FONT.into())
}

/// The first of `candidates` that the machine has, else `fallback`.
///
/// A family name the text system does not know makes GPUI fail on the first
/// line it lays out, so a name is only used when `installed` lists it. An
/// empty `installed` means the question cannot be answered: the first
/// candidate is then used as-is.
fn first_installed<'a>(candidates: &[&'a str], installed: &[String], fallback: &'a str) -> &'a str {
    let Some(first) = candidates.first().copied() else {
        return fallback;
    };
    if installed.is_empty() {
        return first;
    }
    candidates
        .iter()
        .copied()
        .find(|candidate| installed.iter().any(|name| name == candidate))
        .unwrap_or(fallback)
}

/// Applies the dark theme to the component library.
///
/// The library's own widgets (inputs, menus, dialogs, tooltips, scroll bars)
/// read these colours, so they match the surfaces drawn by hand.
pub(crate) fn apply(window: Option<&mut Window>, cx: &mut App) {
    Theme::change(ThemeMode::Dark, window, cx);
    let installed = cx.text_system().all_font_names();
    let ui: SharedString = first_installed(UI_FAMILIES, &installed, SYSTEM_UI_FONT).into();
    let mono: SharedString = first_installed(MONO_FAMILIES, &installed, SYSTEM_UI_FONT).into();
    let _ = MONO_FAMILY.set(mono.clone());
    Theme::update(cx, |theme| {
        theme.font_family = ui;
        theme.mono_font_family = mono;
        let colors = &mut theme.colors;
        colors.background = BG_CANVAS.hsla();
        colors.foreground = FG.hsla();
        colors.border = LINE.hsla();
        colors.input = LINE_STRONG.hsla();
        colors.ring = BEHAVE.solid.hsla();
        colors.caret = FG.hsla();
        colors.selection = Tone::rgba(0x5aa7ff, 70).hsla();
        colors.muted = BG_RAISED_2.hsla();
        colors.muted_foreground = FG_3.hsla();
        colors.accent = BG_ACTIVE.hsla();
        colors.accent_foreground = FG.hsla();
        colors.popover = BG_RAISED.hsla();
        colors.popover_foreground = FG.hsla();
        colors.secondary = BG_RAISED.hsla();
        colors.secondary_foreground = FG.hsla();
        colors.secondary_hover = BG_HOVER.hsla();
        colors.secondary_active = BG_ACTIVE.hsla();
        colors.primary = PRIMARY_BG.hsla();
        colors.primary_foreground = PRIMARY_FG.hsla();
        colors.primary_hover = PRIMARY_HOVER.hsla();
        colors.primary_active = FG_2.hsla();
        colors.button_primary = PRIMARY_BG.hsla();
        colors.button_primary_foreground = PRIMARY_FG.hsla();
        colors.button_primary_hover = PRIMARY_HOVER.hsla();
        colors.button_primary_active = FG_2.hsla();
        colors.list = BG_PANEL.hsla();
        colors.list_hover = BG_HOVER.hsla();
        colors.list_active = BG_SELECTED.hsla();
        colors.list_active_border = CHANGED.solid.hsla();
        colors.title_bar = BG_BAR.hsla();
        colors.title_bar_border = LINE.hsla();
        colors.status_bar = BG_BAR.hsla();
        colors.status_bar_border = LINE.hsla();
        colors.scrollbar = Tone::rgba(0x000000, 0).hsla();
        colors.scrollbar_thumb = LINE_STRONG.hsla();
        colors.scrollbar_thumb_hover = FG_4.hsla();
        // Shown whenever there is more to scroll to. The default shows them
        // only while scrolling, which hides long code lines: a plain mouse
        // wheel never scrolls sideways, so that bar would never appear.
        theme.scrollbar_mode = ScrollbarMode::Always;
        colors.overlay = SCRIM.hsla();
        colors.info = BEHAVE.solid.hsla();
        colors.success = TEST.solid.hsla();
        colors.warning = EDIT.solid.hsla();
        colors.danger = DEL_FG.hsla();
        colors.link = BEHAVE.fg.hsla();
        colors.drag_border = CHANGED.solid.hsla();
        colors.drop_target = CHANGED.bg.hsla();
        colors.skeleton = BG_RAISED_2.hsla();
        // The settings dialog's category list: a side panel like the
        // window's own, its selected page a selected row.
        colors.sidebar = BG_PANEL.hsla();
        colors.sidebar_foreground = FG_2.hsla();
        colors.sidebar_border = LINE.hsla();
        colors.sidebar_accent = BG_SELECTED.hsla();
        colors.sidebar_accent_foreground = FG.hsla();
        colors.sidebar_primary = CHANGED.solid.hsla();
        colors.sidebar_primary_foreground = FG.hsla();
        colors.group_box = BG_RAISED.hsla();
        colors.group_box_foreground = FG.hsla();
        // A switch's track is dark when off; the switches fill it when on.
        colors.switch = LINE_STRONG.hsla();
        colors.switch_thumb = FG.hsla();
        theme.radius = RADIUS_MD;
        theme.radius_lg = RADIUS_LG;
        theme.font_size = TEXT_UI;
        theme.mono_font_size = CODE_TEXT;
        if let Ok(highlight) = serde_json::from_str::<HighlightTheme>(HIGHLIGHT) {
            theme.highlight_theme = Arc::new(highlight);
        }
    });
}

/// The colours of source text: the editor surface and one colour per kind of
/// token, in the shape of a Zed theme.
const HIGHLIGHT: &str = r##"{
  "name": "Blastr",
  "appearance": "dark",
  "style": {
    "editor.background": "#0b0c0f",
    "editor.foreground": "#b8c0ca",
    "editor.active_line.background": "#111317",
    "editor.line_number": "#5f6975",
    "editor.active_line_number": "#b8c0ca",
    "editor.gutter.background": "#0b0c0f",
    "created": "#56c993",
    "deleted": "#f0616d",
    "modified": "#f2c14e",
    "syntax": {
      "attribute": { "color": "#eab676" },
      "boolean": { "color": "#eab676" },
      "comment": { "color": "#5f6975" },
      "comment.doc": { "color": "#6f7a86" },
      "constant": { "color": "#eab676" },
      "constructor": { "color": "#5fcfc0" },
      "function": { "color": "#7fb6ff" },
      "keyword": { "color": "#c49bea" },
      "label": { "color": "#eab676" },
      "number": { "color": "#eab676" },
      "operator": { "color": "#b8c0ca" },
      "property": { "color": "#b8c0ca" },
      "punctuation": { "color": "#929ca8" },
      "punctuation.bracket": { "color": "#929ca8" },
      "punctuation.delimiter": { "color": "#929ca8" },
      "string": { "color": "#a6d189" },
      "string.escape": { "color": "#eab676" },
      "type": { "color": "#5fcfc0" },
      "variable": { "color": "#e7eaee" },
      "variable.special": { "color": "#c49bea" }
    }
  }
}"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_zoom_snaps_to_steps_and_stops_at_the_ends() {
        assert_eq!(code_zoom(0), 100);
        assert_eq!(code_zoom(103), 100);
        assert_eq!(code_zoom(1000), 250);
        assert_eq!(step_code_zoom(100, true), 110);
        assert_eq!(step_code_zoom(100, false), 90);
        assert_eq!(step_code_zoom(250, true), 250);
        assert_eq!(step_code_zoom(60, false), 60);
        assert_eq!(step_code_zoom(0, true), 110);
    }
}
