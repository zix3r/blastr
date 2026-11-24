//! The graph of the focused change: references painted on a canvas, the boxes
//! laid over it. Drag to pan, scroll to move, Ctrl and scroll to zoom.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use blastr_model::{Confidence, SymbolPath, wording};
use gpui_kit::component::{Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Bounds, Context, Entity, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement, PathBuilder, Pixels, Point, Render, ScrollDelta, ScrollEvent, Styled,
    Subscription, Window, canvas, div, point, px,
};

use super::layout::{self, NODE_H, NODE_W, Placed};
use super::model::{self, Model, NodeKind};
use crate::theme::{self, Accent, TEXT_MICRO, TEXT_SMALL};
use crate::views::ui;
use crate::workspace::Workspace;

/// What the graph was built from; a different key builds it again.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    analysis: usize,
    change: usize,
    confidence: Confidence,
    selected: Option<SymbolPath>,
    expanded: usize,
}

/// The graph view.
pub(crate) struct GraphView {
    workspace: Entity<Workspace>,
    model: Model,
    placed: Placed,
    key: Option<Key>,
    expanded: HashSet<SymbolPath>,
    zoom: f32,
    offset: Point<Pixels>,
    fit_pending: bool,
    drag: Option<Point<Pixels>>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    _subscription: Subscription,
}

impl GraphView {
    pub(crate) fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> GraphView {
        let subscription = cx.observe(&workspace, |this, _, cx| {
            this.sync(cx);
            cx.notify();
        });
        GraphView {
            workspace,
            model: Model::default(),
            placed: Placed::default(),
            key: None,
            expanded: HashSet::new(),
            zoom: 1.0,
            offset: point(px(0.), px(0.)),
            fit_pending: true,
            drag: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            _subscription: subscription,
        }
    }

    /// Builds the graph again when what it shows changed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let workspace = self.workspace.read(cx);
        let (Some(analysis), Some(change)) =
            (workspace.session.analysis(), workspace.focus_change())
        else {
            self.model = Model::default();
            self.placed = Placed::default();
            self.key = None;
            return;
        };
        let selected = workspace
            .session
            .selection()
            .map(|selection| selection.symbol.clone());
        let key = Key {
            analysis: Arc::as_ptr(analysis) as usize,
            change,
            confidence: workspace.filter.min_confidence,
            selected,
            expanded: self.expanded.len(),
        };
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let scene_changed = self
            .key
            .as_ref()
            .is_none_or(|old| old.analysis != key.analysis || old.change != key.change);
        if scene_changed && !self.expanded.is_empty() {
            self.expanded.clear();
            return self.sync(cx);
        }
        self.model = model::build(
            analysis,
            change,
            key.confidence,
            &self.expanded,
            key.selected.as_ref(),
        );
        self.placed = layout::layout(&self.model);
        self.fit_pending |= scene_changed;
        self.key = Some(key);
    }

    fn fit(&mut self) {
        let size = self.bounds.get().size;
        let (zoom, (x, y)) = layout::fit(
            &self.placed,
            (f32::from(size.width), f32::from(size.height)),
        );
        self.zoom = zoom;
        self.offset = point(px(x), px(y));
    }

    fn zoom_by(&mut self, factor: f32, around: Point<Pixels>) {
        let zoom = (self.zoom * factor).clamp(0.3, 2.0);
        let ratio = zoom / self.zoom;
        self.offset = around - (around - self.offset) * ratio;
        self.zoom = zoom;
    }

    fn click_node(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(node) = self.model.nodes.get(index) else {
            return;
        };
        match (&node.kind, &node.symbol) {
            (NodeKind::More { parent, .. }, _) => {
                self.expanded.insert(parent.clone());
                self.sync(cx);
                cx.notify();
            }
            (_, Some(symbol)) => {
                let symbol = symbol.clone();
                self.workspace
                    .update(cx, |workspace, cx| workspace.select(symbol, None, cx));
            }
            _ => {}
        }
    }

    fn node(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let node = &self.model.nodes[index];
        let (x, y) = self.placed.origins[index];
        let zoom = self.zoom;
        let selected = self.key.as_ref().and_then(|key| key.selected.as_ref());
        let is_selected = node.symbol.is_some() && node.symbol.as_ref() == selected;
        let accent = node_accent(&node.kind);
        let more = matches!(node.kind, NodeKind::More { .. });
        let (fill, border) = match () {
            _ if is_selected => (theme::BG_SELECTED, accent.solid),
            _ if more => (theme::BG_RAISED, theme::LINE),
            _ if node.on_path => (accent.bg, accent.solid),
            _ => (accent.bg, accent.line),
        };
        let chip = match &node.kind {
            NodeKind::Change => self
                .workspace
                .read(cx)
                .session
                .analysis()
                .and_then(|analysis| analysis.diff.changes.get(self.key.as_ref()?.change))
                .map(wording::change_label),
            NodeKind::Item { is_test: true, .. } => Some("test".to_owned()),
            NodeKind::Item {
                confidence: Confidence::Possible,
                ..
            } => Some("possible".to_owned()),
            _ => None,
        };
        div()
            .id(("graph-node", index))
            .absolute()
            .left(self.offset.x + px(x * zoom))
            .top(self.offset.y + px(y * zoom))
            .w(px(NODE_W * zoom))
            .h(px(NODE_H * zoom))
            .rounded(px(8. * zoom))
            .border_1()
            .border_color(border)
            .bg(fill)
            .overflow_hidden()
            .cursor_pointer()
            .hover(|style| style.border_color(accent.solid))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| this.click_node(index, cx)))
            .when(more, |this| {
                this.flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.5 * zoom.max(0.8)))
                    .text_color(theme::FG_3)
                    .child(node.name.clone())
            })
            .when(!more, |this| {
                this.flex()
                    .child(
                        div()
                            .w(px(3.))
                            .my(px(6. * zoom))
                            .rounded(px(1.5))
                            .bg(accent.solid),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .justify_center()
                            .pl(px(9. * zoom))
                            .pr(px(8. * zoom))
                            .gap(px(2. * zoom))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(theme::mono())
                                            .text_size(px(12. * zoom))
                                            .text_color(if matches!(node.kind, NodeKind::Step) {
                                                theme::FG_3
                                            } else {
                                                theme::FG
                                            })
                                            .child(node.name.clone()),
                                    )
                                    .children(chip.map(|chip| {
                                        div()
                                            .flex_none()
                                            .px(px(4.))
                                            .rounded(px(3.))
                                            .bg(accent.bg)
                                            .text_size(px(9. * zoom.max(0.85)))
                                            .text_color(accent.fg)
                                            .child(chip.to_uppercase())
                                    })),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(10.5 * zoom))
                                    .text_color(theme::FG_4)
                                    .child(node.subtitle.clone()),
                            ),
                    )
            })
            .into_any_element()
    }

    fn bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let legend = |accent: Accent, shape: ui::Marker, label: &'static str| {
            h_flex()
                .gap_1()
                .items_center()
                .child(ui::marker(accent.solid, shape))
                .child(label)
        };
        h_flex()
            .h(px(30.))
            .flex_none()
            .px_2()
            .gap_3()
            .items_center()
            .border_t_1()
            .border_color(theme::LINE)
            .bg(theme::BG_PANEL)
            .text_size(TEXT_MICRO)
            .text_color(theme::FG_3)
            .child(legend(theme::CHANGED, ui::Marker::Changed, "Changed"))
            .child(legend(theme::EDIT, ui::Marker::Diamond, "Needs editing"))
            .child(legend(
                theme::BEHAVE,
                ui::Marker::Dot,
                "May behave differently",
            ))
            .child(legend(theme::TEST, ui::Marker::Dot, "Test"))
            .child(legend(theme::NEUTRAL, ui::Marker::Candidate, "Candidate"))
            .child(div().flex_1())
            .child(
                ui::icon_button("zoom-out", IconName::Minus, "Zoom out").on_click(cx.listener(
                    |this, _, _, cx| {
                        let centre = this.bounds.get().size.center();
                        this.zoom_by(1.0 / 1.2, centre);
                        cx.notify();
                    },
                )),
            )
            .child(
                div()
                    .w(px(36.))
                    .text_center()
                    .child(format!("{:.0}%", self.zoom * 100.0)),
            )
            .child(
                ui::icon_button("zoom-in", IconName::Plus, "Zoom in").on_click(cx.listener(
                    |this, _, _, cx| {
                        let centre = this.bounds.get().size.center();
                        this.zoom_by(1.2, centre);
                        cx.notify();
                    },
                )),
            )
            .child(
                ui::icon_button("fit", IconName::Frame, "Fit the graph").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.fit();
                        cx.notify();
                    },
                )),
            )
            .into_any_element()
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let change = self
            .key
            .as_ref()
            .and_then(|key| workspace.session.analysis()?.diff.changes.get(key.change));
        let name = change
            .map(|change| wording::short_name(&change.symbol.path).name)
            .unwrap_or_default();
        let reached = self.model.reached;
        h_flex()
            .h(px(32.))
            .flex_none()
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme::LINE)
            .bg(theme::BG_PANEL)
            .text_size(TEXT_SMALL)
            .text_color(theme::FG_3)
            .child(
                Icon::new(IconName::Network)
                    .size_3()
                    .text_color(theme::FG_4),
            )
            .child("Where")
            .child(
                div()
                    .font_family(theme::mono())
                    .text_color(theme::CHANGED.fg)
                    .child(name),
            )
            .child(match reached {
                0 => "reaches nothing listed".to_owned(),
                1 => "reaches 1 item".to_owned(),
                count => format!("reaches {count} items"),
            })
            .into_any_element()
    }
}

impl Render for GraphView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = self.workspace.read(cx);
        if workspace.session.analysis().is_none() {
            let loading = workspace.session.shown().is_loading();
            return ui::empty_state(
                if loading {
                    IconName::Loader
                } else {
                    IconName::Network
                },
                Some(theme::CHANGED),
                if loading {
                    "Tracing impact"
                } else {
                    "No result yet"
                },
                if loading {
                    "The graph appears with the result."
                } else {
                    "Choose what to compare in the title bar."
                },
            );
        }
        if self.model.nodes.is_empty() {
            return ui::empty_state(
                IconName::Network,
                Some(theme::BEHAVE),
                "Select a change",
                "The graph shows where the selected change reaches, step by step.",
            );
        }
        if self.fit_pending {
            if self.bounds.get().size.width > px(0.) {
                self.fit_pending = false;
                self.fit();
            } else {
                window.request_animation_frame();
            }
        }
        let edges = self.model.edges.clone();
        let origins = self.placed.origins.clone();
        let (zoom, offset, bounds) = (self.zoom, self.offset, self.bounds.clone());
        let picture = canvas(
            move |area, _, _| bounds.set(area),
            move |area, (), window, _| {
                let at = |x: f32, y: f32| area.origin + offset + point(px(x * zoom), px(y * zoom));
                for edge in &edges {
                    let (from, to) = (origins[edge.from], origins[edge.to]);
                    let start = at(from.0 + NODE_W, from.1 + NODE_H / 2.0);
                    let end = at(to.0, to.1 + NODE_H / 2.0);
                    let reach = (end.x - start.x) * 0.5;
                    let width = if edge.highlight { 2.0 } else { 1.0 };
                    let mut path = PathBuilder::stroke(px(width * zoom.max(0.6)));
                    let dashes = [px(4. * zoom), px(4. * zoom)];
                    if edge.candidate {
                        path = path.dash_array(&dashes);
                    }
                    path.move_to(start);
                    path.cubic_bezier_to(
                        end,
                        point(start.x + reach, start.y),
                        point(end.x - reach, end.y),
                    );
                    let colour = if edge.highlight {
                        theme::FG_2
                    } else if edge.candidate {
                        theme::POSSIBLE
                    } else {
                        theme::LINE_STRONG
                    };
                    if let Ok(path) = path.build() {
                        window.paint_path(path, colour.hsla());
                    }
                }
            },
        );
        let nodes: Vec<AnyElement> = (0..self.model.nodes.len())
            .map(|index| self.node(index, cx))
            .collect();
        v_flex()
            .size_full()
            .bg(theme::BG_CANVAS)
            .child(self.header(cx))
            .child(
                div()
                    .id("graph")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, _| {
                            this.drag = Some(event.position)
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.drag = None),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        let Some(from) = this.drag else {
                            return;
                        };
                        if event.pressed_button != Some(MouseButton::Left) {
                            this.drag = None;
                            return;
                        }
                        this.offset += event.position - from;
                        this.drag = Some(event.position);
                        cx.notify();
                    }))
                    .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                        let delta = match event.delta {
                            ScrollDelta::Lines(lines) => {
                                point(px(lines.x * 40.), px(lines.y * 40.))
                            }
                            ScrollDelta::Pixels(pixels) => pixels,
                        };
                        if event.modifiers.control {
                            let around = event.position - this.bounds.get().origin;
                            this.zoom_by(1.1_f32.powf(f32::from(delta.y) / 40.0), around);
                        } else {
                            this.offset += delta;
                        }
                        cx.notify();
                    }))
                    .child(picture.absolute().size_full())
                    .children(nodes),
            )
            .child(self.bar(cx))
            .into_any_element()
    }
}

/// The colours of a node.
fn node_accent(kind: &NodeKind) -> Accent {
    match kind {
        NodeKind::Change => theme::CHANGED,
        NodeKind::Item { is_test: true, .. } => theme::TEST,
        NodeKind::Item { class, .. } => ui::class_accent(*class),
        NodeKind::Step | NodeKind::More { .. } => theme::NEUTRAL,
    }
}
