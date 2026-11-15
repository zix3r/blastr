/// Multiplies the value it is given.
macro_rules! double {
    ($x:expr) => {
        $x * 2
    };
}

/// Doubles a number with the macro.
pub fn twice(n: i32) -> i32 {
    double!(n)
}
