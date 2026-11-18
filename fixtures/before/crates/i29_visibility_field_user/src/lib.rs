//! Consumer side of the field-visibility fixture.

use i29_visibility_field::Spec;

/// Builds a specification with a literal that names every field.
pub fn build(name: &str) -> Spec {
    Spec {
        name: name.to_owned(),
        argument: Some("error".to_owned()),
    }
}

/// Builds a specification from another one, leaving the argument to it.
pub fn rebuild(base: &Spec) -> Spec {
    Spec {
        name: base.name.to_uppercase(),
        ..base.clone()
    }
}

/// Reads the argument from outside the library.
pub fn argument_of(spec: &Spec) -> bool {
    spec.argument.is_some()
}

/// Reads the name, which the change does not touch.
pub fn name_of(spec: &Spec) -> &str {
    spec.name.as_str()
}

/// Matches the structure with a pattern that names the argument.
pub fn has_argument(spec: &Spec) -> bool {
    matches!(
        spec,
        Spec {
            argument: Some(_),
            ..
        }
    )
}
