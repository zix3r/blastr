//! Visibility narrowed where nobody outside notices.

mod inner {
    /// Used only by the item next to it.
    fn helper(n: u32) -> u32 {
        n + 1
    }

    /// Calls the helper in its own module.
    pub fn local(n: u32) -> u32 {
        helper(n) * 2
    }
}

/// Doubles a number.
pub(crate) fn shared(n: u32) -> u32 {
    n * 2
}

/// Calls the shared function from the same crate.
pub fn wrapper(n: u32) -> u32 {
    shared(n) + 1
}

/// Calls into the module.
pub fn entry(n: u32) -> u32 {
    inner::local(n) + 1
}
