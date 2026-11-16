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

    /// Touches nothing either.
    #[test]
    fn d() {
        assert_eq!(3 + 3, 6);
    }

    /// Touches nothing either.
    #[test]
    fn e() {
        assert_eq!(4 + 4, 8);
    }

    /// Touches nothing either.
    #[test]
    fn f() {
        assert_eq!(5 + 5, 10);
    }
}
