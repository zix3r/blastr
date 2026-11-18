//! Geometry the app is built on.

/// A point in the plane.
pub struct Point {
    /// Horizontal position.
    pub x: i64,
    /// Vertical position.
    pub y: i64,
}

/// What a drawing contains.
pub enum Shape {
    /// A single point.
    Dot,
    /// A segment of the given length.
    Line(i64),
    /// A filled disc of the given radius.
    Disc { radius: i64 },
}

/// Something with a size in cells.
pub trait Sizer {
    /// The size of this value.
    fn size(&self) -> i64;

    /// Twice the size, the way the renderer counts it.
    fn doubled(&self) -> i64 {
        self.size() * 2
    }
}

/// The origin of the coordinate system.
pub fn origin() -> Point {
    Point { x: 0, y: 0 }
}

/// The distance from the origin, counted in grid steps.
pub fn manhattan(point: &Point) -> i64 {
    point.x.abs() + point.y.abs()
}

/// Moves `point` by `dx` and `dy` and returns the result.
pub fn shifted(point: &Point, dx: i64, dy: i64) -> Point {
    Point {
        x: point.x + dx,
        y: point.y + dy,
    }
}

/// The size of a shape in cells.
pub fn shape_size(shape: &Shape) -> i64 {
    match shape {
        Shape::Dot => 1,
        Shape::Line(length) => *length,
        Shape::Disc { radius } => radius * radius,
    }
}

impl Sizer for Point {
    fn size(&self) -> i64 {
        manhattan(self)
    }
}

impl Sizer for Shape {
    fn size(&self) -> i64 {
        shape_size(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_origin_is_the_origin() {
        let point = origin();
        assert_eq!(point.x, 0);
        assert_eq!(point.y, 0);
    }

    #[test]
    fn distance_is_counted_in_grid_steps() {
        assert_eq!(manhattan(&Point { x: -2, y: 3 }), 5);
    }

    #[test]
    fn shifting_moves_both_axes() {
        let point = shifted(&origin(), 2, 3);
        assert_eq!(manhattan(&point), 5);
    }

    #[test]
    fn shapes_have_sizes() {
        assert_eq!(shape_size(&Shape::Dot), 1);
        assert_eq!(shape_size(&Shape::Line(7)), 7);
        assert_eq!(shape_size(&Shape::Disc { radius: 3 }), 9);
    }

    #[test]
    fn a_sizer_doubles() {
        assert_eq!(origin().doubled(), 0);
    }
}
