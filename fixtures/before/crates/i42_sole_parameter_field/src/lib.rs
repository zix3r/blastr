//! A struct that loses the field its lifetime parameter was written for, and
//! a generic struct that loses a field another field also uses.

/// A borrowed view of one source text.
pub struct Source<'a> {
    /// The text the view was made from.
    text: &'a str,
    /// How many lines the view carries.
    count: usize,
}

impl<'a> Source<'a> {
    /// A view of `text` that carries `count` lines.
    pub fn new(text: &'a str, count: usize) -> Self {
        Self { text, count }
    }
}

/// How many lines the view carries.
pub fn count_of(source: &Source<'_>) -> usize {
    source.count
}

/// Two values of one type.
pub struct Pair<T> {
    /// The first value.
    pub left: T,
    /// The second value.
    pub right: T,
}

impl<T> Pair<T> {
    /// Pairs two values.
    pub fn new(left: T, right: T) -> Self {
        Self { left, right }
    }
}

/// The second value of a pair.
pub fn right_of<T>(pair: &Pair<T>) -> &T {
    &pair.right
}
