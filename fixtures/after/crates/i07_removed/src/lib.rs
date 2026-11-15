//! A function removed from under one caller.

/// Computes a total.
pub fn total(n: u32) -> u32 {
    n * 2 + 1
}

/// Adds one to its argument.
pub fn other(n: u32) -> u32 {
    n + 1
}
