/// Returns the sum of the two values.
pub fn total(a: u32, b: u32) -> u32 {
    a + b
}

/// How many rounds of work to do.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    /// How deep to go.
    pub depth: u32,
}

impl Settings {
    /// The settings one round deeper than these.
    pub fn deeper(&self) -> Settings {
        Settings {
            depth: self.depth + 1,
        }
    }
}

/// The settings to start from.
pub fn defaults() -> Settings {
    Settings { depth: 2 }
}
