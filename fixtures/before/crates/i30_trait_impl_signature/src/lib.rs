//! An implementation's method changes while the trait it implements does not.

/// Something with a name.
pub trait Named {
    /// The name.
    fn name(&self) -> &str;
}

/// A tag with a stored label.
pub struct Tag {
    label: String,
}

impl Named for Tag {
    fn name(&self) -> &str {
        self.label.as_str()
    }
}

/// Names a tag by calling the trait method on the concrete type.
pub fn label(tag: &Tag) -> String {
    tag.name().to_owned()
}
