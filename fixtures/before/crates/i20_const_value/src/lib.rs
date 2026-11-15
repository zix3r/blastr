//! A constant whose value changes.

/// The number of retries allowed.
pub const MAX_RETRIES: u32 = 3;

/// The delay between retries in milliseconds.
pub const DELAY_MS: u32 = 100;

/// Reads the retry limit.
pub fn retries() -> u32 {
    MAX_RETRIES
}

/// Reads the delay.
pub fn delay() -> u32 {
    DELAY_MS
}

/// Describes the retry policy.
pub fn describe() -> String {
    format!("retries: {}", retries())
}
