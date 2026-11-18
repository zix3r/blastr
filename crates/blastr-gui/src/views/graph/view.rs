//! The impact graph: custom painting, pan and zoom, and the interaction that
//! ties the graph to the one selection every other view shares.
//!
//! The picture is painted in four passes on one canvas — lane guides, then
//! references, then the node boxes, then the labels as elements over the
//! canvas — so an edge never crosses a name. A slim bar under the canvas holds
//! the legend and the zoom controls, and never covers a node.

use gpui_kit::prelude::*;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, BorderStyle, Bounds, Context, IntoElement, ParentElement, PathBuilder, Point,
    Styled, Window, div, point, px, quad, size,
};

use blastr_model::{Analysis, SymbolPath};

use super::layout::{self, NODE_H, NODE_W, PILL_H, PILL_W, Placed};
use super::model::{self, Model, NodeClass, PER_LAYER_STEP};
use crate::state::outline::Counts;
use crate::theme::{self, TEXT_SMALL, Tone};
use crate::views::shell::Shell;
use crate::views::ui;

/// How much one wheel step changes the zoom.
const ZOOM_STEP: f32 = 1.1;
/// The smallest zoom the wheel reaches.
const ZOOM_MIN: f32 = 0.4;
/// The largest zoom the wheel reaches.
const ZOOM_MAX: f32 = 2.5;
/// The height of the bar under the canvas.
const BAR_H: f32 = 28.0;
/// The width of the hover card.
const CARD_W: f32 = 320.0;
/// The colour of a reference that is not on the selected path: quieter than
/// a plain resolved edge, so the path stands out.
const EDGE_DIM: Tone = Tone::rgba(0x343b46, 190);
/// How far, in graph pixels, one edge leaves the middle of its source box.
/// The edges out of a node are spread over this half-range so a bundle into a
/// full layer reads as separate lines.
const EDGE_SPREAD: f32 = 12.0;

/// What the pointer found in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// A drawn symbol.
    Node(usize),
    /// The pill that hides the rest of a layer.
    Pill(usize),
}

/// The state of the graph view.
#[derive(Debug)]
pub(crate) struct GraphState {
    /// The symbols whose hidden neighbours are pulled in.
    pub(crate) expanded: Vec<SymbolPath>,
    /// How many symbols each layer adds beyond the default, by layer.
    limits: BTreeMap<usize, usize>,
    offset: Point<gpui_kit::Pixels>,
    zoom: f32,
    drag_from: Option<Point<gpui_kit::Pixels>>,
    hover: Option<usize>,
    model: Option<Model>,
    placed: Option<Placed>,
    key: Option<CacheKey>,
    /// The result and the graph the drawn picture came from.
    ///
    /// Kept across a rebuild, so that only a new result or a new graph refits
    /// the picture: an expansion or a raised layer limit keeps the view.
    source: Option<(usize, usize)>,
    /// The element an expansion is holding still while the picture is laid out
    /// again, and where it stood before.
    held: Option<Held>,
    /// The bar a press grabbed, while the pointer is still down.
    bar_drag: Option<BarGrab>,
    /// The bar the pointer is over.
    bar_hot: Option<ScrollbarAxis>,
    bounds: Rc<RefCell<Bounds<gpui_kit::Pixels>>>,
    /// Whether the picture still has to be fitted to the canvas.
    fit_pending: bool,
    /// Whether a fit already asked for the frame that reports the canvas size.
    fit_asked: bool,
    /// The selection the last paint saw, so a selection change can pan to a
    /// node that lies out of view without refitting the picture.
    shown_selection: Option<SymbolPath>,
}

/// What the drawn subgraph was built from; a change rebuilds it.
#[derive(Debug, Clone, PartialEq)]
struct CacheKey {
    /// The identity of the result.
    analysis: usize,
    /// The identity of the newer side's graph, or zero when there is none.
    graph: usize,
    /// The symbols the reader expanded.
    expanded: Vec<SymbolPath>,
    /// The extra symbols each layer shows.
    limits: BTreeMap<usize, usize>,
    /// The shared selection, which decides what the picture always keeps.
    selected: Option<SymbolPath>,
}

impl Default for GraphState {
    fn default() -> Self {
        GraphState {
            expanded: Vec::new(),
            limits: BTreeMap::new(),
            offset: point(px(20.), px(10.)),
            zoom: 1.0,
            drag_from: None,
            hover: None,
            model: None,
            placed: None,
            key: None,
            source: None,
            held: None,
            bar_drag: None,
            bar_hot: None,
            bounds: Rc::new(RefCell::new(Bounds::default())),
            fit_pending: true,
            fit_asked: false,
            shown_selection: None,
        }
    }
}

impl GraphState {
    /// Forgets the drawn subgraph; the next render builds it and fits it.
    pub(crate) fn invalidate(&mut self) {
        self.forget();
        // A new result has no old place to keep: it is framed from scratch.
        self.source = None;
        self.held = None;
        self.fit_pending = true;
        self.fit_asked = false;
    }

    /// Forgets the drawn subgraph but keeps the view, so an expansion or a
    /// raised layer limit does not jump the picture.
    fn rebuild(&mut self) {
        self.forget();
    }

    /// Drops everything derived from the analysis.
    fn forget(&mut self) {
        self.model = None;
        self.placed = None;
        self.key = None;
        self.hover = None;
    }

    /// Fits the picture to the canvas it was last painted into.
    fn fit(&mut self) {
        self.fit_pending = true;
        self.fit_asked = false;
    }

    /// The canvas size the last paint reported.
    fn pane(&self) -> (f32, f32) {
        let bounds = *self.bounds.borrow();
        (f32::from(bounds.size.width), f32::from(bounds.size.height))
    }

    /// Sets the zoom, keeping the middle of the pane where it is.
    fn zoom_about_pane(&mut self, zoom: f32) {
        let pane = self.pane();
        let cursor = (pane.0 / 2.0, pane.1 / 2.0);
        let offset = (f32::from(self.offset.x), f32::from(self.offset.y));
        let kept = zoom_around(cursor, offset, zoom / self.zoom);
        self.offset = point(px(kept.0), px(kept.1));
        self.zoom = zoom;
        self.fit_pending = false;
    }

    /// The node or pill under a window position.
    fn hit(&self, position: Point<gpui_kit::Pixels>) -> Option<Hit> {
        let placed = self.placed.as_ref()?;
        let bounds = *self.bounds.borrow();
        let local = position - bounds.origin;
        let inside = |left: Point<gpui_kit::Pixels>, width, height| {
            local.x >= left.x
                && local.x <= left.x + px(width) * self.zoom
                && local.y >= left.y
                && local.y <= left.y + px(height) * self.zoom
        };
        for (index, (x, y)) in placed.origins.iter().enumerate() {
            let left = point(
                px(*x) * self.zoom + self.offset.x,
                px(*y) * self.zoom + self.offset.y,
            );
            if inside(left, NODE_W, NODE_H) {
                return Some(Hit::Node(index));
            }
        }
        for (index, (x, y)) in placed.pills.iter().enumerate() {
            let left = point(
                px(*x) * self.zoom + self.offset.x,
                px(*y) * self.zoom + self.offset.y,
            );
            // The pill keeps a fixed height while its width follows the node.
            if local.x >= left.x
                && local.x <= left.x + px(PILL_W) * self.zoom
                && local.y >= left.y
                && local.y <= left.y + px(PILL_H)
            {
                let layer = self
                    .model
                    .as_ref()
                    .and_then(|model| model.collapsed.get(index))
                    .map_or(0, |collapsed| collapsed.layer);
                return Some(Hit::Pill(layer));
            }
        }
        None
    }

    /// The bars of the canvas over the pane the last paint reported.
    fn bars(&self) -> Bars {
        let Some(placed) = self.placed.as_ref() else {
            return Bars::default();
        };
        Bars::of(
            placed,
            self.pane(),
            (f32::from(self.offset.x), f32::from(self.offset.y)),
            self.zoom,
        )
    }

    /// The bar that a point on the canvas grabs, and how far into its thumb.
    fn grab_bar(&self, local: Point<gpui_kit::Pixels>) -> Option<BarGrab> {
        let bars = self.bars();
        let (width, height) = self.pane();
        let x = f32::from(local.x);
        let y = f32::from(local.y);
        let band = ui::scroll::TRACK;
        // The corner belongs to the vertical bar, and neither bar covers the
        // canvas when its axis has nothing to scroll.
        if let Some(bar) = bars.vertical
            && (width - band..=width).contains(&x)
            && (0.0..=height).contains(&y)
        {
            return Some(BarGrab {
                axis: ScrollbarAxis::Vertical,
                grab: bar_grab(bar, y),
            });
        }
        if let Some(bar) = bars.horizontal
            && (height - band..=height).contains(&y)
            && (0.0..=width - band).contains(&x)
        {
            return Some(BarGrab {
                axis: ScrollbarAxis::Horizontal,
                grab: bar_grab(bar, x),
            });
        }
        None
    }

    /// Takes a press on a bar: the pan jumps to where the press landed.
    fn press_bar(&mut self, local: Point<gpui_kit::Pixels>) {
        self.bar_drag = self.grab_bar(local);
        self.drag_bar(local);
    }

    /// Moves the pan to where the dragged bar has been taken. Returns whether
    /// the pan moved.
    fn drag_bar(&mut self, local: Point<gpui_kit::Pixels>) -> bool {
        let Some(grab) = self.bar_drag else {
            return false;
        };
        let bars = self.bars();
        let vertical = grab.axis.is_vertical();
        let Some(bar) = (if vertical {
            bars.vertical
        } else {
            bars.horizontal
        }) else {
            return false;
        };
        let along = if vertical {
            f32::from(local.y)
        } else {
            f32::from(local.x)
        };
        // The bar counts how far the pane has moved into the picture; the pan
        // counts the same in the other direction.
        let wanted = -bar.offset_at(along, grab.grab);
        let current = if vertical {
            f32::from(self.offset.y)
        } else {
            f32::from(self.offset.x)
        };
        if (wanted - current).abs() < 0.5 {
            return false;
        }
        if vertical {
            self.offset.y = px(wanted);
        } else {
            self.offset.x = px(wanted);
        }
        self.fit_pending = false;
        true
    }

    /// Remembers where an element the reader is about to grow stands, so that
    /// the next layout can keep it on the same pixel.
    fn hold(&mut self, anchor: Anchor) {
        self.held = self.position(&anchor).map(|at| Held { anchor, at });
    }

    /// Where an element stands in graph coordinates.
    fn position(&self, anchor: &Anchor) -> Option<(f32, f32)> {
        let placed = self.placed.as_ref()?;
        match anchor {
            Anchor::Pill(layer) => {
                let pill = placed.pill_layers.iter().position(|at| at == layer)?;
                placed.pills.get(pill).copied()
            }
            Anchor::Node(symbol) => {
                let model = self.model.as_ref()?;
                let node = model.nodes.iter().position(|node| &node.symbol == symbol)?;
                placed.origins.get(node).copied()
            }
        }
    }

    /// Puts the held element back on the pixel it was on before the picture
    /// was laid out again, by moving the pan by as much as the element moved.
    fn settle(&mut self) {
        let Some(held) = self.held.take() else {
            return;
        };
        let Some(now) = self.position(&held.anchor) else {
            // The element the reader grew is gone: the pan stays where it is.
            return;
        };
        let kept = keep_anchor(
            (f32::from(self.offset.x), f32::from(self.offset.y)),
            self.zoom,
            held.at,
            now,
        );
        self.offset = point(px(kept.0), px(kept.1));
    }
}

/// The zoom and offset that keep the point under the pointer in place.
fn zoom_around(cursor: (f32, f32), offset: (f32, f32), ratio: f32) -> (f32, f32) {
    (
        cursor.0 - (cursor.0 - offset.0) * ratio,
        cursor.1 - (cursor.1 - offset.1) * ratio,
    )
}

/// The offset that leaves a point of the picture on the pixel it was on.
///
/// `before` and `after` are the same element's place in the picture before and
/// after it was laid out again. The pan absorbs the difference, scaled by the
/// zoom, so an expansion adds nodes without moving the one that was clicked.
fn keep_anchor(offset: (f32, f32), zoom: f32, before: (f32, f32), after: (f32, f32)) -> (f32, f32) {
    (
        offset.0 - (after.0 - before.0) * zoom,
        offset.1 - (after.1 - before.1) * zoom,
    )
}

/// The element an expansion holds still while the picture is laid out again.
#[derive(Debug, Clone, PartialEq)]
enum Anchor {
    /// The pill that stands for one layer's hidden neighbours.
    Pill(usize),
    /// A node whose neighbours the reader pulled in.
    Node(SymbolPath),
}

/// An anchor and where it stood before the picture was laid out again.
#[derive(Debug, Clone)]
struct Held {
    /// What to find among the nodes and pills of the new picture.
    anchor: Anchor,
    /// Where it stood, in graph coordinates.
    at: (f32, f32),
}

/// The two bars that say where the pane sits in the picture.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Bars {
    /// The bar on the right edge, for the picture above and below the pane.
    vertical: Option<ui::scroll::Bar>,
    /// The bar on the bottom edge, for the picture either side of the pane.
    horizontal: Option<ui::scroll::Bar>,
}

impl Bars {
    /// The bars of a picture `placed`, seen through a pane of `pane` pixels at
    /// `zoom` and panned to `offset`. An axis whose picture fits has no bar.
    fn of(placed: &Placed, pane: (f32, f32), offset: (f32, f32), zoom: f32) -> Bars {
        Bars {
            vertical: ui::scroll::Bar::new(placed.height * zoom, pane.1, -offset.1, pane.1),
            horizontal: ui::scroll::Bar::new(placed.width * zoom, pane.0, -offset.0, pane.0),
        }
    }
}

/// The bar a press grabbed, and where inside its thumb the press landed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BarGrab {
    /// Which bar was pressed.
    axis: ScrollbarAxis,
    /// How far into the thumb, along the track, the press landed.
    grab: f32,
}

/// How far into a bar's thumb a press at `along` lands.
///
/// A press outside the thumb counts as one on its middle, so that a click on
/// the track puts the middle of the thumb under the pointer.
fn bar_grab(bar: ui::scroll::Bar, along: f32) -> f32 {
    let start = bar.start();
    if (start..=start + bar.length()).contains(&along) {
        along - start
    } else {
        bar.length() / 2.0
    }
}

/// The colours of a node: its class, or the test accent for a test symbol.
fn node_accent(class: NodeClass, is_test: bool) -> theme::Accent {
    if class == NodeClass::Changed {
        return theme::CHANGED;
    }
    if is_test {
        return theme::TEST;
    }
    match class {
        NodeClass::NeedsEditing => theme::EDIT,
        NodeClass::MayBehave => theme::BEHAVE,
        NodeClass::Expanded | NodeClass::Changed => theme::NEUTRAL,
    }
}

/// The picture, ready to be painted: everything the paint closure needs.
struct Canvas {
    zoom: f32,
    offset: (f32, f32),
    /// The top-left corner of every node box.
    origins: Vec<(f32, f32)>,
    edges: Vec<model::Edge>,
    /// The accent of every node.
    accents: Vec<theme::Accent>,
    /// The node the reader selected, when it is drawn.
    selected: Option<usize>,
    /// How many columns the picture has.
    depth: usize,
    /// Whether the hover card is on screen: the canvas then watches for the
    /// pointer leaving it.
    hover: bool,
    /// Where the two bars stand.
    bars: Bars,
    /// The bar under the pointer, or the one being dragged.
    bar_hot: Option<ScrollbarAxis>,
}

impl Shell {
    /// The graph pane.
    pub(crate) fn graph_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(analysis_arc) = self.session.analysis().cloned() else {
            if self.session.shown().failure().is_some() {
                return self.failure_card(cx);
            }
            return v_flex()
                .size_full()
                .bg(theme::BG_CANVAS)
                .child(ui::empty_state(
                    IconName::Network,
                    Some(theme::CHANGED),
                    "The graph arrives with the result",
                    "It shows the changes and what they reach.",
                ))
                .into_any_element();
        };
        if analysis_arc.items.is_empty() && analysis_arc.diff.changes.is_empty() {
            return v_flex()
                .size_full()
                .bg(theme::BG_CANVAS)
                .child(ui::empty_state(
                    IconName::Network,
                    None,
                    "Nothing to draw",
                    "No structural change was found in this comparison.",
                ))
                .into_any_element();
        }

        let graph = self
            .session
            .shown()
            .ready()
            .and_then(|outcome| outcome.graph.clone());
        let selected = self
            .session
            .selection()
            .map(|selection| selection.symbol.clone());
        let key = CacheKey {
            analysis: Arc::as_ptr(&analysis_arc) as usize,
            graph: graph
                .as_ref()
                .map_or(0, |graph| Arc::as_ptr(graph) as usize),
            expanded: self.graph.expanded.clone(),
            limits: self.graph.limits.clone(),
            selected: selected.clone(),
        };
        if self.graph.key.as_ref() != Some(&key) {
            // A new result or a new graph is refitted; an expansion, a raised
            // layer limit or a selection change keeps the current view. The
            // source outlives the forgotten picture, so a rebuild is not
            // mistaken for a new result.
            let source = (key.analysis, key.graph);
            let refit = self.graph.source != Some(source);
            let built = model::build(model::Request {
                analysis: analysis_arc.as_ref(),
                graph: graph.as_deref(),
                expanded: &self.graph.expanded,
                selected: selected.as_ref(),
                limits: &self.graph.limits,
            });
            self.graph.placed = Some(layout::layout(&built));
            self.graph.model = Some(built);
            self.graph.key = Some(key);
            self.graph.source = Some(source);
            if refit {
                self.graph.held = None;
                self.graph.fit_pending = true;
                self.graph.fit_asked = false;
            } else {
                // An expansion keeps the reader's place: the element that was
                // grown stays on the pixel it was on.
                self.graph.settle();
            }
        }
        let Some(placed) = self.graph.placed.clone() else {
            return div().into_any_element();
        };
        let selected_index = self.graph.model.as_ref().and_then(|model| {
            model
                .nodes
                .iter()
                .position(|node| selected.as_ref() == Some(&node.symbol))
        });

        // The canvas only reports its size once it has been painted, so the
        // first fit waits one frame for it.
        let pane = self.graph.pane();
        let mut fitted = false;
        if self.graph.fit_pending && pane.0 > 1.0 && pane.1 > 1.0 {
            let view = layout::fit(&placed, pane, selected_index);
            self.graph.zoom = view.zoom;
            self.graph.offset = point(px(view.offset.0), px(view.offset.1));
            self.graph.fit_pending = false;
            fitted = true;
        } else if self.graph.fit_pending && !self.graph.fit_asked {
            self.graph.fit_asked = true;
            cx.notify();
        }

        // A selection never refits: it only pans when the node it names lies
        // entirely outside the pane, and even then the zoom is left alone.
        let selection_changed = self.graph.shown_selection.as_ref() != selected.as_ref();
        self.graph.shown_selection = selected.clone();
        if selection_changed && !fitted && pane.0 > 1.0 && pane.1 > 1.0 {
            let held = layout::View {
                zoom: self.graph.zoom,
                offset: (
                    f32::from(self.graph.offset.x),
                    f32::from(self.graph.offset.y),
                ),
            };
            let moved = layout::after_selection(&placed, selected_index, pane, held);
            if moved != held {
                self.graph.offset = point(px(moved.offset.0), px(moved.offset.1));
            }
        }

        let Some(model) = self.graph.model.as_ref() else {
            return div().into_any_element();
        };
        let summary = self.graph_summary(analysis_arc.as_ref(), model);
        let zoom = self.graph.zoom;
        let offset = self.graph.offset;
        let mono = cx.theme().mono_font_family.clone();
        let bars = self.graph.bars();
        let bar_hot = self
            .graph
            .bar_drag
            .map(|grab| grab.axis)
            .or(self.graph.bar_hot);

        // Passes one to three: guides, references and node boxes on the canvas.
        let hover_index = self.graph.hover;
        let canvas = Canvas {
            zoom,
            offset: (f32::from(offset.x), f32::from(offset.y)),
            origins: placed.origins.clone(),
            edges: model.edges.clone(),
            accents: model
                .nodes
                .iter()
                .map(|node| node_accent(node.class, node.is_test))
                .collect(),
            selected: selected_index,
            depth: placed
                .layers
                .iter()
                .copied()
                .max()
                .map_or(0, |layer| layer + 1),
            hover: hover_index.is_some(),
            bars,
            bar_hot,
        };
        let bounds_cell = self.graph.bounds.clone();
        let weak = cx.weak_entity();
        let canvas_layer = gpui_kit::canvas(
            move |bounds, _, _| {
                *bounds_cell.borrow_mut() = bounds;
            },
            move |bounds, _, window, _cx| {
                paint(&canvas, bounds, window);
                // The canvas has no hover listener of its own, so the window
                // reports the moves that take the pointer off the graph.
                if !canvas.hover && canvas.bar_hot.is_none() {
                    return;
                }
                let weak = weak.clone();
                window.on_mouse_event(
                    move |event: &gpui_kit::MouseMoveEvent, _, _, cx: &mut gpui_kit::App| {
                        if bounds.contains(&event.position) {
                            return;
                        }
                        let _ = weak.update(cx, |this, cx| {
                            let mut changed = this.graph.hover.take().is_some();
                            if this.graph.bar_hot.take().is_some() {
                                changed = true;
                            }
                            if changed {
                                cx.notify();
                            }
                        });
                    },
                );
            },
        );

        // Pass four: the labels, as elements over the canvas.
        let labels: Vec<AnyElement> = model
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let (x, y) = placed.origins.get(index).copied().unwrap_or((0.0, 0.0));
                let accent = node_accent(node.class, node.is_test);
                // The chip keeps a legible size whatever the zoom is.
                let chip_zoom = zoom.max(0.85);
                let chip = node.chip.as_ref().map(|chip| {
                    div()
                        .flex_none()
                        .h(px(13.) * chip_zoom)
                        .px(px(4.) * chip_zoom)
                        .rounded(px(3.) * chip_zoom)
                        .bg(accent.bg)
                        .text_size(px(8.5) * chip_zoom)
                        .text_color(accent.fg)
                        .flex()
                        .items_center()
                        .whitespace_nowrap()
                        .child(chip.clone())
                });
                div()
                    .absolute()
                    .left(px(x) * zoom + offset.x)
                    .top(px(y) * zoom + offset.y)
                    .w(px(NODE_W) * zoom)
                    .h(px(NODE_H) * zoom)
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(2.) * zoom)
                    .pl(px(12.) * zoom)
                    .pr(px(8.) * zoom)
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .truncate()
                                    .font_family(mono.clone())
                                    .text_size(px(12.) * zoom)
                                    .text_color(theme::FG)
                                    .child(node.name.clone()),
                            )
                            .when_some(chip, |this, chip| this.child(chip)),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .truncate()
                                    .text_size(px(10.5) * zoom)
                                    .text_color(theme::FG_4)
                                    .child(node.subtitle()),
                            )
                            .when(node.hidden > 0, |this| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .text_size(px(9.5) * zoom)
                                        .text_color(theme::FG_4)
                                        .whitespace_nowrap()
                                        .child(format!("+{}", node.hidden)),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        // The pills of the collapsed layers, after the nodes they follow.
        let pills: Vec<AnyElement> = model
            .collapsed
            .iter()
            .enumerate()
            .map(|(index, collapsed)| {
                let (x, y) = placed.pills.get(index).copied().unwrap_or((0.0, 0.0));
                div()
                    .absolute()
                    .left(px(x) * zoom + offset.x)
                    .top(px(y) * zoom + offset.y)
                    .w(px(PILL_W) * zoom)
                    .h(px(PILL_H))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(PILL_H / 2.0))
                    .border_1()
                    .border_color(theme::LINE)
                    .bg(theme::BG_RAISED)
                    .text_size(px(11.))
                    .text_color(theme::FG_3)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(format!("+{} more", collapsed.hidden))
                    .into_any_element()
            })
            .collect();

        let hover = hover_index.and_then(|index| {
            let node = model.nodes.get(index)?;
            let (x, y) = placed.origins.get(index).copied().unwrap_or((0.0, 0.0));
            Some(hover_card(node, x, y, zoom, offset, pane, &mono))
        });

        let viewport = div()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .bg(theme::BG_CANVAS)
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, cx| {
                    let bounds = *this.graph.bounds.borrow();
                    let local = event.position - bounds.origin;
                    // A bar takes the press before the picture does: the bars
                    // sit over the edges of the canvas.
                    if this.graph.grab_bar(local).is_some() {
                        this.graph.drag_from = None;
                        this.graph.hover = None;
                        this.graph.press_bar(local);
                        cx.notify();
                        return;
                    }
                    this.graph.bar_drag = None;
                    match this.graph.hit(event.position) {
                        Some(Hit::Node(index)) => {
                            let Some(node) = this
                                .graph
                                .model
                                .as_ref()
                                .and_then(|model| model.nodes.get(index))
                            else {
                                return;
                            };
                            let symbol = node.symbol.clone();
                            if event.click_count >= 2 {
                                // The node that is grown is held where it is,
                                // so the new neighbours appear around it and
                                // the pan and the zoom stay exactly as they
                                // were.
                                this.graph.hold(Anchor::Node(symbol.clone()));
                                if let Some(position) = this
                                    .graph
                                    .expanded
                                    .iter()
                                    .position(|expanded| expanded == &symbol)
                                {
                                    this.graph.expanded.remove(position);
                                } else {
                                    this.graph.expanded.push(symbol);
                                }
                                this.graph.rebuild();
                            } else {
                                this.select_symbol(symbol, None, cx);
                            }
                        }
                        Some(Hit::Pill(layer)) => {
                            // The pill is a button: one click shows more of the
                            // layer. A double-click counts as two clicks and
                            // simply adds twice. The pill itself is held in
                            // place, so the layer grows under the pointer.
                            this.graph.hold(Anchor::Pill(layer));
                            let limit = this.graph.limits.entry(layer).or_insert(0);
                            *limit = limit.saturating_add(PER_LAYER_STEP);
                            this.graph.rebuild();
                        }
                        None => this.graph.drag_from = Some(event.position),
                    }
                    this.graph.hover = None;
                    cx.notify();
                }),
            )
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.graph.bar_drag = None;
                }),
            )
            .on_mouse_move(
                cx.listener(|this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                    let bounds = *this.graph.bounds.borrow();
                    let local = event.position - bounds.origin;
                    if event.pressed_button == Some(gpui_kit::MouseButton::Left) {
                        if this.graph.bar_drag.is_some() {
                            if this.graph.drag_bar(local) {
                                cx.notify();
                            }
                            return;
                        }
                        if let Some(from) = this.graph.drag_from {
                            let delta = event.position - from;
                            this.graph.offset += delta;
                            this.graph.drag_from = Some(event.position);
                            this.graph.hover = None;
                            cx.notify();
                        }
                        return;
                    }
                    this.graph.drag_from = None;
                    this.graph.bar_drag = None;
                    let hot = this.graph.grab_bar(local).map(|grab| grab.axis);
                    if this.graph.bar_hot != hot {
                        this.graph.bar_hot = hot;
                        cx.notify();
                    }
                    let hover = match this.graph.hit(event.position) {
                        Some(Hit::Node(index)) => Some(index),
                        _ => None,
                    };
                    if this.graph.hover != hover {
                        this.graph.hover = hover;
                        cx.notify();
                    }
                }),
            )
            .on_scroll(cx.listener(|this, event: &gpui_kit::ScrollEvent, _, cx| {
                let steps = match event.delta {
                    gpui_kit::ScrollDelta::Lines(delta) => delta.y,
                    gpui_kit::ScrollDelta::Pixels(delta) => f32::from(delta.y) / 40.0,
                };
                let zoom = (this.graph.zoom * ZOOM_STEP.powf(steps)).clamp(ZOOM_MIN, ZOOM_MAX);
                if (zoom - this.graph.zoom).abs() < f32::EPSILON {
                    return;
                }
                let bounds = *this.graph.bounds.borrow();
                let cursor = (
                    f32::from(event.position.x - bounds.origin.x),
                    f32::from(event.position.y - bounds.origin.y),
                );
                let offset = (
                    f32::from(this.graph.offset.x),
                    f32::from(this.graph.offset.y),
                );
                let kept = zoom_around(cursor, offset, zoom / this.graph.zoom);
                this.graph.offset = point(px(kept.0), px(kept.1));
                this.graph.zoom = zoom;
                this.graph.fit_pending = false;
                cx.notify();
            }))
            .child(canvas_layer.absolute().inset_0())
            .children(labels)
            .children(pills)
            .children(hover);

        v_flex()
            .size_full()
            .min_h_0()
            .bg(theme::BG_CANVAS)
            .child(summary)
            .child(viewport)
            .child(self.graph_bar(model, cx))
            .into_any_element()
    }

    /// The slim bar under the graph: the legend, what is shown, and the zoom.
    fn graph_bar(&self, model: &Model, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .h(px(BAR_H))
            .flex_shrink_0()
            .px_3()
            .gap_3()
            .items_center()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme::LINE)
            .bg(theme::BG_PANEL)
            .text_size(px(11.))
            .text_color(theme::FG_3)
            .child(legend(theme::CHANGED.solid, ui::Marker::Changed, "Changed"))
            .child(legend(
                theme::EDIT.solid,
                ui::Marker::Diamond,
                "Needs editing",
            ))
            .child(legend(
                theme::BEHAVE.solid,
                ui::Marker::Dot,
                "May behave differently",
            ))
            .child(legend(theme::TEST.solid, ui::Marker::Dot, "Test"))
            .child(legend(theme::POSSIBLE, ui::Marker::Candidate, "Candidate"))
            .child(div().flex_1())
            .when(model.capped, |this| {
                this.child(
                    div()
                        .flex_none()
                        .text_color(theme::EDIT.fg)
                        .whitespace_nowrap()
                        .child(format!(
                            "{} of {} symbols shown",
                            model.nodes.len(),
                            model.all_nodes
                        )),
                )
            })
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("zoom-out")
                            .small()
                            .ghost()
                            .icon(IconName::Minus)
                            .tooltip("Zoom out")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let zoom = (this.graph.zoom / ZOOM_STEP).clamp(ZOOM_MIN, ZOOM_MAX);
                                this.graph.zoom_about_pane(zoom);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .w(px(38.))
                            .text_center()
                            .text_size(px(11.))
                            .text_color(theme::FG_3)
                            .child(format!("{:.0}%", self.graph.zoom * 100.0)),
                    )
                    .child(
                        Button::new("zoom-in")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .tooltip("Zoom in")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let zoom = (this.graph.zoom * ZOOM_STEP).clamp(ZOOM_MIN, ZOOM_MAX);
                                this.graph.zoom_about_pane(zoom);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("zoom-fit")
                            .small()
                            .ghost()
                            .icon(IconName::Maximize)
                            .tooltip("Fit the graph to the pane")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.graph.fit();
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The line above the graph: what the picture contains.
    fn graph_summary(&self, analysis: &Analysis, model: &Model) -> AnyElement {
        let counts = Counts::of(analysis);
        let tests = &analysis.tests;
        h_flex()
            .h(px(30.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme::LINE_SOFT)
            .text_size(TEXT_SMALL)
            .text_color(theme::FG_3)
            .child(count_text(
                counts.changes,
                "change",
                theme::CHANGED,
                "changes",
            ))
            .child(count_text(
                counts.needs_editing,
                "need editing",
                theme::EDIT,
                "need editing",
            ))
            .child(count_text(
                counts.edited,
                "edited with the change",
                theme::TEST,
                "edited with the change",
            ))
            .child(count_text(
                counts.may_behave,
                "may behave differently",
                theme::BEHAVE,
                "may behave differently",
            ))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::TEST.fg)
                    .child(format!(
                        "{} of {} tests to run",
                        tests.selected.len(),
                        tests.total
                    )),
            )
            .when(model.nodes.len() < model.all_nodes, |this| {
                this.child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::FG_4)
                        .child(format!(
                            "showing {} of {}",
                            model.nodes.len(),
                            model.all_nodes
                        )),
                )
            })
            .into_any_element()
    }
}

/// The vertical exit offset of every edge, in graph pixels.
///
/// The edges that leave one node are ordered by the height of their targets
/// and fan out over the middle of its right edge, so the bundle does not
/// collapse onto a single point.
fn exit_offsets(canvas: &Canvas) -> Vec<f32> {
    let mut exits = vec![0.0_f32; canvas.edges.len()];
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, edge) in canvas.edges.iter().enumerate() {
        groups.entry(edge.from).or_default().push(index);
    }
    for group in groups.values_mut() {
        group.sort_by(|left, right| {
            let height = |index: &usize| {
                canvas
                    .origins
                    .get(canvas.edges[*index].to)
                    .map_or(0.0, |(_, y)| *y)
            };
            height(left)
                .partial_cmp(&height(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (position, offset) in fan(group.len()).into_iter().enumerate() {
            if let Some(index) = group.get(position) {
                exits[*index] = offset;
            }
        }
    }
    exits
}

/// The offsets that spread `count` edges over the middle of a node's edge.
/// One edge leaves at the middle; more fan out evenly, at most
/// [`EDGE_SPREAD`] either way.
fn fan(count: usize) -> Vec<f32> {
    if count < 2 {
        return vec![0.0; count];
    }
    let step = EDGE_SPREAD * 2.0 / (count - 1) as f32;
    (0..count)
        .map(|position| position as f32 * step - EDGE_SPREAD)
        .collect()
}

/// Paints the guides, the references and the node boxes.
fn paint(canvas: &Canvas, bounds: Bounds<gpui_kit::Pixels>, window: &mut Window) {
    let origin = bounds.origin;
    let to_screen = |x: f32, y: f32| {
        point(
            origin.x + px(x) * canvas.zoom + px(canvas.offset.0),
            origin.y + px(y) * canvas.zoom + px(canvas.offset.1),
        )
    };

    // Lane guides: one faint line in the middle of every gap between columns.
    for layer in 0..canvas.depth.saturating_sub(1) {
        let x = Placed::column(layer) + NODE_W + layout::LAYER_GAP / 2.0;
        let left = origin.x + px(x) * canvas.zoom + px(canvas.offset.0);
        if left < origin.x || left > origin.x + bounds.size.width {
            continue;
        }
        window.paint_quad(quad(
            Bounds {
                origin: point(left, origin.y),
                size: size(px(1.), bounds.size.height),
            },
            px(0.),
            theme::LINE_SOFT.hsla(),
            px(0.),
            theme::LINE_SOFT.hsla(),
            BorderStyle::Solid,
        ));
    }

    // References: cubic beziers from the right edge of one box to the left
    // edge of the next. Every edge of one source leaves at its own height, so
    // a bundle into a full layer reads as separate lines.
    let exits = exit_offsets(canvas);
    for (index, edge) in canvas.edges.iter().enumerate() {
        let (Some((from_x, from_y)), Some((to_x, to_y))) = (
            canvas.origins.get(edge.from).copied(),
            canvas.origins.get(edge.to).copied(),
        ) else {
            continue;
        };
        let exit = exits.get(index).copied().unwrap_or(0.0);
        let start = to_screen(from_x + NODE_W, from_y + NODE_H / 2.0 + exit);
        let end = to_screen(to_x, to_y + NODE_H / 2.0);
        let reach = (end.x - start.x) * 0.5;
        let mut builder =
            PathBuilder::stroke(px(if edge.highlight { 2. } else { 1. }) * canvas.zoom);
        if edge.candidate {
            let dashes = [px(4.) * canvas.zoom, px(4.) * canvas.zoom];
            builder = builder.dash_array(&dashes);
        }
        builder.move_to(start);
        builder.cubic_bezier_to(
            end,
            point(start.x + reach, start.y),
            point(end.x - reach, end.y),
        );
        if let Ok(path) = builder.build() {
            let colour = if edge.highlight {
                theme::FG_3
            } else if edge.candidate {
                theme::POSSIBLE
            } else {
                EDGE_DIM
            };
            window.paint_path(path, colour.hsla());
        }
    }

    // Node boxes: a tinted fill, its border and the class accent on the left.
    for (index, (x, y)) in canvas.origins.iter().enumerate() {
        let accent = canvas.accents.get(index).copied().unwrap_or(theme::NEUTRAL);
        let selected = canvas.selected == Some(index);
        let top_left = to_screen(*x, *y);
        let box_size = size(px(NODE_W) * canvas.zoom, px(NODE_H) * canvas.zoom);
        let (fill, border, width) = if selected {
            (theme::BG_SELECTED, accent.solid, 1.5)
        } else {
            (accent.bg, accent.line, 1.0)
        };
        window.paint_quad(quad(
            Bounds {
                origin: top_left,
                size: box_size,
            },
            px(8.) * canvas.zoom,
            fill.hsla(),
            px(width) * canvas.zoom,
            border.hsla(),
            BorderStyle::Solid,
        ));
        let bar = Bounds {
            origin: point(top_left.x + px(1.), top_left.y + px(6.) * canvas.zoom),
            size: size(px(3.), px((NODE_H - 12.0).max(2.0)) * canvas.zoom),
        };
        window.paint_quad(quad(
            bar,
            px(1.5),
            accent.solid.hsla(),
            px(0.),
            accent.solid.hsla(),
            BorderStyle::Solid,
        ));
    }

    paint_bars(canvas, bounds, window);
}

/// Paints the two bars over the right and bottom edges of the canvas.
///
/// A bar is drawn only on an axis whose picture is larger than the pane, so a
/// picture that fits has nothing to grab and nothing to see.
fn paint_bars(canvas: &Canvas, bounds: Bounds<gpui_kit::Pixels>, window: &mut Window) {
    let track = ui::scroll::TRACK;
    let thumb = ui::scroll::THUMB;
    // The thumb is narrower than the track it runs in, and the two share a
    // centre line.
    let inset = (track - thumb) / 2.0;
    for (axis, bar) in [
        (ScrollbarAxis::Vertical, canvas.bars.vertical),
        (ScrollbarAxis::Horizontal, canvas.bars.horizontal),
    ] {
        let Some(bar) = bar else {
            continue;
        };
        let colour = theme::FG_4.hsla().alpha(if canvas.bar_hot == Some(axis) {
            ui::scroll::THUMB_HOT_ALPHA
        } else {
            ui::scroll::THUMB_ALPHA
        });
        let start = px(bar.start() + inset);
        let length = px((bar.length() - inset * 2.0).max(1.0));
        let (left, top) = if axis.is_vertical() {
            (
                bounds.origin.x + bounds.size.width - px(track) + px(inset),
                bounds.origin.y + start,
            )
        } else {
            (
                bounds.origin.x + start,
                bounds.origin.y + bounds.size.height - px(track) + px(inset),
            )
        };
        let area = if axis.is_vertical() {
            Bounds {
                origin: point(left, top),
                size: size(px(thumb), length),
            }
        } else {
            Bounds {
                origin: point(left, top),
                size: size(length, px(thumb)),
            }
        };
        window.paint_quad(quad(
            area,
            px(ui::scroll::RADIUS),
            colour,
            px(0.),
            colour,
            BorderStyle::Solid,
        ));
    }
}

/// The card that names a hovered node.
fn hover_card(
    node: &model::Node,
    x: f32,
    y: f32,
    zoom: f32,
    offset: Point<gpui_kit::Pixels>,
    pane: (f32, f32),
    mono: &gpui_kit::SharedString,
) -> AnyElement {
    let node_left = f32::from(px(x) * zoom + offset.x);
    let node_top = f32::from(px(y) * zoom + offset.y);
    let node_right = node_left + NODE_W * zoom;
    // Prefer the right of the node, and fall back to its left near the edge.
    let left = if node_right + CARD_W + 16.0 <= pane.0 {
        node_right + 8.0
    } else {
        (node_left - CARD_W - 8.0).max(4.0)
    };
    let top = (node_top - 4.0).clamp(4.0, (pane.1 - 96.0).max(4.0));

    v_flex()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(CARD_W))
        .gap_1()
        .p_2()
        .rounded(px(6.))
        .border_1()
        .border_color(theme::LINE_STRONG)
        .bg(theme::BG_RAISED_2)
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .truncate()
                .font_family(mono.clone())
                .text_size(px(11.))
                .text_color(theme::FG_2)
                .child(node.symbol.path.clone()),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .truncate()
                .text_size(px(11.))
                .text_color(theme::FG_3)
                .child(node.origin.clone()),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::FG_4)
                        .child(format!("confidence {}", node.confidence.as_str())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(theme::FG_4)
                        .child(node.site.clone()),
                ),
        )
        .into_any_element()
}

/// One legend entry.
///
/// The marker is drawn in the same gutter as in a list row, so a colour and a
/// shape mean the same thing in the same place wherever they are shown.
fn legend(colour: Tone, marker: ui::Marker, label: &'static str) -> AnyElement {
    h_flex()
        .flex_none()
        .gap(px(2.))
        .items_center()
        .line_height(ui::ROW_LINE)
        .text_size(px(11.))
        .text_color(theme::FG_3)
        .child(ui::marker(colour, marker))
        .child(label)
        .into_any_element()
}

/// A count in its semantic colour.
fn count_text(
    count: usize,
    singular: &'static str,
    accent: theme::Accent,
    plural: &'static str,
) -> AnyElement {
    div()
        .text_size(px(11.))
        .text_color(accent.fg)
        .child(format!(
            "{count} {}",
            if count == 1 { singular } else { plural }
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_are_one_gap_apart() {
        assert_eq!(Placed::column(0), layout::MARGIN);
        assert_eq!(
            Placed::column(1) - Placed::column(0),
            NODE_W + layout::LAYER_GAP
        );
    }

    #[test]
    fn one_bundle_fans_out_and_never_leaves_the_edge() {
        assert_eq!(fan(1), vec![0.0]);
        assert_eq!(fan(3), vec![-EDGE_SPREAD, 0.0, EDGE_SPREAD]);
        for count in 2..=40 {
            let offsets = fan(count);
            assert_eq!(offsets.len(), count);
            for offset in offsets {
                assert!(
                    offset.abs() <= EDGE_SPREAD + 0.001,
                    "an edge leaves the node at {offset}"
                );
            }
        }
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let offset = (30.0, -10.0);
        let cursor = (120.0, 90.0);
        let kept = zoom_around(cursor, offset, 2.0);
        // The graph point under the cursor before and after is the same.
        let before = ((cursor.0 - offset.0), (cursor.1 - offset.1));
        let after = ((cursor.0 - kept.0) / 2.0, (cursor.1 - kept.1) / 2.0);
        assert!((before.0 - after.0).abs() < 0.001);
        assert!((before.1 - after.1).abs() < 0.001);
    }

    #[test]
    fn a_test_symbol_is_green_whatever_its_class() {
        assert_eq!(
            node_accent(NodeClass::MayBehave, true).solid,
            theme::TEST.solid
        );
        assert_eq!(
            node_accent(NodeClass::MayBehave, false).solid,
            theme::BEHAVE.solid
        );
        assert_eq!(
            node_accent(NodeClass::Changed, true).solid,
            theme::CHANGED.solid
        );
    }

    /// A picture of the size the bars are measured against.
    fn picture(width: f32, height: f32) -> Placed {
        Placed {
            width,
            height,
            ..Placed::default()
        }
    }

    #[test]
    fn expanding_keeps_the_anchor_on_the_same_pixel() {
        let zoom = 1.4;
        let offset = (40.0, -12.0);
        // The pill stood here, and the layer growing under it pushed it down.
        let before = (24.0, 200.0);
        let after = (24.0, 290.0);
        let kept = keep_anchor(offset, zoom, before, after);
        let screen =
            |offset: (f32, f32), at: (f32, f32)| (at.0 * zoom + offset.0, at.1 * zoom + offset.1);
        let was = screen(offset, before);
        let now = screen(kept, after);
        assert!((was.0 - now.0).abs() < 0.001, "the anchor moved sideways");
        assert!((was.1 - now.1).abs() < 0.001, "the anchor moved down");
        // Only the pan moved; the zoom is exactly what it was.
        assert!((kept.1 - offset.1 + 90.0 * zoom).abs() < 0.001);
        // An element that did not move leaves the pan alone.
        assert_eq!(keep_anchor(offset, zoom, before, before), offset);
    }

    #[test]
    fn a_picture_that_fits_has_no_bars() {
        let placed = picture(300.0, 200.0);
        assert_eq!(
            Bars::of(&placed, (900.0, 500.0), (0.0, 0.0), 1.0),
            Bars::default()
        );
        // One axis at a time: what fits across does not fit down.
        let bars = Bars::of(&placed, (300.0, 150.0), (0.0, 0.0), 1.0);
        assert!(bars.vertical.is_some());
        assert!(bars.horizontal.is_none());
        let bars = Bars::of(&placed, (150.0, 400.0), (0.0, 0.0), 1.0);
        assert!(bars.vertical.is_none());
        assert!(bars.horizontal.is_some());
    }

    #[test]
    fn the_bars_follow_the_pan_and_the_zoom() {
        let placed = picture(1000.0, 800.0);
        let pane = (400.0, 400.0);
        let at = |offset: f32, zoom: f32| {
            Bars::of(&placed, pane, (0.0, offset), zoom)
                .vertical
                .expect("a picture taller than the pane")
        };
        // At the head of the picture the thumb is at the head of the track.
        assert_eq!(at(0.0, 1.0).start(), 0.0);
        // At the far end of it the thumb is against the far end of the track.
        let far = at(-400.0, 1.0);
        assert!((far.start() - (pane.1 - far.length())).abs() < 0.001);
        // Zooming in shows less of the picture, so the thumb gets shorter.
        assert!(at(0.0, 2.0).length() < at(0.0, 1.0).length());
    }

    #[test]
    fn a_press_on_a_bar_takes_the_thumb_with_it() {
        let placed = picture(1000.0, 800.0);
        let bar = Bars::of(&placed, (400.0, 400.0), (0.0, 0.0), 1.0)
            .vertical
            .expect("a picture taller than the pane");
        // A press on the track puts the middle of the thumb under the pointer.
        assert!((bar_grab(bar, 300.0) - bar.length() / 2.0).abs() < 0.001);
        // A press on the thumb keeps the spot it grabbed where it was.
        let start = bar.start();
        let grab = bar_grab(bar, start + 10.0);
        assert!((grab - 10.0).abs() < 0.001);
        assert!(bar.offset_at(start + 10.0, grab).abs() < 0.001);
    }
}
