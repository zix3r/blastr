//! An enum variant that gains a field.

/// A message to show.
pub enum Message {
    /// A notice with its text.
    Notice(&'static str),
    /// A warning with a level.
    Warning(u8),
}

/// Builds a notice.
pub fn notice() -> Message {
    Message::Notice("plain")
}

/// Describes the message.
pub fn describe(message: &Message) -> &'static str {
    match message {
        Message::Notice(_) => "notice",
        Message::Warning(_) => "warning",
    }
}

/// Builds a warning.
pub fn warning(level: u8) -> Message {
    Message::Warning(level)
}
