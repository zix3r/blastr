/// A counter with a current value.
pub struct Counter {
    /// The current value.
    pub count: u64,
}

impl Counter {
    /// Creates a counter from a starting value.
    pub fn new(count: u32) -> Counter {
        Counter { count: count.into() }
    }

    /// Returns twice the current value.
    pub fn double(&self) -> u64 {
        self.count * 2
    }
}
