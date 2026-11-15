//! A macro whose definition changes.

/// Doubles the expression it is given.
macro_rules! twice {
    ($value:expr) => {
        $value * 2
    };
}

/// Adds one to the expression it is given.
macro_rules! plus_one {
    ($value:expr) => {
        $value + 1
    };
}

/// Doubles a number through the macro.
pub fn doubled(n: u32) -> u32 {
    twice!(n)
}

/// Doubles twice through the function.
pub fn quadrupled(n: u32) -> u32 {
    doubled(doubled(n))
}

/// Adds one through the other macro.
pub fn incremented(n: u32) -> u32 {
    plus_one!(n)
}
