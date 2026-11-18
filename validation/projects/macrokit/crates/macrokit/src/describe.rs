//! The reflection trait implemented by `#[derive(Describe)]`.
//!
//! `Describe` is intentionally tiny so the derive macro stays small: a type
//! reports its display name, the names of the fields it wants to expose and a
//! `Debug` rendering of each of them. The configuration layer uses it for
//! command output and for diagnostics.

use std::fmt::{self, Display, Formatter};

/// Static list of exposed field names.
pub type Fields = &'static [&'static str];

/// The result of [`Describe::describe`]: ordered `(field, rendered)` pairs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Description {
    names: Fields,
    entries: Vec<(String, String)>,
}

impl Description {
    /// Starts a description that exposes `names`.
    pub fn new(names: Fields) -> Self {
        Self {
            names,
            entries: Vec::with_capacity(names.len()),
        }
    }

    /// Records the rendering of one field.
    pub fn push(&mut self, name: &'static str, rendered: String) {
        self.entries.push((name.to_string(), rendered));
    }

    /// The exposed field names.
    pub fn fields(&self) -> Fields {
        self.names
    }

    /// The rendered fields, in declaration order.
    pub fn entries(&self) -> &[(String, String)] {
        &self.entries
    }

    /// The rendering of one field, if it is exposed.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, rendered)| rendered.as_str())
    }

    /// Number of exposed fields.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the type exposes no fields.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `field = rendered` lines, one per entry.
    pub fn to_lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|(field, rendered)| format!("{field} = {rendered}"))
            .collect()
    }
}

impl Display for Description {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        for (index, (field, rendered)) in self.entries.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{field}={rendered}")?;
        }
        Ok(())
    }
}

/// Types that can describe their own fields.
///
/// Implemented by `#[derive(Describe)]`; the constants are generated, the
/// methods have working defaults so handwritten implementations only need the
/// two constants.
pub trait Describe {
    /// Display name of the type.
    const NAME: &'static str;
    /// Names of the exposed fields.
    const FIELDS: Fields;

    /// Renders every exposed field with `Debug`.
    fn describe(&self) -> Description;

    /// The display name of the type.
    fn name(&self) -> &'static str {
        Self::NAME
    }

    /// The exposed field names.
    fn field_names(&self) -> Fields {
        Self::FIELDS
    }

    /// Whether `field` is exposed by this type.
    fn has_field(&self, field: &str) -> bool {
        Self::FIELDS.contains(&field)
    }
}

#[cfg(test)]
mod tests {
    use super::Description;

    const NAMES: &[&str] = &["gain", "label"];

    #[test]
    fn description_keeps_insertion_order() {
        let mut description = Description::new(NAMES);
        description.push("gain", "1.25".to_string());
        description.push("label", "\"primary\"".to_string());
        assert_eq!(description.len(), 2);
        assert!(!description.is_empty());
        assert_eq!(description.fields(), NAMES);
        assert_eq!(description.get("gain"), Some("1.25"));
        assert_eq!(description.get("missing"), None);
        assert_eq!(
            description.to_lines(),
            vec!["gain = 1.25".to_string(), "label = \"primary\"".to_string()]
        );
        assert_eq!(description.to_string(), "gain=1.25, label=\"primary\"");
    }

    #[test]
    fn empty_description_is_legal() {
        let description = Description::new(&[]);
        assert!(description.is_empty());
        assert_eq!(description.to_string(), "");
        assert_eq!(description.to_lines(), Vec::<String>::new());
    }

    #[test]
    fn description_preserves_the_declared_names() {
        let description = Description::new(NAMES);
        assert_eq!(description.fields().len(), 2);
    }
}
