//! A rename detected by an unchanged body.

/// Rounds a number up by one.
pub fn ceil(n: u32) -> u32 {
    n + 1
}

/// Uses the rounding function.
pub fn apply(n: u32) -> u32 {
    ceil(n) * 2
}

/// Does not use the rounding function.
pub fn plain(n: u32) -> u32 {
    n * 3
}
