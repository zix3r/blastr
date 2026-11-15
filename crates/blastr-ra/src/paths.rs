//! Building the canonical symbol paths of the extraction rules.
//!
//! Every path is a list of segments joined with `::`, the first segment being
//! the crate name as it appears in code. The functions here only assemble text;
//! which segments an item gets is decided by [`crate::extract`].

/// Appends one segment to a canonical path.
pub(crate) fn child(parent: &str, name: &str) -> String {
    let mut path = String::with_capacity(parent.len() + name.len() + 2);
    path.push_str(parent);
    path.push_str("::");
    path.push_str(name);
    path
}

/// The `<impl ...>` segment of an impl block's canonical path.
///
/// Both parts are the source text of the trait path and the self type with all
/// whitespace removed, so `impl Display for Wrapper<T>` becomes
/// `<impl Display for Wrapper<T>>`.
pub(crate) fn impl_segment(trait_text: Option<&str>, self_text: &str) -> String {
    match trait_text {
        Some(trait_text) => format!("<impl {trait_text} for {self_text}>"),
        None => format!("<impl {self_text}>"),
    }
}

#[cfg(test)]
mod tests {
    use super::{child, impl_segment};

    #[test]
    fn child_appends_one_segment() {
        assert_eq!(child("mycrate", "a"), "mycrate::a");
        assert_eq!(child("mycrate::a", "b"), "mycrate::a::b");
        assert_eq!(child("", "b"), "::b");
    }

    #[test]
    fn inherent_impl_segment_names_only_the_type() {
        assert_eq!(impl_segment(None, "Point"), "<impl Point>");
        assert_eq!(impl_segment(None, "Wrapper<T>"), "<impl Wrapper<T>>");
    }

    #[test]
    fn trait_impl_segment_names_trait_and_type() {
        assert_eq!(
            impl_segment(Some("Display"), "Point"),
            "<impl Display for Point>"
        );
        assert_eq!(
            impl_segment(Some("From<u8>"), "Wrapper<T>"),
            "<impl From<u8> for Wrapper<T>>"
        );
    }
}
