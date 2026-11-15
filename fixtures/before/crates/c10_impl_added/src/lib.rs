/// A point on a grid.
pub struct Point {
    /// The x coordinate.
    pub x: i32,
    /// The y coordinate.
    pub y: i32,
}

impl Point {
    /// The origin.
    pub fn origin() -> Point {
        Point { x: 0, y: 0 }
    }
}
