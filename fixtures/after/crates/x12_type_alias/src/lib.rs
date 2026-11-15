//! A type alias.

/// A count per name.
pub type Counts = Vec<(String, u32)>;

/// Counts the entries.
pub fn len(counts: &Counts) -> usize {
    counts.len()
}
