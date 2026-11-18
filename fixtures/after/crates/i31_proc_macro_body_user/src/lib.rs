//! Consumer side of the procedural-macro fixture.

use i31_proc_macro_body::Greet;

/// The trait the derive implements.
pub trait Greet {
    fn greet(&self) -> u8;
}

/// A struct whose implementation the derive generates.
#[derive(Greet)]
pub struct Point {
    /// A field the derive's tokens pass over.
    pub x: u8,
}

/// Calls the method the derive generated.
pub fn greet_point(point: &Point) -> u8 {
    point.greet()
}

/// Calls the generated method through another function.
pub fn greet_twice(point: &Point) -> u8 {
    greet_point(point) + greet_point(point)
}

#[cfg(test)]
mod tests {
    use super::{Point, greet_point};

    #[test]
    fn a_derived_method_greets() {
        let point = Point { x: 1 };
        assert!(greet_point(&point) > 0);
    }
}
