//! A call through a trait whose receiver has a known type.

/// Greets by name.
pub trait Greeter {
    /// The name to greet.
    fn name(&self) -> String;

    /// The greeting; only `Loud` writes its own.
    fn greet(&self) -> String {
        format!("hello {}", self.name())
    }
}

/// Keeps the default greeting.
pub struct Plain;

/// Writes its own greeting.
pub struct Loud;

impl Greeter for Plain {
    fn name(&self) -> String {
        String::from("plain")
    }
}

impl Greeter for Loud {
    fn name(&self) -> String {
        String::from("loud")
    }

    fn greet(&self) -> String {
        format!("HELLO {}", self.name())
    }
}

/// Calls the default greeting of `Plain`, which `Loud`'s own cannot be.
pub fn plain_greeting() -> String {
    Plain.greet()
}

/// The same call through the trait's path, with the receiver first.
pub fn plain_greeting_by_path() -> String {
    Greeter::greet(&Plain)
}

/// Calls the greeting of a value of any type, which may be either.
pub fn any_greeting<T: Greeter>(greeter: &T) -> String {
    greeter.greet()
}
