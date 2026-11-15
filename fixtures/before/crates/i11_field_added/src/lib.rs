//! A struct that gains a field.

/// A server configuration.
#[derive(Default)]
pub struct Config {
    /// The host number.
    pub host: u32,
    /// The port number.
    pub port: u32,
}

/// Builds a configuration with every field named.
pub fn full(host: u32, port: u32) -> Config {
    Config { host, port }
}

/// Builds a configuration from the defaults.
pub fn partial(host: u32) -> Config {
    Config {
        host,
        ..Default::default()
    }
}

/// Destructures every field.
pub fn pair(config: Config) -> u32 {
    let Config { host, port } = config;
    host + port
}

/// Destructures the host only.
pub fn host_of(config: Config) -> u32 {
    let Config { host, .. } = config;
    host
}

/// Reads the port.
pub fn port_of(config: &Config) -> u32 {
    config.port
}
