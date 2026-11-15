/// A named value.
#[derive(Debug, Clone)]
pub struct Label {
    /// The text of the label.
    pub text: &'static str,
}

impl Label {
    /// Builds a label.
    pub fn new(text: &'static str) -> Label {
        Label { text }
    }
}
