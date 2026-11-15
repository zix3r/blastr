//! Struct and variant construction.

/// A point on a grid.
pub struct Point {
    /// The x coordinate.
    pub x: u32,
    /// The y coordinate.
    pub y: u32,
}

/// A direction of travel.
pub enum Direction {
    /// Towards larger y.
    Up,
    /// Towards smaller y.
    Down,
}

/// Builds a point and a direction.
pub fn build() -> (Point, Direction) {
    let point = Point { x: 1, y: 2 };
    let direction = Direction::Up;
    (point, direction)
}
