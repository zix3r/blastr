//! Struct literals and patterns that leave fields out.

/// A labelled point.
pub struct Point {
    /// The x coordinate.
    pub x: u32,
    /// The y coordinate.
    pub y: u32,
    /// The label.
    pub label: &'static str,
}

/// A pair of numbers.
pub struct Pair(pub u32, pub u32);

/// Names every field of a point.
pub fn full(x: u32, y: u32, label: &'static str) -> Point {
    Point { x, y, label }
}

/// Takes the other fields from another point.
pub fn shifted(base: Point) -> Point {
    Point { x: base.x + 1, ..base }
}

/// Matches every field of a point.
pub fn exact(point: Point) -> u32 {
    let Point { x, y, label: _ } = point;
    x + y
}

/// Matches some fields of a point.
pub fn partial(point: Point) -> u32 {
    let Point { x, .. } = point;
    x
}

/// Matches a pair without every field.
pub fn first(pair: Pair) -> u32 {
    let Pair(first, ..) = pair;
    first
}

/// What a worker is doing.
pub enum State {
    /// Nothing to do.
    Idle,
    /// At work.
    Busy {
        /// How far along the work is.
        progress: u8,
    },
}

/// Matches a busy state without every field.
pub fn is_busy(state: &State) -> bool {
    match state {
        State::Busy { .. } => true,
        _ => false,
    }
}

/// Names every variant of a state.
pub fn code(state: State) -> u8 {
    match state {
        State::Idle => 0,
        State::Busy { progress } => progress,
    }
}
