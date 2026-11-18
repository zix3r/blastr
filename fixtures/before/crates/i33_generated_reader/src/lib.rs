//! A macro that writes a reader of one field.

/// A point on a grid.
pub struct Point {
    /// The horizontal position.
    pub x: u32,
    /// The vertical position.
    pub y: u32,
}

/// Declares a function that reads one named coordinate.
macro_rules! coordinate_reader {
    ($name:ident, $field:ident) => {
        /// Reads one coordinate.
        pub fn $name(point: &Point) -> u32 {
            point.$field
        }
    };
}

coordinate_reader!(vertical, y);
