//! Match patterns over an enum.

/// A colour.
pub enum Colour {
    /// Red.
    Red,
    /// Green.
    Green,
    /// Blue.
    Blue,
}

/// Names every variant.
pub fn all(colour: &Colour) -> u8 {
    match colour {
        Colour::Red => 1,
        Colour::Green => 2,
        Colour::Blue => 3,
    }
}

/// Names one variant and catches the rest.
pub fn partial(colour: &Colour) -> u8 {
    match colour {
        Colour::Red => 1,
        _ => 0,
    }
}
