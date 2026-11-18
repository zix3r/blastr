//! Library side of the field-visibility fixture.

/// A specification with one public and one crate-visible field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    /// The name, which stays public.
    pub name: String,
    /// The argument, whose visibility the case narrows.
    pub(crate) argument: Option<String>,
}

impl Spec {
    /// Builds a specification inside its own crate.
    pub fn parse(name: &str, argument: Option<&str>) -> Spec {
        Spec {
            name: name.to_owned(),
            argument: argument.map(str::to_owned),
        }
    }

    /// The argument, or an empty string.
    pub fn require_argument(&self) -> &str {
        self.argument.as_deref().unwrap_or_default()
    }
}
