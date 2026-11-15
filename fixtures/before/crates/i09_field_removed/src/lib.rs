//! A struct that loses a field.

/// A point on a grid.
pub struct Point {
    /// The horizontal position.
    pub x: u32,
    /// The vertical position.
    pub y: u32,
}

/// Reads the horizontal position.
pub fn horizontal(point: &Point) -> u32 {
    point.x
}

/// Reads the vertical position.
pub fn vertical(point: &Point) -> u32 {
    point.y
}

/// Builds a point at the origin.
pub fn origin() -> Point {
    Point { x: 0, y: 0 }
}
