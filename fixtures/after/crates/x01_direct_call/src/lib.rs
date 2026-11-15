//! Direct call fixture.

/// Adds one to `value`.
pub fn callee(value: u32) -> u32 {
    value + 1
}

/// Calls [`callee`] and doubles the result.
pub fn caller(value: u32) -> u32 {
    callee(value) * 2
}
