//! A trait that gains a supertrait.

/// Something with a name.
pub trait Named {
    /// The name of the thing.
    fn name(&self) -> &'static str;
}

/// Something that can be counted and named.
pub trait Counter: Named {
    /// The count of things.
    fn count(&self) -> u32;
}

/// A tally of things.
pub struct Tally {
    count: u32,
}

impl Named for Tally {
    fn name(&self) -> &'static str {
        "tally"
    }
}

impl Counter for Tally {
    fn count(&self) -> u32 {
        self.count
    }
}

/// Counts through a trait object.
pub fn total(counter: &dyn Counter) -> u32 {
    counter.count()
}

/// Names something through the other trait.
pub fn describe(named: &dyn Named) -> &'static str {
    named.name()
}
