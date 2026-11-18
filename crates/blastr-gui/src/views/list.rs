//! The rows of a long list, as far as the window has to draw them.
//!
//! The lists of the left column have rows of different heights, so they are
//! laid out by hand: the caller keeps the heights, and this decides which
//! rows are inside the visible band and where the band starts.

use gpui_kit::prelude::*;

/// The part of a list that is on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Slice {
    /// The first row to draw.
    pub first: usize,
    /// One past the last row to draw.
    pub last: usize,
    /// The total height of all rows.
    pub total: f32,
}

/// The rows of `heights` that the band `[offset, offset + viewport)` touches,
/// with `overscan` rows drawn on either side so that a fast scroll does not
/// show a gap.
pub(crate) fn slice(heights: &[f32], offset: f32, viewport: f32, overscan: usize) -> Slice {
    let total: f32 = heights.iter().sum();
    let offset = offset.max(0.0);
    let mut top = 0.0;
    let mut first = heights.len();
    let mut last = 0;
    for (index, height) in heights.iter().enumerate() {
        let bottom = top + height;
        if bottom > offset && first == heights.len() {
            first = index;
        }
        if top < offset + viewport {
            last = index + 1;
        }
        top = bottom;
    }
    if first == heights.len() {
        first = heights.len();
    }
    Slice {
        first: first.saturating_sub(overscan),
        last: (last + overscan).min(heights.len()),
        total,
    }
}

/// The top of row `index` inside the list.
pub(crate) fn top_of(heights: &[f32], index: usize) -> f32 {
    heights.iter().take(index).sum()
}

/// The scrollable body of a list: a spacer as tall as every row, with only the
/// visible rows drawn on top of it.
///
/// `row` receives the index and the top of the row inside the list and must
/// place the row there itself (absolutely, at that top).
///
/// The caller attaches the scroll listener that redraws the list when the
/// band moves, so that the rows that come into view are built then.
pub(crate) fn scroller(
    id: impl Into<gpui_kit::ElementId>,
    handle: &gpui_kit::ScrollHandle,
    heights: &[f32],
    viewport: f32,
    overscan: usize,
    mut row: impl FnMut(usize, f32) -> gpui_kit::AnyElement,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    use gpui_kit::{AnyElement, ParentElement as _, Styled as _, div, px};

    let offset = (-f32::from(handle.offset().y)).max(0.0);
    let slice = slice(heights, offset, viewport.max(1.0), overscan);
    let children: Vec<AnyElement> = (slice.first..slice.last)
        .map(|index| row(index, top_of(heights, index)))
        .collect();

    div()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .track_scroll(handle)
        .child(
            div()
                .relative()
                .w_full()
                .h(px(slice.total.max(1.0)))
                .children(children),
        )
}

/// The offset that shows row `index` inside a band of `viewport` height,
/// moving as little as possible from `offset`.
pub(crate) fn offset_to_show(heights: &[f32], index: usize, offset: f32, viewport: f32) -> f32 {
    let total: f32 = heights.iter().sum();
    let max = (total - viewport).max(0.0);
    let top = top_of(heights, index);
    let height = heights.get(index).copied().unwrap_or(0.0);
    let mut offset = offset.clamp(0.0, max);
    if top < offset {
        offset = top;
    } else if top + height > offset + viewport {
        offset = top + height - viewport;
    }
    offset.clamp(0.0, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<f32> {
        vec![30.0, 22.0, 26.0, 34.0, 34.0, 34.0, 8.0, 30.0, 26.0, 26.0]
    }

    #[test]
    fn an_empty_list_has_an_empty_slice() {
        assert_eq!(
            slice(&[], 0.0, 100.0, 0),
            Slice {
                first: 0,
                last: 0,
                total: 0.0
            }
        );
    }

    #[test]
    fn a_short_list_is_drawn_whole() {
        let slice = slice(&rows(), 0.0, 600.0, 0);
        assert_eq!((slice.first, slice.last), (0, 10));
        assert!((slice.total - 270.0).abs() < f32::EPSILON);
    }

    #[test]
    fn only_the_rows_inside_the_band_are_drawn() {
        // Rows 0..=2 take 78 px; row 3 starts at 78.
        let slice = slice(&rows(), 78.0, 68.0, 0);
        assert_eq!((slice.first, slice.last), (3, 5));
    }

    #[test]
    fn overscan_adds_rows_on_both_sides() {
        let slice = slice(&rows(), 78.0, 68.0, 1);
        assert_eq!((slice.first, slice.last), (2, 6));
    }

    #[test]
    fn a_band_past_the_end_still_draws_the_last_row() {
        let slice = slice(&rows(), 10_000.0, 100.0, 0);
        assert_eq!((slice.first, slice.last), (10, 10));
    }

    #[test]
    fn showing_a_row_moves_the_band_as_little_as_it_can() {
        let heights = rows();
        // Row 1 is already visible: nothing moves.
        assert!((offset_to_show(&heights, 1, 0.0, 100.0) - 0.0).abs() < f32::EPSILON);
        // Row 4 starts at 112 and does not fit in [0, 100).
        let moved = offset_to_show(&heights, 4, 0.0, 100.0);
        assert!(moved > 0.0);
        assert!(moved <= 112.0);
        // Row 0 is above the band.
        assert!((offset_to_show(&heights, 0, 120.0, 100.0) - 0.0).abs() < f32::EPSILON);
        // A row below the end is clamped to the maximum.
        let max = 270.0 - 100.0;
        assert!((offset_to_show(&heights, 10_000, 0.0, 100.0) - max).abs() < f32::EPSILON);
    }
}
