/// A shape.
pub enum Shape {
    /// A circle with a radius.
    Circle(f64),
    /// A square with a side.
    Square(f64),
}

/// Returns the radius of a circle, or zero for other shapes.
pub fn radius(shape: &Shape) -> f64 {
    match shape {
        Shape::Circle(r) => *r,
        Shape::Square(_) => 0.0,
    }
}

/// Builds a circle.
pub fn circle(r: f64) -> Shape {
    Shape::Circle(r)
}
