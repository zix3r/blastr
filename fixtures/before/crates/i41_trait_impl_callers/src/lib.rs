//! A trait item whose signature changes beneath two call sites.
//!
//! One call names the trait object and is reached from the changed trait item
//! directly. The other names the concrete type, so it resolves to the method
//! that implements the trait item: the implementation has to be edited to match
//! the new signature, and the call that reads its result has to adapt too.

/// Renders itself on one line.
pub trait Rendered {
    /// The rendering.
    fn render(&self) -> String;
}

/// A value with a fixed rendering.
pub struct Card;

impl Rendered for Card {
    fn render(&self) -> String {
        String::from("card")
    }
}

/// Reads the rendering of a card, through the concrete type.
pub fn card_text() -> String {
    Card.render()
}

/// Reads the rendering of anything that renders itself.
pub fn dyn_text(rendered: &dyn Rendered) -> String {
    rendered.render()
}
