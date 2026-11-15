//! An enum that loses a variant.

/// A payment outcome.
pub enum Outcome {
    /// Paid in full.
    Paid,
    /// Waiting for the bank.
    Pending,
}

/// Describes the outcome.
pub fn describe(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Paid => "paid",
        _ => "pending",
    }
}

/// Maps a legacy outcome code.
pub fn from_code(code: u8) -> Outcome {
    match code {
        0 => Outcome::Paid,
        _ => Outcome::Pending,
    }
}

/// Reports whether payment is pending.
pub fn is_pending(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Pending => true,
        _ => false,
    }
}
