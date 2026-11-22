//! A strong long path and a weak short path to the same hub.

/// The changed function the long route ends at.
pub fn leaf(n: u32) -> u32 {
    n + 1
}

/// The changed function the hub takes as a value.
pub fn hook(n: u32) -> u32 {
    n + 1
}

/// One resolved call reaches the long route's change from here.
pub fn step1(n: u32) -> u32 {
    leaf(n)
}

/// The hub: two resolved calls reach the long route's change from here, and one
/// candidate step reaches the short route's change.
pub fn hub(n: u32) -> u32 {
    let direct = hook;
    let _ = direct;
    step1(n)
}

/// A resolved call reaches the hub from here.
pub fn caller(n: u32) -> u32 {
    hub(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caller_adds_both_routes() {
        assert_eq!(caller(1), 3);
    }
}
