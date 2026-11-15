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

/// A greeter that overrides the default greeting.
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
pub fn greet(g: &impl Greeter) -> String {
    g.greet()
}
