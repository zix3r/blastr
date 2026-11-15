//! Derived traits and a hand-written implementation.

/// A point on a grid.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    /// The x coordinate.
    pub x: u32,
    /// The y coordinate.
    pub y: u32,
}

impl Default for Point {
    fn default() -> Self {
        Point { x: 0, y: 0 }
    }
}
