//! A `for` loop and an adaptor reach the iterator's own `next`.

/// Counts down from a number.
pub struct Countdown {
    remaining: u32,
}

impl Countdown {
    /// A countdown of `start` steps.
    pub fn new(start: u32) -> Countdown {
        Countdown { remaining: start }
    }
}

impl Iterator for Countdown {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        let current = self.remaining;
        if current < 2 {
            return None;
        }
        self.remaining = current - 1;
        Some(current)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.remaining as usize;
        (left, Some(left))
    }
}

/// Loops over a countdown.
pub fn sum_loop(start: u32) -> u32 {
    let mut total = 0;
    for value in Countdown::new(start) {
        total += value;
    }
    total
}

/// Collects a countdown through an adaptor.
pub fn sum_adaptor(start: u32) -> u32 {
    Countdown::new(start).map(|value| value * 2).sum()
}

/// Counts a countdown through an adaptor.
pub fn count_adaptor(start: u32) -> usize {
    Countdown::new(start).count()
}

/// Loops over a slice, which has no iterator implementation of this workspace.
pub fn sum_list(values: &[u32]) -> u32 {
    let mut total = 0;
    for value in values {
        total += *value;
    }
    total
}
