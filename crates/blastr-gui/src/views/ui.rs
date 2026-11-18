//! Small pieces every view shares: markers, badges, headings and buttons.
//!
//! Meaning is always carried twice: by the colour of these pieces and by the
//! words they show.

use gpui_kit::prelude::*;

use std::sync::{Arc, LazyLock};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::{
    Scrollbar, ScrollbarAxis, ScrollbarHandle, ScrollbarMode, ScrollbarThumbStyle,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::theme::ActiveTheme as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Div, ElementId, Hsla, Image, ImageFormat, IntoElement, ParentElement, Pixels,
    SharedString, Size, Stateful, Styled, div, img, px,
};

use crate::theme::{self, Accent, RADIUS_SM, TEXT_MICRO, TEXT_SMALL, Tone};

/// The family symbols, code, line numbers and hashes are drawn in.
pub(crate) fn mono(cx: &App) -> SharedString {
    cx.theme().mono_font_family.clone()
}

/// The width of the gutter that leads a row.
///
/// Every marker and every leading icon sits in this one column, so the text of
/// a change row, an impact row, a file row and an evidence step all starts at
/// the same x.
pub(crate) const GUTTER: Pixels = px(14.);

/// The height of a row's first line, and of every badge that sits on it.
///
/// Mono and interface text in one row are laid out at this height, so they
/// share a centre line with the marker in the gutter, which is drawn in a box
/// this tall.
pub(crate) const ROW_LINE: Pixels = px(18.);

/// The height of a secondary line, such as the reason under an impact item.
pub(crate) const SUB_LINE: Pixels = px(14.);

/// The space between a row's gutter and the text that follows it.
pub(crate) const GUTTER_GAP: Pixels = px(8.);

/// Where a row's second line starts: under the text, not under the marker.
pub(crate) const SECOND_LEAD: Pixels = px(22.);

/// The height of a badge, pill or tag on a row's first line.
pub(crate) const PILL: Pixels = px(18.);

/// The horizontal padding inside a badge, the same on every side of it.
pub(crate) const PILL_PAD: Pixels = px(6.);

/// The width of the right-aligned line-number column of a change row.
pub(crate) const LINE_NUMBER_W: Pixels = px(38.);

/// The left padding of a row's content, measured after its 2 px rule.
pub(crate) const LEAD: Pixels = px(11.);

/// The left padding of a row that carries no 2 px rule, so that both kinds of
/// row start their text at one column.
pub(crate) const LEAD_PLAIN: Pixels = px(13.);

/// The fixed-width leading column of a row.
///
/// Its content is centred both ways in one line's height. A row that holds
/// several lines uses `items_start` and still centres its marker on the first
/// line; a row of one line uses `items_center`.
pub(crate) fn gutter(child: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(GUTTER)
        .h(ROW_LINE)
        .child(child)
}

/// The brand mark, decoded from the logo file shipped with the source.
///
/// The mark is a gradient, and `svg()` draws a single-colour mask, so the file
/// is loaded as an image instead: GPUI rasterises the SVG once, at the 256 px
/// it was drawn for, and the element scales that picture down. The decoded
/// image is kept for the life of the process, so a redraw reuses it.
static MARK: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Svg,
        include_bytes!("../../assets/blastr.svg").to_vec(),
    ))
});

/// The Blastr mark at `size`, in the brand's own colours.
pub(crate) fn logo(size: gpui_kit::Pixels) -> AnyElement {
    img(MARK.clone())
        .w(size)
        .h(size)
        .object_fit(gpui_kit::ObjectFit::Contain)
        .into_any_element()
}

/// A small key cap such as the `Ctrl P` hint of the search box.
pub(crate) fn keycap(text: impl Into<SharedString>, _cx: &App) -> AnyElement {
    div()
        .flex()
        .items_center()
        .h(px(17.))
        .px_1()
        .rounded(RADIUS_SM)
        .border_1()
        .border_color(theme::LINE_STRONG)
        .bg(theme::BG_RAISED)
        .text_size(px(10.5))
        .text_color(theme::FG_3)
        .child(text.into())
        .into_any_element()
}

/// An uppercase block header with its icon, label and count.
///
/// The header is a clickable strip with a coloured rule on its left edge; the
/// label is set in semibold small caps and the count sits at the right end in
/// the block's colour.
pub(crate) fn block_head(
    label: &str,
    count: Option<usize>,
    icon: IconName,
    accent: Accent,
    raised: bool,
    collapsed: bool,
    cx: &App,
) -> AnyElement {
    let _ = cx;
    h_flex()
        .h(px(crate::state::outline::height::HEAD))
        .w_full()
        .pl(px(9.))
        .pr(px(10.))
        .gap_1()
        .items_center()
        .bg(if raised {
            theme::BG_RAISED
        } else {
            theme::BG_PANEL
        })
        .border_b_1()
        .when(!raised, |this| this.border_t_1())
        .border_color(if raised {
            theme::LINE_STRONG
        } else {
            theme::LINE
        })
        // The colour bar on the left edge says which block this is; the cause
        // carries it over the full height of the header, the effects inset it.
        .child(
            div()
                .w(px(2.))
                .h(px(16.))
                .when(raised, |this| this.h_full())
                .rounded(px(1.))
                .bg(accent.solid),
        )
        .child(div().w(px(2.)))
        .child(
            Icon::new(if collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .size_3()
            .text_color(theme::FG_4),
        )
        .child(Icon::new(icon).size_3().text_color(accent.solid))
        .child(
            div()
                .text_size(TEXT_MICRO)
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(if raised { theme::FG } else { theme::FG_2 })
                .truncate()
                .child(label.to_owned()),
        )
        .child(div().flex_1())
        .children(count.map(|count| {
            div()
                .flex_none()
                .h(px(16.))
                .px(px(5.))
                .rounded(RADIUS_SM)
                .bg(accent.bg)
                .text_color(accent.fg)
                .text_size(px(10.5))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(count.to_string())
        }))
        .into_any_element()
}

/// The super-header that separates the cause from its effects.
pub(crate) fn group_head(label: &str, icon: IconName) -> AnyElement {
    h_flex()
        .h(px(28.))
        .w_full()
        .pl(px(9.))
        .pr(px(10.))
        .gap_2()
        .items_center()
        .border_t_1()
        .border_color(theme::LINE_STRONG)
        .child(Icon::new(icon).size_3().text_color(theme::FG_4))
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(theme::FG_4)
                .child(label.to_owned()),
        )
        .child(div().flex_1().h(px(1.)).bg(theme::LINE))
        .into_any_element()
}

/// The five shapes a list, the graph and the evidence panel share.
///
/// The shape is drawn in the gutter, so it shares its x and its vertical
/// centre with the first line of the row's text.
pub(crate) fn marker(colour: Tone, shape: Marker) -> AnyElement {
    let drawn = shape.draw();
    let shape_el = div()
        .w(px(drawn.size))
        .h(px(drawn.size))
        .rounded(px(drawn.radius))
        .border(px(drawn.border))
        .border_color(colour)
        .when(drawn.filled, |this| this.bg(colour))
        .when(drawn.dashed, |this| this.border_dashed());
    gutter(shape_el).into_any_element()
}

/// A badge on a row's first line: a class, a kind or a confidence.
pub(crate) fn pill(text: impl Into<SharedString>, accent: Accent) -> AnyElement {
    pill_box(text.into(), accent.bg, accent.fg, None)
}

/// The same badge with a border in its own colour, for a panel that has room
/// for a heavier mark.
pub(crate) fn pill_outlined(text: impl Into<SharedString>, accent: Accent) -> AnyElement {
    pill_box(text.into(), accent.bg, accent.fg, Some(accent.line))
}

/// A quiet tag, for words that are neither a class nor a kind.
pub(crate) fn tag(text: impl Into<SharedString>) -> AnyElement {
    pill_box(
        text.into(),
        theme::BG_RAISED_2,
        theme::FG_3,
        Some(theme::LINE),
    )
}

/// The one pill body every badge is drawn on.
///
/// One height, one padding and one radius, so that a column of badges shares
/// its edges and each badge sits on the height of the row's first line. Only
/// the edge differs, and a badge without one keeps the same box.
fn pill_box(text: SharedString, fill: Tone, colour: Tone, edge: Option<Tone>) -> AnyElement {
    let mut pill = div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(PILL)
        .px(PILL_PAD)
        .rounded(RADIUS_SM)
        .bg(fill)
        .text_size(px(10.5))
        .text_color(colour)
        .whitespace_nowrap()
        .overflow_hidden();
    if let Some(edge) = edge {
        pill = pill.border_1().border_color(edge);
    }
    pill.child(text).into_any_element()
}

/// Which of the shared markers to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marker {
    /// A change: a hollow ring.
    Changed,
    /// Needs editing: a filled square.
    Diamond,
    /// May behave differently: a filled circle.
    Dot,
    /// A candidate or `possible` step: a hollow, dashed ring.
    Candidate,
}

/// How one marker shape is drawn.
///
/// The size is whole pixels and the same across the shapes that share it, so
/// no marker reads as larger than another.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MarkerBox {
    /// Width and height of the shape.
    size: f32,
    /// Corner radius; half the size for a circle.
    radius: f32,
    /// Width of the edge.
    border: f32,
    /// Whether the shape is filled.
    filled: bool,
    /// Whether its edge is dashed, as a `possible` step's is.
    dashed: bool,
}

impl Marker {
    /// The box this shape is drawn in.
    pub(crate) const fn draw(self) -> MarkerBox {
        match self {
            Marker::Changed => MarkerBox {
                size: 8.0,
                radius: 4.0,
                border: 1.5,
                filled: false,
                dashed: false,
            },
            Marker::Diamond => MarkerBox {
                size: 8.0,
                radius: 2.0,
                border: 1.0,
                filled: true,
                dashed: false,
            },
            Marker::Dot => MarkerBox {
                size: 6.0,
                radius: 3.0,
                border: 1.0,
                filled: true,
                dashed: false,
            },
            Marker::Candidate => MarkerBox {
                size: 8.0,
                radius: 4.0,
                border: 1.0,
                filled: false,
                dashed: true,
            },
        }
    }
}

/// The scrollbar every scrolling view draws over its own edge.
///
/// `handle` is the scroll handle the view already tracks its scroll area with,
/// so the bar follows the wheel, the keyboard and any code that moves the
/// area. It is drawn over the content, never beside it: nothing is pushed
/// aside. An axis that has nothing to scroll draws nothing and cannot be
/// grabbed.
///
/// The caller puts the bar in a box that is `relative` and exactly as large as
/// the scroll area, so that the bar reaches the corner of the content it
/// belongs to.
pub(crate) fn scrollbar<H: ScrollbarHandle + Clone>(
    id: &str,
    handle: &H,
    axis: ScrollbarAxis,
) -> AnyElement {
    bar(id, handle, axis, None)
}

/// The same bar for a shift that no scroll area carries.
///
/// The before/after view moves both sides sideways by an offset of its own, so
/// there is no scroll area to measure: the content is as long as the view says
/// it is, and the bar is the only thing that shows where the sides are.
pub(crate) fn scrollbar_of<H: ScrollbarHandle + Clone>(
    id: &str,
    handle: &H,
    axis: ScrollbarAxis,
    content: Size<Pixels>,
) -> AnyElement {
    bar(id, handle, axis, Some(content))
}

/// Draws the bar itself, with the track and thumb the palette asks for.
fn bar<H: ScrollbarHandle + Clone>(
    id: &str,
    handle: &H,
    axis: ScrollbarAxis,
    content: Option<Size<Pixels>>,
) -> AnyElement {
    let rest = theme::FG_4.hsla().alpha(scroll::THUMB_ALPHA);
    let hot = theme::FG_4.hsla().alpha(scroll::THUMB_HOT_ALPHA);
    let mut element = Scrollbar::new(handle)
        .id(ElementId::Name(format!("scrollbar-{id}").into()))
        .axis(axis)
        // A bar that is always there is read as furniture rather than as a
        // reaction to the pointer, and a bar that has nothing to scroll is not
        // drawn at all.
        .mode(ScrollbarMode::Always)
        .viewport_from_layout()
        .styles(|styles| {
            styles
                .track(|track| {
                    track
                        .width(px(scroll::TRACK))
                        .bg(gpui_kit::transparent_black())
                })
                .thumb(|thumb| thumb_shape(thumb, rest))
                .thumb_hover(|thumb| thumb_shape(thumb, hot))
                .thumb_active(|thumb| thumb_shape(thumb, hot))
        });
    if let Some(content) = content {
        element = element.scroll_size(content);
    }
    div().absolute().inset_0().child(element).into_any_element()
}

/// The shape of one thumb, in the colour of its state.
fn thumb_shape(thumb: ScrollbarThumbStyle, colour: Hsla) -> ScrollbarThumbStyle {
    thumb
        .width(px(scroll::THUMB))
        // Centred in the track: the gap either side of the thumb.
        .inset(px((scroll::TRACK - scroll::THUMB) / 2.0))
        .radius(px(scroll::RADIUS))
        .min_length(px(scroll::MIN_THUMB))
        .bg(colour)
}

/// The measurements every scrollbar shares, and the geometry of one thumb.
///
/// The library draws the bars of the scrolling views; this is the same shape,
/// worked out by hand for the graph, whose pan is not a scroll area and whose
/// content grows and shrinks with the zoom.
pub(crate) mod scroll {
    /// The width of the track a thumb runs in.
    pub(crate) const TRACK: f32 = 8.0;
    /// The width of the thumb itself, centred in the track.
    pub(crate) const THUMB: f32 = 6.0;
    /// The radius of the thumb's ends.
    pub(crate) const RADIUS: f32 = 3.0;
    /// The shortest a thumb is ever drawn, so that it stays grabbable.
    pub(crate) const MIN_THUMB: f32 = 24.0;
    /// How opaque an idle thumb is.
    pub(crate) const THUMB_ALPHA: f32 = 0.4;
    /// How opaque a thumb is under the pointer and while it is dragged.
    pub(crate) const THUMB_HOT_ALPHA: f32 = 0.7;

    /// Where the thumb of one axis sits and how long it is.
    ///
    /// Everything is measured in pixels along the track: `extent` is how much
    /// content there is beyond the viewport, `length` how much of the track
    /// the thumb takes, and `offset` how far the viewport has moved into the
    /// content.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub(crate) struct Bar {
        /// The length of the thumb along the track.
        length: f32,
        /// How far the thumb may travel along the track.
        travel: f32,
        /// How much content there is beyond the viewport.
        extent: f32,
        /// How far the viewport has moved into the content.
        offset: f32,
    }

    impl Bar {
        /// The bar of an axis with `content` pixels behind a `viewport` of
        /// `track` pixels, showing the part that starts at `offset`.
        ///
        /// `None` when the content fits: there is nothing to show and nothing
        /// to grab.
        pub(crate) fn new(content: f32, viewport: f32, offset: f32, track: f32) -> Option<Bar> {
            // A bar of a viewport that is not a number of pixels, or of a
            // track of no length, cannot be drawn.
            let measured = content.is_finite() && viewport.is_finite() && track.is_finite();
            if !measured || content <= viewport || track <= 0.0 || viewport <= 0.0 {
                return None;
            }
            let length = (viewport / content * track).max(MIN_THUMB.min(track));
            let extent = content - viewport;
            Some(Bar {
                length,
                travel: track - length,
                extent,
                offset: offset.clamp(0.0, extent),
            })
        }

        /// The length of the thumb along the track.
        pub(crate) fn length(self) -> f32 {
            self.length
        }

        /// Where the thumb starts, from the start of the track.
        pub(crate) fn start(self) -> f32 {
            if self.travel <= 0.0 {
                0.0
            } else {
                self.offset / self.extent * self.travel
            }
        }

        /// The offset a press asks for: `position` along the track, holding the
        /// point `grab` inside the thumb.
        ///
        /// A press on the thumb keeps the offset it already shows; a press on
        /// the track puts the middle of the thumb under the pointer.
        pub(crate) fn offset_at(self, position: f32, grab: f32) -> f32 {
            if self.travel <= 0.0 {
                return 0.0;
            }
            ((position - grab) / self.travel).clamp(0.0, 1.0) * self.extent
        }
    }
}

/// A button that is only an icon, with a tooltip.
pub(crate) fn icon_button(
    id: impl Into<gpui_kit::ElementId>,
    icon: IconName,
    tooltip: impl Into<SharedString>,
) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .compact()
        .icon(icon)
        .tooltip(tooltip)
}

/// The size of one square action in the title bar.
///
/// The pointer finds a target this size without aiming, and the bar's chips
/// are the same height, so the row has one rhythm.
pub(crate) const TOOLBAR_TARGET: Pixels = px(28.);

/// A square action of the title bar: a hit target, an icon and a tooltip.
///
/// The icon is drawn in a quiet colour until the pointer is on the target,
/// which then fills; a disabled action keeps its place and says what it would
/// do, so the row never reflows when a comparison starts or finishes.
pub(crate) fn toolbar_button(
    id: impl Into<ElementId>,
    content: impl IntoElement,
    tip: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    let tip: SharedString = tip.into();
    let button = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(TOOLBAR_TARGET)
        .rounded(theme::RADIUS_MD)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(content);
    if enabled {
        button
            .cursor_pointer()
            .hover(|this| this.bg(theme::BG_RAISED))
            .active(|this| this.bg(theme::BG_ACTIVE))
    } else {
        button.cursor_default()
    }
}

/// The icon of a title bar action: readable at rest, dimmed when the action
/// cannot be taken.
pub(crate) fn action_icon(icon: IconName, enabled: bool) -> AnyElement {
    Icon::new(icon)
        .size(px(15.))
        .text_color(if enabled { theme::FG_2 } else { theme::FG_4 })
        .into_any_element()
}

/// The indicator of an action that is running: a small turning mark.
pub(crate) fn spinner() -> AnyElement {
    Spinner::new()
        .with_size(px(14.))
        .color(theme::CHANGED.fg.into())
        .into_any_element()
}

/// The thin rule that separates the title bar's actions from the window
/// controls.
pub(crate) fn toolbar_divider() -> AnyElement {
    div()
        .flex_none()
        .w(px(1.))
        .h(px(16.))
        .ml(px(7.))
        .mr(px(1.))
        .bg(theme::LINE_STRONG)
        .into_any_element()
}

/// The colour of an impact class.
pub(crate) fn class_accent(class: blastr_model::ImpactClass) -> Accent {
    match class {
        blastr_model::ImpactClass::Adaptation => theme::EDIT,
        blastr_model::ImpactClass::Behaviour => theme::BEHAVE,
    }
}

/// The glyph square a designed empty state shows above its title.
pub(crate) fn glyph(icon: IconName, accent: Option<Accent>) -> AnyElement {
    let accent = accent.unwrap_or(theme::NEUTRAL);
    div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(32.))
        .h(px(32.))
        .rounded(px(10.))
        .border_1()
        .border_color(accent.line)
        .bg(accent.bg)
        .child(Icon::new(icon).size(px(14.)).text_color(accent.fg))
        .into_any_element()
}

/// How many characters of a failure message a card's summary keeps.
const FAILURE_SUMMARY_CHARS: usize = 140;

/// The depth a comparison uses unless the settings ask for another.
pub(crate) fn default_depth() -> u32 {
    blastr_model::AnalysisParams::default().depth
}

/// One short sentence saying why a task failed, for a failure card.
///
/// Runs of whitespace are collapsed to single spaces, so a message that
/// arrives over several lines still reads as one line. The first sentence is
/// kept; when it is longer than the card can hold it is cut at the last word
/// that fits and an ellipsis marks the cut.
pub(crate) fn failure_summary(message: &str) -> String {
    let flat = message.split_whitespace().collect::<Vec<_>>().join(" ");
    shorten(&first_sentence(&flat), FAILURE_SUMMARY_CHARS)
}

/// `text` up to and including its first full stop, else all of it.
fn first_sentence(text: &str) -> String {
    let mut end = text.len();
    for (index, character) in text.char_indices() {
        if character == '.' {
            let rest = &text[index + 1..];
            if rest.chars().next().is_none_or(char::is_whitespace) {
                end = index + 1;
                break;
            }
        }
    }
    text[..end].trim_end().to_owned()
}

/// `text` cut to at most `limit` characters at a word boundary, with an
/// ellipsis when something was dropped.
fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let cut: String = text.chars().take(limit).collect();
    let head = match cut.rfind(|character: char| character.is_whitespace()) {
        Some(index) if index > 0 => &cut[..index],
        // One word longer than the card: there is no boundary to cut at.
        _ => cut.as_str(),
    };
    format!("{}…", head.trim_end_matches(['.', ',', ';', ':']))
}

/// The one-line form of a failure that a side panel shows; the whole card
/// belongs to the centre pane, where there is room for it.
pub(crate) fn failure_line() -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .items_center()
        .child(
            div().flex_none().child(
                Icon::new(IconName::TriangleAlert)
                    .size(px(12.))
                    .text_color(theme::EDIT.fg),
            ),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_2)
                .child("Comparison failed — see the centre pane"),
        )
        .into_any_element()
}

/// A designed empty state: a glyph, a title and one sentence.
pub(crate) fn empty_state(
    icon: IconName,
    accent: Option<Accent>,
    title: &str,
    body: &str,
) -> AnyElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .bg(theme::BG_CANVAS)
        .child(glyph(icon, accent))
        .child(
            div()
                .text_size(px(15.))
                .text_color(theme::FG)
                .child(title.to_owned()),
        )
        .child(
            div()
                .max_w(px(430.))
                .text_center()
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_3)
                .child(body.to_owned()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_is_the_brand_file_with_its_gradients() {
        let svg = String::from_utf8_lossy(MARK.bytes());
        assert!(svg.contains("<svg"), "the embedded file is not an SVG");
        // The gradients are the reason the mark is loaded as an image: the
        // `svg()` element would draw a single colour instead.
        assert!(svg.contains("linearGradient"));
        assert_eq!(MARK.format(), ImageFormat::Svg);
    }

    #[test]
    fn a_short_message_is_its_own_summary() {
        assert_eq!(
            failure_summary("The comparison failed."),
            "The comparison failed."
        );
        assert_eq!(failure_summary("  no full stop here "), "no full stop here");
    }

    #[test]
    fn only_the_first_sentence_is_kept() {
        assert_eq!(
            failure_summary("cargo could not be run. Then there is more detail."),
            "cargo could not be run."
        );
        // A full stop inside a word is not the end of a sentence.
        assert_eq!(
            failure_summary("cargo.exe could not be run"),
            "cargo.exe could not be run"
        );
    }

    #[test]
    fn a_long_message_is_cut_at_a_word_boundary() {
        let words: Vec<String> = (0..60).map(|index| format!("crate{index}")).collect();
        let message = words.join(" ");
        let summary = failure_summary(&message);
        assert!(summary.ends_with('…'), "summary was {summary}");
        assert!(summary.chars().count() <= FAILURE_SUMMARY_CHARS + 1);
        let kept = summary.trim_end_matches('…');
        assert!(message.starts_with(kept), "cut mid-word: {summary}");
        assert!(kept.ends_with(|character: char| character.is_ascii_digit()));
        assert!(!kept.ends_with(' '));
    }

    #[test]
    fn whitespace_runs_become_single_spaces() {
        assert_eq!(failure_summary("one\n\n two\tthree"), "one two three");
    }

    #[test]
    fn one_unbroken_word_is_cut_as_far_as_it_fits() {
        let message = "x".repeat(200);
        let summary = failure_summary(&message);
        assert_eq!(summary.chars().count(), FAILURE_SUMMARY_CHARS + 1);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn every_marker_is_drawn_in_one_whole_pixel_box() {
        for shape in [
            Marker::Changed,
            Marker::Diamond,
            Marker::Dot,
            Marker::Candidate,
        ] {
            let drawn = shape.draw();
            assert_eq!(drawn.size.fract(), 0.0, "{shape:?} is a part pixel");
            assert!(
                drawn.size >= 6.0 && drawn.size <= 8.0,
                "{shape:?} is the wrong size"
            );
            // A shape never spills out of the gutter it is centred in.
            assert!(drawn.size <= f32::from(GUTTER));
            assert!(drawn.size <= f32::from(ROW_LINE));
            // A circle's radius is half its size; a square's is smaller.
            assert!(drawn.radius * 2.0 <= drawn.size + f32::EPSILON);
            assert!(drawn.border > 0.0 && drawn.border <= 1.5);
        }
    }

    #[test]
    fn the_gutter_is_one_line_high_and_holds_the_marker() {
        // The marker's box and the row's first line are the same height, so
        // the two centres are the same line.
        assert_eq!(GUTTER, px(14.));
        assert_eq!(ROW_LINE, px(18.));
        assert_eq!(PILL, ROW_LINE);
        // A badge and a marker column are both centred on the first line.
        assert_eq!(SECOND_LEAD, GUTTER + GUTTER_GAP);
        // The gutter is wide enough for the widest marker with air around it.
        assert!(GUTTER > px(8.));
    }

    #[test]
    fn the_rows_of_the_outline_hold_the_lines_they_draw() {
        use crate::state::outline::height;
        // A change row is one line with the same air above and below it.
        let air = (height::CHANGE - f32::from(ROW_LINE)) / 2.0;
        assert_eq!(air, air.round());
        assert_eq!(air, 5.0);
        // An impact row holds the name line, the gap and the reason line, with
        // an even 2 px above and below them.
        let impact = f32::from(ROW_LINE) + 2.0 + f32::from(SUB_LINE) + 4.0;
        assert_eq!(impact, height::ITEM);
        // A file row is one line in a row that is not tall enough to clip it.
        assert!(f32::from(ROW_LINE) <= height::FILE);
        assert!(f32::from(PILL) <= height::CHANGE);
    }

    #[test]
    fn a_thumb_is_as_long_as_the_share_of_the_content_it_shows() {
        // Half of the content is on screen: half of the track is thumb.
        let bar = scroll::Bar::new(1000.0, 500.0, 0.0, 100.0).expect("something to scroll");
        assert!((bar.length() - 50.0).abs() < 0.001);
        // A tenth of the content is on screen: a tenth of the track.
        let bar = scroll::Bar::new(1000.0, 300.0, 0.0, 100.0).expect("something to scroll");
        assert!((bar.length() - 30.0).abs() < 0.001);
    }

    #[test]
    fn a_thumb_never_becomes_shorter_than_the_grab() {
        // One line of a very long list: the share is a hundredth of the track,
        // but the thumb is still the minimum, so the pointer can find it.
        let bar = scroll::Bar::new(100_000.0, 100.0, 0.0, 100.0).expect("something to scroll");
        assert_eq!(bar.length(), scroll::MIN_THUMB);
    }

    #[test]
    fn a_bar_with_nothing_to_scroll_is_not_drawn() {
        assert!(scroll::Bar::new(400.0, 400.0, 0.0, 100.0).is_none());
        assert!(scroll::Bar::new(300.0, 400.0, 0.0, 100.0).is_none());
        // Nothing can be shown through a viewport or a track of no size.
        assert!(scroll::Bar::new(400.0, 0.0, 0.0, 100.0).is_none());
        assert!(scroll::Bar::new(400.0, 100.0, 0.0, 0.0).is_none());
    }

    #[test]
    fn the_thumb_stands_where_the_viewport_has_moved_to() {
        let track = 100.0;
        let bar = scroll::Bar::new(1000.0, 200.0, 0.0, track).expect("something to scroll");
        assert_eq!(bar.start(), 0.0);
        // The far end of the content puts the thumb against the far end of the
        // track, and nowhere past it.
        let bar = scroll::Bar::new(1000.0, 200.0, 800.0, track).expect("something to scroll");
        assert!((bar.start() - (track - bar.length())).abs() < 0.001);
        let bar = scroll::Bar::new(1000.0, 200.0, 900.0, track).expect("something to scroll");
        assert!((bar.start() - (track - bar.length())).abs() < 0.001);
        // Halfway through the content, halfway along the travel.
        let bar = scroll::Bar::new(1000.0, 200.0, 400.0, track).expect("something to scroll");
        let travel = track - bar.length();
        assert!((bar.start() - travel / 2.0).abs() < 0.001);
    }

    #[test]
    fn dragging_a_thumb_asks_for_the_offset_it_stands_for() {
        let track = 100.0;
        let bar = scroll::Bar::new(1000.0, 200.0, 0.0, track).expect("something to scroll");
        // A press on the thumb a third of the way into it keeps the offset it
        // already shows: dragging starts where the bar is, not at zero.
        let grab = bar.start() + bar.length() / 3.0;
        assert!(bar.offset_at(grab, grab) <= f32::EPSILON);
        // The thumb moved a quarter of its travel asks for a quarter of the
        // content beyond the viewport.
        let moved = bar.length() / 3.0 + (track - bar.length()) / 4.0;
        let asked = bar.offset_at(moved, bar.length() / 3.0);
        assert!((asked - (1000.0 - 200.0) / 4.0).abs() < 0.001);
        // A press past either end asks for the ends of the content.
        assert_eq!(bar.offset_at(-50.0, 10.0), 0.0);
        assert!((bar.offset_at(500.0, 10.0) - 800.0).abs() < 0.001);
    }

    #[test]
    fn a_press_on_the_track_puts_the_middle_of_the_thumb_under_the_pointer() {
        let track = 120.0;
        let bar = scroll::Bar::new(600.0, 120.0, 0.0, track).expect("something to scroll");
        let middle = track / 2.0;
        let asked = bar.offset_at(middle, bar.length() / 2.0);
        // Halfway along the travel is halfway through what is left over.
        assert!((asked - (600.0 - 120.0) / 2.0).abs() < 0.001);
    }

    #[test]
    fn the_bar_is_thin_and_its_thumb_sits_inside_the_track() {
        assert_eq!(scroll::TRACK, 8.0);
        assert_eq!(scroll::THUMB, 6.0);
        assert_eq!(scroll::RADIUS, 3.0);
        assert_eq!(scroll::MIN_THUMB, 24.0);
        // The thumb is narrower than its track and rounded on both ends.
        let (track, thumb, radius) = (scroll::TRACK, scroll::THUMB, scroll::RADIUS);
        assert!(thumb < track);
        assert_eq!(radius, thumb / 2.0);
        // It is easy to find and easy to hit, and it says so on hover.
        let (min, rest, hot) = (
            scroll::MIN_THUMB,
            scroll::THUMB_ALPHA,
            scroll::THUMB_HOT_ALPHA,
        );
        assert!(min >= 24.0);
        assert!(hot > rest);
        assert!(rest > 0.0 && hot < 1.0);
    }
}
