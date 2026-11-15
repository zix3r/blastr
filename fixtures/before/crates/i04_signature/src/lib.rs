//! A signature change with one adapted caller.

/// Formats a total.
pub fn format_total(value: u32) -> String {
    format!("total {value}")
}

/// Describes a total.
pub fn report(value: u32) -> String {
    format_total(value)
}

/// Announces a total.
pub fn announce(value: u32) -> String {
    report(value)
}

/// Formats a bare number.
pub fn bystander(value: u32) -> String {
    value.to_string()
}
