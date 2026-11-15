//! An attribute added to a function.

/// Adds one to a number.
#[inline]
pub fn inc(n: u32) -> u32 {
    n + 1
}

/// Adds two to a number.
pub fn add_two(n: u32) -> u32 {
    inc(inc(n))
}

/// Adds three to a number.
pub fn add_three(n: u32) -> u32 {
    inc(add_two(n))
}

/// Returns its argument unchanged.
pub fn unchanged(n: u32) -> u32 {
    n
}

/// Calls the unchanged function.
pub fn keep(n: u32) -> u32 {
    unchanged(n)
}
