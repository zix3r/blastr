//! Consumer side of the cross-crate fixture.

use x04_cross_crate_lib::{Widget, make};

/// Holds a widget.
pub struct Holder {
    /// The held widget.
    pub widget: Widget,
}

/// Calls the library function and wraps its result.
pub fn build() -> Holder {
    Holder { widget: make(3) }
}

/// Names the library type in its signature.
pub fn direct() -> Widget {
    make(7)
}
