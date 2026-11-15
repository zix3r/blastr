/// Adds one to a number.
pub fn inc(n: i32) -> i32 {
    n + 1
}

/// Adds two to a number.
pub fn add_two(n: i32) -> i32 {
    inc(inc(n))
}

/// Adds three to a number.
pub fn add_three(n: i32) -> i32 {
    inc(add_two(n))
}
