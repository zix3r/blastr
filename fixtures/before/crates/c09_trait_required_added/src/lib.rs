/// A shape that can measure itself.
pub trait Shape {
    /// The area of the shape.
    fn area(&self) -> f64;
}

/// A square.
pub struct Square {
    /// The side length.
    pub side: f64,
}

impl Shape for Square {
    fn area(&self) -> f64 {
        self.side * self.side
    }
}

/// Returns the area of any shape.
pub fn area_of(shape: &impl Shape) -> f64 {
    shape.area()
}
