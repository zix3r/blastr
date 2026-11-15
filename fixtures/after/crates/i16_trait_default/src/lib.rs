//! A changed default method body.

/// A greeting behaviour.
pub trait Greeter {
    /// The name to greet with.
    fn name(&self) -> &'static str;

    /// Greets politely.
    fn greet(&self) -> String {
        format!("hi {}", self.name())
    }
}

/// A greeter that uses the default greeting.
pub struct Quiet;

impl Greeter for Quiet {
    fn name(&self) -> &'static str {
        "quiet"
    }
}

/// A greeter that overrides the greeting.
pub struct Loud;

impl Greeter for Loud {
    fn name(&self) -> &'static str {
        "loud"
    }

    fn greet(&self) -> String {
        format!("HELLO {}", self.name())
    }
}

/// Greets through any greeter.
pub fn greet(greeter: &dyn Greeter) -> String {
    greeter.greet()
}
