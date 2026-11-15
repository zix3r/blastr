/// The identifier type used by this crate.
pub type Id = u32;

/// Returns the first identifier.
pub fn first() -> Id {
    0
}

/// Returns the identifier after the given one.
pub fn next(id: Id) -> Id {
    id + 1
}
