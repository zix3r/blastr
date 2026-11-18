//! Rendering helpers shared by reports and diagnostics.

use std::fmt;

/// A value that can describe itself on one or more lines.
///
/// The trait is blanket-implemented for everything that implements
/// [`std::fmt::Display`], so ordinary values (records, stats, strings) get the
/// helpers for free while types with richer renderings can still rely on them.
pub trait Describe {
    /// The full rendering.
    fn describe(&self) -> String;

    /// The first line of [`Describe::describe`].
    fn headline(&self) -> String {
        let text = self.describe();
        match text.find('\n') {
            Some(end) => text[..end].to_string(),
            None => text,
        }
    }

    /// Whether the rendering contains `needle`.
    fn mentions(&self, needle: &str) -> bool {
        self.describe().contains(needle)
    }
}

impl<T: fmt::Display> Describe for T {
    fn describe(&self) -> String {
        format!("{self}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Card {
        title: &'static str,
        body: &'static str,
    }

    impl fmt::Display for Card {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}\n{}", self.title, self.body)
        }
    }

    #[test]
    fn headline_takes_the_first_line_only() {
        let card = Card {
            title: "run summary",
            body: "3 stages, 4 records",
        };
        assert_eq!(card.headline(), "run summary");
        assert_eq!(card.describe(), "run summary\n3 stages, 4 records");
        assert!(card.mentions("3 stages"));
        assert!(!card.mentions("9 stages"));
    }

    #[test]
    fn single_line_values_describe_themselves() {
        assert_eq!(42usize.headline(), "42");
        assert_eq!("ready".describe(), "ready");
        assert_eq!(String::from("a\nb").headline(), "a");
    }
}
