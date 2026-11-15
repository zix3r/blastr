//! A chain of calls over one changed body.

/// Adds two to a number.
pub fn leaf(n: u32) -> u32 {
    n + 2
}

/// Calls the leaf function.
pub fn mid(n: u32) -> u32 {
    leaf(n) * 2
}

/// Calls the middle function.
pub fn top(n: u32) -> u32 {
    mid(n) + 1
}

/// Calls the top function.
pub fn apex(n: u32) -> u32 {
    top(n) + 1
}

/// Calls an unchanged function.
pub fn bystander(n: u32) -> u32 {
    unchanged(n) + 1
}

/// Returns its argument unchanged.
pub fn unchanged(n: u32) -> u32 {
    n
}
