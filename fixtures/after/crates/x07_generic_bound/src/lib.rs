//! Dispatch through a generic bound.

/// A shape with an area.
pub trait Shape {
    /// The area of the shape.
    fn area(&self) -> u32;
}

/// A circle.
pub struct Circle {
    radius: u32,
}

/// A square.
pub struct Square {
    side: u32,
}

impl Shape for Circle {
    fn area(&self) -> u32 {
        self.radius * self.radius * 3
    }
}

impl Shape for Square {
    fn area(&self) -> u32 {
        self.side * self.side
    }
}

/// Calls the trait method through a generic parameter.
pub fn total<T: Shape>(shape: &T) -> u32 {
    shape.area()
}
