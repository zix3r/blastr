//! Field reads and writes.

/// Configuration with two values.
pub struct Config {
    /// The value that is read.
    pub a: u32,
    /// The value that is written.
    pub b: u32,
}

/// Reads field `a`.
pub fn read_a(config: &Config) -> u32 {
    config.a
}

/// Writes field `b`.
pub fn write_b(config: &mut Config, value: u32) {
    config.b = value;
}
