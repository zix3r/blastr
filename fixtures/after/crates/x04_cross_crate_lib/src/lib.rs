//! Library side of the cross-crate fixture.

/// A widget with a width.
pub struct Widget {
    /// The width of the widget.
    pub width: u32,
}

/// Builds a widget of `width`.
pub fn make(width: u32) -> Widget {
    Widget { width }
}
