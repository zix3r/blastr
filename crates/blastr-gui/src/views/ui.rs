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
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Div, ElementId, Image, ImageFormat, IntoElement, ParentElement, Pixels,
    SharedString, Stateful, Styled, div, img, px,
};

use crate::theme::{self, Accent, RADIUS_SM, TEXT_MICRO, TEXT_SMALL, Tone};

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

/// The thin vertical bar a scrolling list draws over its right edge.
///
/// The caller puts it in a `relative` box as large as the scroll area.
pub(crate) fn scrollbar<H: ScrollbarHandle + Clone>(id: &str, handle: &H) -> AnyElement {
    let thumb = |thumb: ScrollbarThumbStyle, alpha: f32| {
        thumb
            .width(px(6.))
            .inset(px(1.))
            .radius(px(3.))
            .min_length(px(24.))
            .bg(theme::FG_4.hsla().alpha(alpha))
    };
    let bar = Scrollbar::new(handle)
        .id(ElementId::Name(format!("scrollbar-{id}").into()))
        .axis(ScrollbarAxis::Vertical)
        .mode(ScrollbarMode::Always)
        .viewport_from_layout()
        .styles(move |styles| {
            styles
                .track(|track| track.width(px(8.)).bg(gpui_kit::transparent_black()))
                .thumb(move |style| thumb(style, 0.4))
                .thumb_hover(move |style| thumb(style, 0.7))
                .thumb_active(move |style| thumb(style, 0.7))
        });
    div().absolute().inset_0().child(bar).into_any_element()
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
