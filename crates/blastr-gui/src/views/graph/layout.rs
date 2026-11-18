//! Where the nodes of the graph go, and how the picture is fitted to the pane.
//!
//! Left to right is distance from the change: layer zero holds the changes,
//! layer one what they reach directly, and so on. Within a layer the nodes are
//! ordered by the barycentre of their predecessors, which keeps most edges
//! short and crossings down. Layers are laid out from the top; a layer is only
//! shifted down to sit near the predecessors it depends on, so a single change
//! is never pushed thousands of pixels away by one far taller layer.
//!
//! Everything here is pure and deterministic: the same model always gives the
//! same picture.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::model::Model;

/// The width of a drawn node.
pub(crate) const NODE_W: f32 = 200.0;
/// The height of a drawn node.
pub(crate) const NODE_H: f32 = 44.0;
/// The width of the pill that stands for a layer's hidden symbols: as wide as
/// a node, centred under its column.
pub(crate) const PILL_W: f32 = NODE_W;
/// The height of the pill that stands for a layer's hidden symbols.
pub(crate) const PILL_H: f32 = 20.0;
/// The vertical space between a layer's last node and its pill.
pub(crate) const PILL_GAP: f32 = 10.0;
/// The horizontal space between two layers.
pub(crate) const LAYER_GAP: f32 = 56.0;
/// The vertical space between two nodes of one layer.
pub(crate) const NODE_GAP: f32 = 16.0;
/// The margin around the whole picture.
pub(crate) const MARGIN: f32 = 24.0;
/// The gap kept between the picture and the edge of the pane when fitting.
pub(crate) const FIT_MARGIN: f32 = 20.0;
/// The smallest zoom a fit chooses: below this the labels stop being legible,
/// so the picture is framed from its top-left and panned instead.
pub(crate) const MIN_FIT_ZOOM: f32 = 0.85;
/// The largest zoom a fit chooses.
pub(crate) const MAX_FIT_ZOOM: f32 = 1.2;

/// The placed nodes, in model order.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Placed {
    /// The top-left corner of every node, in graph coordinates.
    pub origins: Vec<(f32, f32)>,
    /// The layer of every node.
    pub layers: Vec<usize>,
    /// The top-left corner of every collapsed pill, in model order.
    pub pills: Vec<(f32, f32)>,
    /// The layer every collapsed pill stands for, in model order.
    pub pill_layers: Vec<usize>,
    /// The width of the whole picture.
    pub width: f32,
    /// The height of the whole picture.
    pub height: f32,
}

impl Placed {
    /// The left edge of a layer's column, in graph coordinates.
    pub(crate) fn column(layer: usize) -> f32 {
        MARGIN + layer as f32 * (NODE_W + LAYER_GAP)
    }
}

/// A pan and a zoom that put a rectangle of the picture on the pane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct View {
    /// The zoom, between [`MIN_FIT_ZOOM`] and [`MAX_FIT_ZOOM`].
    pub zoom: f32,
    /// Where the picture's origin sits on the pane.
    pub offset: (f32, f32),
}

/// An axis-aligned rectangle of the picture, in graph coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    /// The rectangle around everything, for a picture of `width` and `height`.
    fn whole(width: f32, height: f32) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: width,
            h: height,
        }
    }

    /// The smallest rectangle holding both.
    fn union(self, other: Rect) -> Rect {
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = (self.x + self.w).max(other.x + other.w);
        let bottom = (self.y + self.h).max(other.y + other.h);
        Rect {
            x: left,
            y: top,
            w: right - left,
            h: bottom - top,
        }
    }
}

/// Places the nodes of `model`.
pub(crate) fn layout(model: &Model) -> Placed {
    let layers = model.layers();
    let depth = layers.len();
    if depth == 0 {
        return Placed::default();
    }

    // The order inside each layer starts as the model's order, which is the
    // ranked order, and is then improved by the barycentre of the neighbours
    // in the previous layer. Two passes keep the result stable.
    let mut order: Vec<Vec<usize>> = layers.clone();
    let mut slot = vec![0usize; model.nodes.len()];
    let mut layer_of = vec![0usize; model.nodes.len()];
    for (layer_index, layer) in order.iter().enumerate() {
        for (position, node) in layer.iter().enumerate() {
            slot[*node] = position;
            layer_of[*node] = layer_index;
        }
    }
    for _ in 0..2 {
        for layer in order.iter_mut().skip(1) {
            let mut keyed: Vec<(f32, usize, usize)> = layer
                .iter()
                .map(|&node| {
                    let mut sum = 0.0_f32;
                    let mut count = 0usize;
                    for edge in &model.edges {
                        if edge.to == node {
                            sum += slot[edge.from] as f32;
                            count += 1;
                        }
                    }
                    let barycentre = if count == 0 {
                        slot[node] as f32
                    } else {
                        sum / count as f32
                    };
                    (barycentre, slot[node], node)
                })
                .collect();
            keyed.sort_by(|left, right| {
                left.0
                    .partial_cmp(&right.0)
                    .unwrap_or(Ordering::Equal)
                    .then(left.1.cmp(&right.1))
            });
            *layer = keyed.into_iter().map(|(_, _, node)| node).collect();
            for (position, node) in layer.iter().enumerate() {
                slot[*node] = position;
            }
        }
    }

    // Where each collapsed pill goes: one per layer, under that layer's nodes.
    let pill_of: BTreeMap<usize, usize> = model
        .collapsed
        .iter()
        .enumerate()
        .map(|(pill, collapsed)| (collapsed.layer, pill))
        .collect();
    let heights: Vec<f32> = (0..depth)
        .map(|layer| {
            let stack = stack_height(order[layer].len());
            if pill_of.contains_key(&layer) {
                stack + PILL_GAP + PILL_H
            } else {
                stack
            }
        })
        .collect();

    // The first layer starts at the margin; a later one is centred on the mean
    // height of its predecessors, and never rises above the margin.
    let mut tops = vec![MARGIN; depth];
    for layer_index in 1..depth {
        let mut sum = 0.0_f32;
        let mut count = 0usize;
        for &node in &order[layer_index] {
            for edge in &model.edges {
                if edge.to != node {
                    continue;
                }
                let from = edge.from;
                let y =
                    tops[layer_of[from]] + slot[from] as f32 * (NODE_H + NODE_GAP) + NODE_H / 2.0;
                sum += y;
                count += 1;
            }
        }
        if count > 0 && heights[layer_index] > 0.0 {
            let barycentre = sum / count as f32;
            tops[layer_index] = (barycentre - heights[layer_index] / 2.0).max(MARGIN);
        }
    }

    let mut origins = vec![(0.0, 0.0); model.nodes.len()];
    let mut layers_of = vec![0usize; model.nodes.len()];
    let mut pills = vec![(0.0, 0.0); model.collapsed.len()];
    let mut pill_layers = vec![0usize; model.collapsed.len()];
    for (layer_index, layer) in order.iter().enumerate() {
        let x = Placed::column(layer_index);
        for (position, node) in layer.iter().enumerate() {
            let y = tops[layer_index] + position as f32 * (NODE_H + NODE_GAP);
            origins[*node] = (x, y);
            layers_of[*node] = layer_index;
        }
        if let Some(&pill) = pill_of.get(&layer_index) {
            let y = tops[layer_index] + stack_height(layer.len()) + PILL_GAP;
            pills[pill] = (x, y);
            pill_layers[pill] = layer_index;
        }
    }

    let height = (0..depth)
        .map(|layer| tops[layer] + heights[layer])
        .fold(0.0_f32, f32::max);
    Placed {
        origins,
        layers: layers_of,
        pills,
        pill_layers,
        width: MARGIN * 2.0 + depth as f32 * (NODE_W + LAYER_GAP) - LAYER_GAP,
        height: height + MARGIN,
    }
}

/// Fits the picture into a pane.
///
/// The whole picture is shown when it fits at [`MIN_FIT_ZOOM`]. When it does
/// not, the picture keeps that legible zoom, is framed from its top-left corner
/// and the reader pans: the first two layers and the selected node stay in
/// sight, and nothing is shrunk into illegibility.
pub(crate) fn fit(placed: &Placed, pane: (f32, f32), selected: Option<usize>) -> View {
    if placed.origins.is_empty() || pane.0 < 1.0 || pane.1 < 1.0 {
        return View {
            zoom: 1.0,
            offset: (FIT_MARGIN, FIT_MARGIN),
        };
    }
    let whole = Rect::whole(placed.width, placed.height);
    let natural = natural_zoom(whole, pane);
    if natural >= MIN_FIT_ZOOM {
        let zoom = natural.min(MAX_FIT_ZOOM);
        return View {
            zoom,
            offset: place(whole, pane, zoom),
        };
    }

    // Too large to fit: show the change, its direct dependants and the
    // selection instead of shrinking the picture into illegibility.
    let mut focus: Option<Rect> = None;
    for (index, (x, y)) in placed.origins.iter().enumerate() {
        let layer = placed.layers.get(index).copied().unwrap_or(0);
        if layer > 1 && selected != Some(index) {
            continue;
        }
        let rect = Rect {
            x: *x,
            y: *y,
            w: NODE_W,
            h: NODE_H,
        };
        focus = Some(focus.map_or(rect, |held| held.union(rect)));
    }
    for (pill, (x, y)) in placed.pills.iter().enumerate() {
        if placed.pill_layers.get(pill).copied().unwrap_or(0) > 1 {
            continue;
        }
        let rect = Rect {
            x: *x,
            y: *y,
            w: PILL_W,
            h: PILL_H,
        };
        focus = Some(focus.map_or(rect, |held| held.union(rect)));
    }
    let focus = focus.unwrap_or(whole);
    let zoom = MIN_FIT_ZOOM;
    View {
        zoom,
        offset: place(focus, pane, zoom),
    }
}

/// Brings a newly selected node into sight without touching the zoom.
///
/// The view is returned unchanged unless the node lies entirely outside the
/// pane; then it is centred, so a selection never refits or rescales a picture
/// whose node the reader can already see.
pub(crate) fn after_selection(
    placed: &Placed,
    index: Option<usize>,
    pane: (f32, f32),
    view: View,
) -> View {
    let Some(index) = index else {
        return view;
    };
    let Some(&(x, y)) = placed.origins.get(index) else {
        return view;
    };
    if pane.0 < 1.0 || pane.1 < 1.0 {
        return view;
    }
    let left = x * view.zoom + view.offset.0;
    let top = y * view.zoom + view.offset.1;
    let right = left + NODE_W * view.zoom;
    let bottom = top + NODE_H * view.zoom;
    if right >= 0.0 && left <= pane.0 && bottom >= 0.0 && top <= pane.1 {
        return view;
    }
    let offset = (
        (pane.0 - NODE_W * view.zoom) / 2.0 - x * view.zoom,
        (pane.1 - NODE_H * view.zoom) / 2.0 - y * view.zoom,
    );
    View {
        zoom: view.zoom,
        offset,
    }
}

/// The zoom at which `rect` would fill a pane, before any clamping.
fn natural_zoom(rect: Rect, pane: (f32, f32)) -> f32 {
    let width = (pane.0 - FIT_MARGIN * 2.0).max(1.0);
    let height = (pane.1 - FIT_MARGIN * 2.0).max(1.0);
    (width / rect.w.max(1.0))
        .min(height / rect.h.max(1.0))
        .max(0.01)
}

/// The offset that centres `rect` in a pane at `zoom`.
///
/// A picture wider or taller than the pane keeps its top-left corner at the
/// margin instead of being centred out of sight, so the change at the head of
/// the graph is always on screen.
fn place(rect: Rect, pane: (f32, f32), zoom: f32) -> (f32, f32) {
    let x = (pane.0 - rect.w * zoom) / 2.0 - rect.x * zoom;
    let y = (pane.1 - rect.h * zoom) / 2.0 - rect.y * zoom;
    (
        x.max(FIT_MARGIN - rect.x * zoom),
        y.max(FIT_MARGIN - rect.y * zoom),
    )
}

/// The height of `count` stacked nodes, including the gaps between them.
fn stack_height(count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    count as f32 * NODE_H + (count.saturating_sub(1)) as f32 * NODE_GAP
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::graph::model::{self, Node, NodeClass, PER_LAYER};
    use blastr_model::{Confidence, SymbolKind, SymbolPath};

    fn node(index: usize, distance: u32) -> Node {
        Node {
            symbol: SymbolPath::new(format!("k::n{index}"), SymbolKind::Function),
            name: format!("n{index}"),
            owner: None,
            module: "k".to_owned(),
            class: NodeClass::MayBehave,
            confidence: Confidence::Likely,
            distance,
            hidden: 0,
            is_test: false,
            edited: false,
            chip: None,
            site: "src/lib.rs:1".to_owned(),
            origin: "the change".to_owned(),
            selected: false,
        }
    }

    fn model(nodes: Vec<Node>, edges: Vec<(usize, usize)>) -> model::Model {
        model::Model {
            all_nodes: nodes.len(),
            nodes,
            collapsed: Vec::new(),
            edges: edges
                .into_iter()
                .map(|(from, to)| model::Edge {
                    from,
                    to,
                    candidate: false,
                    highlight: false,
                })
                .collect(),
            capped: false,
        }
    }

    fn overlaps(left: (f32, f32), right: (f32, f32)) -> bool {
        let (lx, ly) = left;
        let (rx, ry) = right;
        lx < rx + NODE_W && rx < lx + NODE_W && ly < ry + NODE_H && ry < ly + NODE_H
    }

    /// The four corners of a node on the pane, after a fit.
    fn screen_rect(placed: &Placed, index: usize, view: View) -> Rect {
        let (x, y) = placed.origins[index];
        Rect {
            x: x * view.zoom + view.offset.0,
            y: y * view.zoom + view.offset.1,
            w: NODE_W * view.zoom,
            h: NODE_H * view.zoom,
        }
    }

    #[test]
    fn an_empty_graph_has_no_size() {
        assert_eq!(layout(&model(Vec::new(), Vec::new())), Placed::default());
    }

    #[test]
    fn nodes_of_one_layer_never_overlap() {
        let model = model(
            vec![node(0, 0), node(1, 1), node(2, 1), node(3, 1), node(4, 2)],
            vec![(0, 1), (0, 2), (0, 3), (1, 4), (2, 4)],
        );
        let placed = layout(&model);
        for left in 0..model.nodes.len() {
            for right in left + 1..model.nodes.len() {
                assert!(
                    !overlaps(placed.origins[left], placed.origins[right]),
                    "{} and {right} overlap",
                    model.nodes[left].symbol
                );
            }
        }
    }

    #[test]
    fn a_pill_never_overlaps_the_nodes_of_its_layer() {
        let mut model = model(
            vec![node(0, 0), node(1, 1), node(2, 1), node(3, 1)],
            vec![(0, 1), (0, 2), (0, 3)],
        );
        model.collapsed = vec![model::Collapsed {
            layer: 1,
            hidden: 40,
        }];
        let placed = layout(&model);
        let (px_, py_) = placed.pills[0];
        for (x, y) in &placed.origins {
            let collides =
                *x < px_ + PILL_W && px_ < *x + NODE_W && *y < py_ + PILL_H && py_ < *y + NODE_H;
            assert!(!collides, "the pill overlaps a node");
        }
        assert!(py_ + PILL_H <= placed.height);
    }

    #[test]
    fn every_edge_points_to_a_later_layer() {
        let model = model(
            vec![node(0, 0), node(1, 1), node(2, 1), node(3, 2)],
            vec![(0, 1), (0, 2), (1, 3), (2, 3)],
        );
        let placed = layout(&model);
        for edge in &model.edges {
            assert!(
                placed.origins[edge.from].0 < placed.origins[edge.to].0,
                "an edge points backwards"
            );
            assert!(placed.layers[edge.from] < placed.layers[edge.to]);
        }
    }

    #[test]
    fn the_layout_is_deterministic() {
        let model = model(
            vec![node(0, 0), node(1, 1), node(2, 1), node(3, 2)],
            vec![(0, 2), (0, 1), (2, 3), (1, 3)],
        );
        assert_eq!(layout(&model), layout(&model));
    }

    #[test]
    fn the_barycentre_keeps_crossings_down() {
        // Both layer-one nodes depend on n0; the order must be the model's.
        let model = model(
            vec![node(0, 0), node(1, 1), node(2, 1), node(3, 2)],
            vec![(0, 1), (0, 2), (1, 3), (2, 3)],
        );
        let placed = layout(&model);
        assert_eq!(placed.layers, vec![0, 1, 1, 2]);
        assert!(placed.origins[1].1 < placed.origins[2].1);
    }

    #[test]
    fn every_node_lies_inside_the_picture() {
        let model = model(
            vec![node(0, 0), node(1, 1), node(2, 2)],
            vec![(0, 1), (1, 2)],
        );
        let placed = layout(&model);
        for (x, y) in &placed.origins {
            assert!(*x >= 0.0 && *x + NODE_W <= placed.width);
            assert!(*y >= 0.0 && *y + NODE_H <= placed.height);
        }
    }

    #[test]
    fn a_far_taller_layer_does_not_push_the_change_away() {
        // One change and sixty dependants: the change stays at the top and the
        // picture is as tall as the tall layer, not twice as tall.
        let mut nodes = vec![node(0, 0)];
        let mut edges = Vec::new();
        for index in 1..=60 {
            nodes.push(node(index, 1));
            edges.push((0, index));
        }
        let model = model(nodes, edges);
        let placed = layout(&model);
        assert!(placed.origins[0].1 <= MARGIN + 1.0);
        assert!(placed.height <= stack_height(60) + MARGIN * 2.0);
    }

    #[test]
    fn the_change_is_in_view_after_a_fit() {
        // The real shape of a bad result: one change and 156 dependants.
        let mut nodes = vec![node(0, 0)];
        let mut edges = Vec::new();
        for index in 1..=PER_LAYER {
            nodes.push(node(index, 1));
            edges.push((0, index));
        }
        let mut model = model(nodes, edges);
        model.collapsed = vec![model::Collapsed {
            layer: 1,
            hidden: 156 - PER_LAYER,
        }];
        for pane in [(1200.0, 500.0), (700.0, 420.0), (400.0, 300.0)] {
            let placed = layout(&model);
            let view = fit(&placed, pane, None);
            assert!(view.zoom >= MIN_FIT_ZOOM);
            let change = screen_rect(&placed, 0, view);
            assert!(
                change.x >= 0.0
                    && change.y >= 0.0
                    && change.x + change.w <= pane.0
                    && change.y + change.h <= pane.1,
                "the change is outside a {pane:?} pane: {change:?}"
            );
        }
    }

    #[test]
    fn a_fit_never_shrinks_a_readable_picture_below_the_minimum() {
        let mut nodes = vec![node(0, 0)];
        for index in 1..=40 {
            nodes.push(node(index, 1));
        }
        let model = model(nodes, Vec::new());
        let placed = layout(&model);
        let view = fit(&placed, (300.0, 200.0), Some(1));
        assert_eq!(view.zoom, MIN_FIT_ZOOM);
        let selected = screen_rect(&placed, 1, view);
        assert!(selected.y >= 0.0 && selected.y + selected.h <= 200.0);
    }

    #[test]
    fn a_fit_never_goes_below_the_readable_zoom() {
        let mut nodes = vec![node(0, 0)];
        let mut edges = Vec::new();
        for index in 1..=12 {
            nodes.push(node(index, 1));
            edges.push((0, index));
        }
        let model = model(nodes, edges);
        let placed = layout(&model);
        for pane in [
            (1400.0, 900.0),
            (900.0, 500.0),
            (500.0, 320.0),
            (240.0, 160.0),
        ] {
            let view = fit(&placed, pane, Some(3));
            assert!(
                view.zoom >= MIN_FIT_ZOOM,
                "a fit at {pane:?} shrank to {}",
                view.zoom
            );
            assert!(view.zoom <= MAX_FIT_ZOOM);
        }
    }

    #[test]
    fn a_fit_that_does_not_fit_frames_the_head_of_the_graph() {
        // Too wide and too tall for the pane: the change stays at the top-left
        // and the zoom stays legible.
        let mut nodes = vec![node(0, 0)];
        let mut edges = Vec::new();
        for index in 1..=40 {
            nodes.push(node(index, 1));
            edges.push((0, index));
        }
        let model = model(nodes, edges);
        let placed = layout(&model);
        let pane = (400.0, 300.0);
        let view = fit(&placed, pane, None);
        assert_eq!(view.zoom, MIN_FIT_ZOOM);
        let change = screen_rect(&placed, 0, view);
        assert!(change.x >= 0.0 && change.y >= 0.0);
        assert!(change.x + change.w <= pane.0 && change.y + change.h <= pane.1);
        let first = screen_rect(&placed, 1, view);
        assert!(first.x < pane.0 && first.y < pane.1);
    }

    #[test]
    fn a_selection_in_view_does_not_refit() {
        let mut nodes = vec![node(0, 0)];
        for index in 1..=40 {
            nodes.push(node(index, 1));
        }
        let model = model(nodes, Vec::new());
        let placed = layout(&model);
        let pane = (900.0, 500.0);
        let view = fit(&placed, pane, None);
        // Node 0 is in sight after any fit, so choosing it moves nothing.
        assert_eq!(after_selection(&placed, Some(0), pane, view), view);
        assert_eq!(after_selection(&placed, None, pane, view), view);
    }

    #[test]
    fn a_selection_out_of_view_is_panned_to_without_zooming() {
        let mut nodes = vec![node(0, 0)];
        for index in 1..=40 {
            nodes.push(node(index, 1));
        }
        let model = model(nodes, Vec::new());
        let placed = layout(&model);
        let pane = (900.0, 500.0);
        let view = fit(&placed, pane, None);
        let far = placed.origins.len() - 1;
        let moved = after_selection(&placed, Some(far), pane, view);
        assert_eq!(moved.zoom, view.zoom);
        let rect = screen_rect(&placed, far, moved);
        assert!(rect.x >= 0.0 && rect.y >= 0.0);
        assert!(rect.x + rect.w <= pane.0 && rect.y + rect.h <= pane.1);
    }

    #[test]
    fn a_small_graph_fits_without_growing_past_the_maximum_zoom() {
        let model = model(vec![node(0, 0), node(1, 1)], vec![(0, 1)]);
        let placed = layout(&model);
        let view = fit(&placed, (2000.0, 1400.0), None);
        assert_eq!(view.zoom, MAX_FIT_ZOOM);
        assert!(view.offset.0 > 0.0 && view.offset.1 > 0.0);
    }

    #[test]
    fn an_empty_picture_fits_without_a_division() {
        let view = fit(&Placed::default(), (800.0, 400.0), None);
        assert_eq!(view.zoom, 1.0);
    }
}
