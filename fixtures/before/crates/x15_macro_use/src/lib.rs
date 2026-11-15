//! Invocations of macros the crate defines.

/// Doubles its argument.
macro_rules! twice {
    ($value:expr) => {
        $value * 2
    };
}

/// Invokes `twice!` from its own expansion.
macro_rules! four_times {
    ($value:expr) => {
        twice!(twice!($value))
    };
}

/// Adds one to `value`.
pub fn increment(value: u32) -> u32 {
    value + 1
}

/// Invokes a macro of this crate.
pub fn doubled(value: u32) -> u32 {
    twice!(value)
}

/// Invokes a macro whose expansion invokes another one.
pub fn quadrupled(value: u32) -> u32 {
    four_times!(value)
}

/// Calls a function in the argument of a macro.
pub fn doubled_increment(value: u32) -> u32 {
    twice!(increment(value))
}

/// Uses only macros of the standard library.
pub fn counted(value: u32) -> usize {
    vec![value, value].len()
}
