//! A function passed around as a value.

/// Applies a function to a number.
pub fn apply(f: fn(u32) -> u32, n: u32) -> u32 {
    f(n)
}

/// Multiplies a number by three.
pub fn scale(n: u32) -> u32 {
    n * 3
}

/// Passes the scaling function as an argument.
pub fn via_argument(n: u32) -> u32 {
    apply(scale, n)
}

/// Binds the scaling function to a local name.
pub fn via_binding(n: u32) -> u32 {
    let g = scale;
    g(n) + 1
}

/// Returns its argument unchanged.
pub fn other(n: u32) -> u32 {
    n
}

/// Passes an unchanged function as an argument.
pub fn via_other(n: u32) -> u32 {
    apply(other, n)
}
