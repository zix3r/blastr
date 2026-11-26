//! Consumer side of the attribute-macro fixture: callers whose whole item an
//! attribute macro re-emits, as `#[tokio::main]` does.

use i43_attribute_wrapped::wrapped;

/// Gains a parameter.
pub fn reshaped(value: u8) -> u8 {
    value
}

/// Is removed.
pub fn dropped() -> u8 {
    1
}

/// Returns something else.
pub fn reworked() -> u8 {
    1
}

// No doc comment, exactly like a `#[tokio::main] async fn main`: the macro's
// input then starts at `pub`, after the attribute.
#[wrapped]
pub async fn calls_reshaped() -> u8 {
    reshaped(1)
}

#[wrapped]
/// A doc comment after the attribute is part of the macro's input.
pub async fn calls_reshaped_documented() -> u8 {
    reshaped(2)
}

#[wrapped]
pub async fn calls_dropped() -> u8 {
    dropped()
}

#[wrapped]
/// A doc comment after the attribute is part of the macro's input.
pub async fn calls_reworked() -> u8 {
    reworked()
}
