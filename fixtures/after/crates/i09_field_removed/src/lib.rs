//! A struct that loses a field.

/// A point on a grid.
pub struct Point {
    /// The horizontal position.
    pub x: u32,
}

/// Reads the horizontal position.
pub fn horizontal(point: &Point) -> u32 {
    point.x
}

/// Reads the vertical position, which the point no longer carries.
pub fn vertical(point: &Point) -> u32 {
    let _ = point;
    0
}

/// Builds a point at the origin.
pub fn origin() -> Point {
    Point { x: 0 }
}
