/// A helper module.
pub mod a {}

/// Another helper module.
pub mod b {
    /// Adds one to a number.
    pub fn helper(n: u32) -> u32 {
        n + 1
    }
}

/// Calls the helper.
pub fn caller() -> u32 {
    b::helper(41)
}

/// Returns a fixed value.
pub fn base() -> u32 {
    1
}
