//! Constants and statics.

/// The default limit.
pub const LIMIT: u32 = 8;

/// The name of the tool.
pub static NAME: &str = "blastr";

/// Uses both items.
pub fn describe() -> String {
    let limit = LIMIT;
    format!("{NAME}: {limit}")
}
