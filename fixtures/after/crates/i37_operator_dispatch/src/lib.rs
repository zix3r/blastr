//! An operator calls the operator trait's implementation, which nothing names.
//!
//! `left + right` is `Add::add(left, right)`, but no path in the body says so:
//! without an edge for the operator itself a changed `add` has no callers.

/// A whole number of sampler ticks.
#[derive(Debug, Clone, Copy)]
pub struct Ticks(pub u32);

impl std::ops::Add for Ticks {
    type Output = Ticks;

    fn add(self, other: Ticks) -> Ticks {
        Ticks(self.0.saturating_add(other.0))
    }
}

impl std::ops::Sub for Ticks {
    type Output = Ticks;

    fn sub(self, other: Ticks) -> Ticks {
        Ticks(self.0.saturating_sub(other.0))
    }
}

impl std::ops::Neg for Ticks {
    type Output = Ticks;

    fn neg(self) -> Ticks {
        Ticks(self.0)
    }
}

/// Four samples kept in order.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    values: [i64; 4],
}

impl Window {
    /// A window of four values.
    pub fn new(values: [i64; 4]) -> Window {
        Window { values }
    }
}

impl std::ops::Index<usize> for Window {
    type Output = i64;

    fn index(&self, index: usize) -> &i64 {
        &self.values[index]
    }
}

/// Adds two tick counts with `+`.
pub fn total(left: Ticks, right: Ticks) -> u32 {
    (left + right).0
}

/// Subtracts one tick count from another with `-`.
pub fn gap(left: Ticks, right: Ticks) -> u32 {
    (left - right).0
}

/// Mirrors a tick count with unary `-`.
pub fn mirrored(value: Ticks) -> u32 {
    (-value).0
}

/// Reads the first sample through `[]`.
pub fn first(window: Window) -> i64 {
    window[0]
}

/// Doubles a total, so the propagation has a second step.
pub fn span(left: Ticks, right: Ticks) -> u32 {
    total(left, right) * 2
}
