/// A shape.
pub enum Shape {
    /// A circle with a radius.
    Circle(f64),
    /// A square with a side.
    Square(f64),
}

/// Describes the shape.
pub fn describe(shape: &Shape) -> &'static str {
    match shape {
        Shape::Circle(_) => "circle",
        Shape::Square(_) => "square",
    }
}

/// Reports whether the shape is round.
pub fn is_round(shape: &Shape) -> bool {
    match shape {
        Shape::Circle(_) => true,
        _ => false,
    }
}
