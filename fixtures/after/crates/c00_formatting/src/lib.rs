//! A tiny arithmetic crate.
//!
//! Used to test formatting-only changes.

/// Adds two numbers and returns their sum.
pub fn add(a: i32, b: i32) -> i32 {
    a+b
}


/// Doubles a number.
pub fn double(n: i32) -> i32 {
    n*2
}

/// The step used when bumping.
pub const STEP: i32 = 3;

/// Bumps a number by the step.
pub fn bump(n: i32) -> i32
{
    n + STEP
}
