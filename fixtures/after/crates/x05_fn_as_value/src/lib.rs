//! Functions passed as values.

/// Doubles `value`.
pub fn double(value: u32) -> u32 {
    value * 2
}

/// Applies `f` to `value`.
pub fn apply(f: fn(u32) -> u32, value: u32) -> u32 {
    f(value)
}

/// Passes [`double`] to [`apply`] without calling it.
pub fn spread(value: u32) -> u32 {
    let f = double;
    apply(f, value)
}
