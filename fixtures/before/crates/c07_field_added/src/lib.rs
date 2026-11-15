/// A configuration with a display name.
pub struct Config {
    /// The display name.
    pub name: &'static str,
}

impl Config {
    /// Builds a configuration with a default name.
    pub fn new() -> Config {
        Config { name: "default" }
    }

    /// Returns the display name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}
