//! A trait method whose signature changes.

/// Something with a width.
pub trait Measured {
    /// The width in whole units.
    fn width(&self) -> u64;
}

/// A bin with a stored width.
pub struct Bin {
    width: u32,
}

impl Measured for Bin {
    fn width(&self) -> u64 {
        u64::from(self.width)
    }
}

/// Reads the width through a trait object.
pub fn width_of(measured: &dyn Measured) -> u64 {
    measured.width()
}

/// Reads the width of a bin directly.
pub fn raw(bin: &Bin) -> u32 {
    bin.width
}
