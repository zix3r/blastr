//! Unit tests around one changed function.

/// Adds one to a number.
pub fn inc(n: u32) -> u32 {
    n + 1
}

#[cfg(test)]
mod tests {
    use super::inc;

    /// Reaches the changed function.
    #[test]
    fn a() {
        assert_eq!(inc(1), 2);
    }

    /// Reaches the changed function twice.
    #[test]
    fn b() {
        assert_eq!(inc(inc(1)), 3);
    }

    /// Touches nothing that changed.
    #[test]
    fn c() {
        assert_eq!(2 + 2, 4);
    }
}
