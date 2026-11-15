/// A configuration with a display name.
pub struct Config {
    /// The display name.
    pub name: &'static str,
    /// How many times the name is repeated.
    pub level: u32,
}

impl Config {
    /// Builds a configuration with a default name.
    pub fn new() -> Config {
        Config { name: "default", level: 1 }
    }

    /// Returns the display name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}
