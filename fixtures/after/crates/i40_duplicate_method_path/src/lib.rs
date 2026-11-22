//! An inherent method written once per instantiation of a generic type.
//!
//! The canonical path of a method leaves the impl's own self type out, so
//! `impl Range<f64>` and `impl Range<i64>` both declare
//! `i40_duplicate_method_path::Range::bounds_text`. Two symbols therefore share
//! one path, and a comparison has to tell them apart by their text.
//!
//! The float method is gone — the edit this fixture mirrors removes one
//! definition — while the impl block it lived in stays, and the integer method
//! is untouched.

/// An inclusive range of comparable values.
pub struct Range<T> {
    min: T,
    max: T,
}

impl<T: Copy> Range<T> {
    /// Builds a range from its bounds.
    pub const fn new(min: T, max: T) -> Self {
        Self { min, max }
    }
}

impl Range<f64> {}

impl Range<i64> {
    /// Renders the bounds in full.
    pub fn bounds_text(&self) -> String {
        format!("{}..{}", self.min, self.max)
    }
}

/// Prints the bounds of a float range.
pub fn describe_float() -> String {
    format!("{:.1}..{:.1}", 0.5, 1.5)
}

/// Prints the bounds of an integer range.
pub fn describe_int() -> String {
    Range::new(1, 64).bounds_text()
}
