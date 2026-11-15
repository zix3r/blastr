//! A type alias whose target changes.

/// An identifier.
pub type Id = u32;

/// Shows an identifier between braces.
pub fn show(id: Id) -> String {
    format!("{{{id}}}")
}

/// Returns the next plain number.
pub fn plain(n: u32) -> u32 {
    n + 1
}
