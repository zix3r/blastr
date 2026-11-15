//! A file whose layout changes and nothing else.

/// Adds one to a number.
pub fn inc(n: u32) -> u32 {
    n + 1
}

/// Adds two to a number.
pub fn add_two(n: u32) -> u32 {
    inc(inc(n))
}
