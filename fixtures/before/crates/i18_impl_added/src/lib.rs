//! A hand-written trait implementation added to a type.

/// Something with a value.
pub trait Valued {
    /// The value in units.
    fn value(&self) -> u32;
}

/// A coin worth one.
pub struct Coin;

impl Valued for Coin {
    fn value(&self) -> u32 {
        1
    }
}

/// A note worth ten.
pub struct Note;

/// Sums whatever a holder has.
pub fn total(held: &dyn Valued) -> u32 {
    held.value()
}

/// Reads the value of a coin directly.
pub fn coin_value() -> u32 {
    Coin.value()
}
