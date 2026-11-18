//! A tiny front end over the geometry core.

use tinycore::{Point, Shape, Sizer, manhattan, shape_size, shifted};

/// The total footprint of every shape in a drawing.
pub fn footprint(shapes: &[Shape]) -> i64 {
    let mut total = 0;
    for shape in shapes {
        total += shape_size(shape);
    }
    total
}

/// How far the centre of the drawing is from a point.
pub fn distance_of(shapes: &[Shape], point: &Point) -> i64 {
    let centre = shifted(&Point { x: 0, y: 0 }, shapes.len() as i64, 0);
    manhattan(&centre) + manhattan(point)
}

/// The size of the largest shape, or zero for an empty drawing.
pub fn largest(shapes: &[Shape]) -> i64 {
    shapes.iter().map(Sizer::size).max().unwrap_or(0)
}

/// A one-line description of the drawing.
pub fn describe(shapes: &[Shape]) -> String {
    format!("{} shapes, {} cells", shapes.len(), footprint(shapes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_adds_the_shapes_up() {
        let shapes = [Shape::Dot, Shape::Line(4)];
        assert_eq!(footprint(&shapes), 5);
    }

    #[test]
    fn the_largest_shape_wins() {
        let shapes = [Shape::Dot, Shape::Line(9), Shape::Disc { radius: 2 }];
        assert_eq!(largest(&shapes), 9);
    }

    #[test]
    fn descriptions_mention_the_shapes() {
        assert_eq!(describe(&[Shape::Dot]), "1 shapes, 1 cells");
    }
}
