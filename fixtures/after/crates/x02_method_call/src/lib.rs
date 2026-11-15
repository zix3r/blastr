//! Method call fixture.

/// A counter.
pub struct Counter {
    value: u32,
}

impl Counter {
    /// Increments the counter.
    pub fn bump(&mut self) {
        self.value += 1;
    }
}

/// Advances `counter` by one.
pub fn drive(counter: &mut Counter) {
    counter.bump();
}
