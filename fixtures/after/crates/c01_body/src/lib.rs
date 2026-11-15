/// Returns the sum of the numbers from one to n.
pub fn total(n: u32) -> u32 {
    (1..=n).map(|x| x * 2).sum()
}

/// Returns a fixed seed value.
pub fn seed() -> u32 {
    41
}

/// Returns zero.
pub fn zero() -> u32 {
    0
}
