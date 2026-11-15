//! Calls that only appear inside macro expansions.

/// Doubles `value`.
pub fn doubled(value: u32) -> u32 {
    value * 2
}

/// Builds text for `value`.
pub fn describe(value: u32) -> Vec<String> {
    let text = format!("{}", doubled(value));
    vec![text, doubled(value).to_string()]
}

#[cfg(test)]
mod tests {
    use super::doubled;

    #[test]
    fn doubled_is_twice() {
        assert_eq!(doubled(2), 4);
    }
}
