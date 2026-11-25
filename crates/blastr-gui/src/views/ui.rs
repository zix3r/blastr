//! Small pieces every view shares: markers, badges, headings and buttons.
//!
//! Meaning is always carried twice: by the colour of these pieces and by the
//! words they show.

use gpui_kit::AnimationExt as _;
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
/// do, so the row never reflows when a comparison starts or finishes. The
/// target is taken out of the bar's drag area (see [`no_drag`]).
pub(crate) fn toolbar_button(
    id: impl Into<ElementId>,
    content: impl IntoElement,
    tip: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    let tip: SharedString = tip.into();
    let button = no_drag(div().id(id))
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

/// Takes a title bar control out of the window's drag area.
///
/// The title bar marks its whole row as the platform's caption, and on
/// Windows the hit test answers `HTCAPTION` wherever the hitboxes under the
/// pointer include that row. A press on a control there starts the system's
/// window-move loop, which swallows the release, so the click never lands.
/// An occluding hitbox ends the hit test at the control: the press is the
/// control's alone, and the empty bar around it still drags and maximises.
pub(crate) fn no_drag<E: InteractiveElement>(element: E) -> E {
    element.occlude()
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

// -- loading states ----------------------------------------------------------

/// A bar that says work is going on without saying how much is left: a
/// segment that runs along the track.
pub(crate) fn indeterminate_bar(id: impl Into<ElementId>, height: Pixels) -> AnyElement {
    div()
        .relative()
        .w_full()
        .h(height)
        .overflow_hidden()
        .rounded(height / 2.)
        .bg(theme::BG_RAISED_2)
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .w(gpui_kit::relative(0.3))
                .rounded(height / 2.)
                .bg(theme::CHANGED.solid)
                .with_animation(
                    id,
                    gpui_kit::Animation::new(std::time::Duration::from_millis(1400))
                        .repeat()
                        .with_easing(gpui_kit::ease_in_out),
                    |segment, delta| segment.left(gpui_kit::relative(-0.3 + 1.3 * delta)),
                ),
        )
        .into_any_element()
}

/// A bar filled to `fraction`, between 0 and 1.
pub(crate) fn determinate_bar(fraction: f32, height: Pixels) -> AnyElement {
    div()
        .w_full()
        .h(height)
        .overflow_hidden()
        .rounded(height / 2.)
        .bg(theme::BG_RAISED_2)
        .child(
            div()
                .h_full()
                .w(gpui_kit::relative(fraction.clamp(0., 1.)))
                .rounded(height / 2.)
                .bg(theme::CHANGED.solid),
        )
        .into_any_element()
}

/// A placeholder block of a skeleton: where text or a badge will be.
pub(crate) fn bone(width: impl Into<gpui_kit::Length> + Clone, height: Pixels) -> Div {
    div()
        .flex_none()
        .w(width)
        .h(height)
        .rounded(RADIUS_SM)
        .bg(theme::BG_RAISED_2)
}

/// `content` with a faint highlight sweeping across it, left to right.
///
/// Every sweep takes the same time and starts on the frame its placeholder
/// first shows, so the rows of a list sweep together, as one band. Only a
/// view that is loading draws one: the animation redraws the window on every
/// frame while it runs.
pub(crate) fn shimmer(id: impl Into<ElementId>, content: impl IntoElement) -> AnyElement {
    let glow = theme::FG.hsla().alpha(0.06);
    let clear = theme::FG.hsla().alpha(0.);
    let half = |from, to| {
        div().h_full().flex_1().bg(gpui_kit::linear_gradient(
            90.,
            gpui_kit::linear_color_stop(from, 0.),
            gpui_kit::linear_color_stop(to, 1.),
        ))
    };
    let band = h_flex()
        .absolute()
        .top_0()
        .bottom_0()
        .w(gpui_kit::relative(0.4))
        .child(half(clear, glow))
        .child(half(glow, clear))
        .with_animation(
            id,
            gpui_kit::Animation::new(std::time::Duration::from_millis(1600))
                .repeat()
                .with_easing(gpui_kit::ease_in_out),
            |band, delta| band.left(gpui_kit::relative(-0.4 + 1.4 * delta)),
        );
    div()
        .relative()
        .size_full()
        .overflow_hidden()
        .child(content)
        .child(band)
        .into_any_element()
}

/// The evidence panel while the result is on its way: a name, its place, two
/// badges, a sentence and the steps of a path.
pub(crate) fn evidence_skeleton() -> AnyElement {
    let step = |index: usize, width: f32| {
        h_flex()
            .id(("evidence-skeleton-step", index))
            .gap(GUTTER_GAP)
            .items_center()
            .child(gutter(bone(px(8.), px(8.)).rounded(px(4.))))
            .child(bone(px(width), px(10.)))
            .child(div().flex_1())
            .child(bone(px(54.), px(8.)))
    };
    shimmer(
        "evidence-skeleton",
        v_flex()
            .p_3()
            .gap_3()
            .child(bone(px(180.), px(14.)))
            .child(bone(gpui_kit::relative(0.8), px(8.)))
            .child(
                h_flex()
                    .gap_1()
                    .child(bone(px(96.), PILL))
                    .child(bone(px(116.), PILL)),
            )
            .child(
                v_flex()
                    .gap(px(7.))
                    .child(bone(gpui_kit::relative(0.95), px(9.)))
                    .child(bone(gpui_kit::relative(0.7), px(9.))),
            )
            .child(bone(px(90.), px(8.)))
            .child(step(0, 120.))
            .child(step(1, 150.))
            .child(step(2, 100.)),
    )
}

/// The code pane while the result is on its way: a header and lines of
/// source of different lengths, indented as code is.
pub(crate) fn code_skeleton() -> AnyElement {
    // Indent and length of each line, in characters' worth of 8 px.
    const LINES: [(u8, u8); 14] = [
        (0, 30),
        (1, 44),
        (2, 26),
        (2, 52),
        (1, 8),
        (0, 0),
        (1, 38),
        (2, 60),
        (3, 34),
        (2, 12),
        (1, 6),
        (0, 0),
        (0, 24),
        (1, 40),
    ];
    let lines = LINES.iter().enumerate().map(|(index, (indent, length))| {
        h_flex()
            .h(px(19.))
            .gap_3()
            .items_center()
            .child(
                div()
                    .w(px(28.))
                    .flex()
                    .justify_end()
                    .child(bone(px(if index >= 9 { 14. } else { 8. }), px(8.))),
            )
            .child(div().w(px(f32::from(*indent) * 24.)))
            .when(*length > 0, |line| {
                line.child(bone(px(f32::from(*length) * 8.), px(9.)))
            })
    });
    v_flex()
        .size_full()
        .bg(theme::BG_CANVAS)
        .child(
            h_flex()
                .h(px(32.))
                .flex_none()
                .px_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(theme::LINE)
                .bg(theme::BG_PANEL)
                .child(bone(px(12.), px(12.)))
                .child(bone(px(220.), px(9.)))
                .child(bone(px(120.), px(9.))),
        )
        .child(shimmer(
            "code-skeleton",
            v_flex().size_full().py_2().children(lines),
        ))
        .into_any_element()
}
