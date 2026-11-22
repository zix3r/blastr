//! A trait item whose signature changes beneath two call sites.
//!
//! Only the trait declaration is edited: this is the revision right after the
//! seeded change, and the implementation that no longer matches it is what the
//! impact analysis has to name. The call that names the concrete type reads the
//! implementation's result, so it has to adapt as well.

/// Renders itself on one line.
pub trait Rendered {
    /// The rendering.
    fn render(&self) -> (String,);
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
