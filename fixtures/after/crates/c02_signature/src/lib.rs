/// Scales a value up by ten.
pub fn scale(value: i32, _factor: i32) -> i32 {
    value * 10
}

/// Applies the scaling and adds one.
pub fn apply(value: i32) -> i32 {
    scale(value, 1) + 1
}

/// Returns its argument unchanged.
pub fn identity(value: i32) -> i32 {
    value
}
