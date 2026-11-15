/// Adds one to a number.
pub(crate) fn inc(n: i32) -> i32 {
    n + 1
}

/// Calls the increment helper.
pub fn apply(n: i32) -> i32 {
    inc(n)
}

/// Returns zero.
pub fn reset() -> i32 {
    0
}
