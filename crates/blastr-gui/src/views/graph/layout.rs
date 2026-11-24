//! Where the boxes of the graph go: one column per layer, ordered by the
//! barycentre of their parents so that references cross as little as they can.

use std::cmp::Ordering;

use super::model::Model;

/// The size of a node.
pub(crate) const NODE_W: f32 = 200.0;
pub(crate) const NODE_H: f32 = 44.0;
/// The space between columns and between the boxes of one column.
pub(crate) const LAYER_GAP: f32 = 56.0;
pub(crate) const NODE_GAP: f32 = 12.0;
/// The space around the picture.
pub(crate) const MARGIN: f32 = 24.0;

/// The placed graph.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Placed {
    /// The top-left corner of each node.
    pub origins: Vec<(f32, f32)>,
    pub width: f32,
    pub height: f32,
}

/// Places the nodes of `model`.
pub(crate) fn layout(model: &Model) -> Placed {
    let mut order = model.layers();
    if order.is_empty() {
        return Placed::default();
    }
    let mut slot = vec![0usize; model.nodes.len()];
    for layer in &order {
        for (position, node) in layer.iter().enumerate() {
            slot[*node] = position;
        }
    }
    // Two sweeps of the barycentre heuristic: each node takes the mean slot
    // of its parents, ties broken by where it was.
    for _ in 0..2 {
        for layer in order.iter_mut().skip(1) {
            let mut keyed: Vec<(f32, usize, usize)> = layer
                .iter()
                .map(|&node| {
                    let parents: Vec<usize> = model
                        .edges
                        .iter()
                        .filter(|edge| edge.to == node)
                        .map(|edge| slot[edge.from])
                        .collect();
                    let centre = if parents.is_empty() {
                        slot[node] as f32
                    } else {
                        parents.iter().sum::<usize>() as f32 / parents.len() as f32
                    };
                    (centre, slot[node], node)
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
    // Each column is centred on the parents of its nodes, never above the top.
    let stack = |count: usize| count as f32 * (NODE_H + NODE_GAP) - NODE_GAP;
    let mut origins = vec![(0.0, 0.0); model.nodes.len()];
    let mut height: f32 = 0.0;
    for (column, layer) in order.iter().enumerate() {
        let parents: Vec<f32> = layer
            .iter()
            .flat_map(|node| model.edges.iter().filter(move |edge| edge.to == *node))
            .map(|edge| origins[edge.from].1 + NODE_H / 2.0)
            .collect();
        let top = if column == 0 || parents.is_empty() {
            MARGIN
        } else {
            let centre = parents.iter().sum::<f32>() / parents.len() as f32;
            (centre - stack(layer.len()) / 2.0).max(MARGIN)
        };
        let x = MARGIN + column as f32 * (NODE_W + LAYER_GAP);
        for (position, node) in layer.iter().enumerate() {
            origins[*node] = (x, top + position as f32 * (NODE_H + NODE_GAP));
        }
        height = height.max(top + stack(layer.len()));
    }
    Placed {
        origins,
        width: MARGIN * 2.0 + order.len() as f32 * (NODE_W + LAYER_GAP) - LAYER_GAP,
        height: height + MARGIN,
    }
}

/// The zoom and offset that fit `placed` into a pane of `pane` pixels: the
/// whole picture when it is legible that small, else its start.
pub(crate) fn fit(placed: &Placed, pane: (f32, f32)) -> (f32, (f32, f32)) {
    if placed.origins.is_empty() || pane.0 < 1.0 || pane.1 < 1.0 {
        return (1.0, (0.0, 0.0));
    }
    let zoom = (pane.0 / placed.width)
        .min(pane.1 / placed.height)
        .clamp(0.85, 1.1);
    let x = ((pane.0 - placed.width * zoom) / 2.0).max(0.0);
    let y = ((pane.1 - placed.height * zoom) / 2.0).max(0.0);
    (zoom, (x, y))
}

#[cfg(test)]
mod tests {
    use super::super::model::{Edge, Model, Node, NodeKind};
    use super::*;

    fn model(layers: &[usize], edges: &[(usize, usize)]) -> Model {
        Model {
            nodes: layers
                .iter()
                .map(|layer| Node {
                    symbol: None,
                    name: String::new(),
                    subtitle: String::new(),
                    kind: NodeKind::Step,
                    layer: *layer,
                    on_path: false,
                })
                .collect(),
            edges: edges
                .iter()
                .map(|(from, to)| Edge {
                    from: *from,
                    to: *to,
                    candidate: false,
                    highlight: false,
                })
                .collect(),
            reached: 0,
        }
    }

    #[test]
    fn an_empty_graph_has_no_size() {
        assert_eq!(layout(&Model::default()), Placed::default());
    }

    #[test]
    fn columns_follow_the_layers_and_boxes_never_overlap() {
        let model = model(&[0, 1, 1, 1, 2], &[(0, 1), (0, 2), (0, 3), (1, 4), (2, 4)]);
        let placed = layout(&model);
        for edge in &model.edges {
            assert!(placed.origins[edge.from].0 < placed.origins[edge.to].0);
        }
        for left in 0..model.nodes.len() {
            for right in left + 1..model.nodes.len() {
                let (a, b) = (placed.origins[left], placed.origins[right]);
                let apart = (a.0 - b.0).abs() >= NODE_W || (a.1 - b.1).abs() >= NODE_H;
                assert!(apart, "{left} and {right} overlap");
            }
        }
        assert!(placed.height >= NODE_H * 3.0);
        assert_eq!(layout(&model), placed, "the layout is deterministic");
    }

    #[test]
    fn children_follow_the_order_of_their_parents() {
        // 1 and 2 are parents of 4 and 3 respectively: 4 goes above 3.
        let model = model(&[0, 1, 1, 2, 2], &[(0, 1), (0, 2), (2, 3), (1, 4)]);
        let placed = layout(&model);
        assert!(placed.origins[4].1 < placed.origins[3].1);
    }

    #[test]
    fn a_small_picture_is_centred_and_a_large_one_starts_at_the_top() {
        let small = layout(&model(&[0, 1], &[(0, 1)]));
        let (zoom, (x, y)) = fit(&small, (1200.0, 600.0));
        assert!(zoom <= 1.1 && x > 0.0 && y > 0.0);
        let tall: Vec<usize> = std::iter::once(0)
            .chain(std::iter::repeat_n(1, 40))
            .collect();
        let edges: Vec<(usize, usize)> = (1..41).map(|to| (0, to)).collect();
        let large = layout(&model(&tall, &edges));
        let (zoom, (_, y)) = fit(&large, (800.0, 400.0));
        assert_eq!(zoom, 0.85);
        assert_eq!(y, 0.0);
    }
}
