//! A struct whose field changes type.

/// A measurement with a unit.
pub struct Measure {
    /// The number of units.
    pub amount: u32,
    /// The unit code.
    pub unit: u8,
}

/// Reads the amount.
pub fn amount_of(measure: &Measure) -> u32 {
    measure.amount
}

/// Reads the unit.
pub fn unit_of(measure: &Measure) -> u8 {
    measure.unit
}

/// Reports the amount as text.
pub fn report(measure: &Measure) -> String {
    format!("{} units", amount_of(measure))
}
