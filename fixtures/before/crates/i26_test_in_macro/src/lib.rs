//! A function reached only from a test through a macro.

/// Halves a price.
pub fn discounted(price: u32) -> u32 {
    price / 2
}

#[cfg(test)]
mod tests {
    use super::discounted;

    /// Halves a price in an assertion.
    #[test]
    fn halves_a_price() {
        assert_eq!(discounted(10), 5);
    }
}
