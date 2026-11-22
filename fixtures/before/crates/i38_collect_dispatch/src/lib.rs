//! `collect` builds a collection through `FromIterator`, which nothing names.
//!
//! The `collect` of a `Vec` has no implementation of this workspace to reach:
//! only the type asked for decides whether one runs.

/// A list of report lines.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Lines {
    lines: Vec<String>,
}

impl Lines {
    /// An empty list.
    pub fn new() -> Lines {
        Lines { lines: Vec::new() }
    }

    /// The number of lines.
    pub fn count(&self) -> usize {
        self.lines.len()
    }

    /// The lines, joined by a newline.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

impl FromIterator<String> for Lines {
    fn from_iter<I: IntoIterator<Item = String>>(lines: I) -> Lines {
        let mut out = Lines::new();
        out.lines.extend(lines);
        out
    }
}

/// Builds the list with `collect`.
pub fn collected(texts: &[&str]) -> Lines {
    texts.iter().map(|text| text.to_string()).collect()
}

/// Builds a `Vec` with `collect`, which no impl of this workspace provides.
pub fn copied(texts: &[&str]) -> Vec<String> {
    texts.iter().map(|text| text.to_string()).collect()
}

/// Counts the lines `collect` produced, so the propagation has a second step.
pub fn line_count(texts: &[&str]) -> usize {
    collected(texts).count()
}
