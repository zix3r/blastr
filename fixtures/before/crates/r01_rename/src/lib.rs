/// Doubles a number.
pub fn compute(n: u32) -> u32 {
    n * 2
}

/// Calls the doubling function.
pub fn caller() -> u32 {
    compute(21)
}

/// Returns a fixed value.
pub fn untouched() -> u32 {
    7
}
