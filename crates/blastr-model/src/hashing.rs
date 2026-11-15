//! Stable hashing of token sequences.

use std::collections::HashSet;

/// The FNV-1a 64-bit offset basis.
const FNV64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// The FNV-1a 64-bit prime.
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The FNV-1a 32-bit offset basis.
const FNV32_OFFSET: u32 = 0x811c_9dc5;

/// The FNV-1a 32-bit prime.
const FNV32_PRIME: u32 = 0x0100_0193;

/// Written after every token so that token boundaries take part in the hash.
const TOKEN_SEPARATOR: u8 = 0x1f;

/// How many hash values a sketch keeps.
const SKETCH_SIZE: usize = 16;

/// How many consecutive tokens one sketch window covers.
const WINDOW: usize = 3;

/// Hashes a sequence of tokens. Stable across runs, platforms and versions.
/// Returns 0 for an empty sequence and never for a non-empty one.
pub fn hash_tokens<'a>(tokens: impl IntoIterator<Item = &'a str>) -> u64 {
    let mut hash = FNV64_OFFSET;
    let mut any = false;

    for token in tokens {
        any = true;
        for byte in token.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV64_PRIME);
        }
        hash ^= u64::from(TOKEN_SEPARATOR);
        hash = hash.wrapping_mul(FNV64_PRIME);
    }

    if !any {
        return 0;
    }
    if hash == 0 { 1 } else { hash }
}

/// Hashes one window of tokens with FNV-1a, 32-bit.
fn hash_window(window: &[&str]) -> u32 {
    let mut hash = FNV32_OFFSET;
    for token in window {
        for byte in token.as_bytes() {
            hash ^= u32::from(*byte);
            hash = hash.wrapping_mul(FNV32_PRIME);
        }
        hash ^= u32::from(TOKEN_SEPARATOR);
        hash = hash.wrapping_mul(FNV32_PRIME);
    }
    hash
}

/// Builds a similarity sketch of a token sequence: the 16 smallest hashes of
/// its token 3-grams, sorted ascending. Shorter for short sequences.
pub fn sketch_tokens<'a>(tokens: impl IntoIterator<Item = &'a str>) -> Vec<u32> {
    let tokens: Vec<&str> = tokens.into_iter().collect();
    let mut hashes = match tokens.len() {
        0 => return Vec::new(),
        // A sequence shorter than one window is its own single window.
        len if len <= WINDOW => vec![hash_window(&tokens)],
        _ => tokens.windows(WINDOW).map(hash_window).collect(),
    };

    hashes.sort_unstable();
    hashes.dedup();
    hashes.truncate(SKETCH_SIZE);
    hashes
}

/// Estimates how similar two token sequences are from their sketches,
/// from 0.0 (nothing shared) to 1.0 (identical). Two empty sketches give 0.0.
pub fn sketch_similarity(a: &[u32], b: &[u32]) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }

    let k = SKETCH_SIZE.min(a.len().max(b.len()));
    if k == 0 {
        return 0.0;
    }

    let set_a: HashSet<u32> = a.iter().copied().collect();
    let set_b: HashSet<u32> = b.iter().copied().collect();

    let mut union: Vec<u32> = a.iter().chain(b).copied().collect();
    union.sort_unstable();
    union.dedup();
    union.truncate(k);

    let shared = union
        .iter()
        .filter(|value| set_a.contains(*value) && set_b.contains(*value))
        .count();

    shared as f32 / k as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A direct FNV-1a implementation, written out so the constants above are
    /// checked against the textbook algorithm rather than against themselves.
    fn reference_hash(tokens: &[&str]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for token in tokens {
            for byte in token.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            hash ^= 0x1f;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Tokens that make long, mostly distinct sequences.
    fn words(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("token{index}")).collect()
    }

    #[test]
    fn hash_of_nothing_is_zero() {
        assert_eq!(hash_tokens(std::iter::empty::<&str>()), 0);
        assert_eq!(hash_tokens(Vec::<&str>::new()), 0);
    }

    #[test]
    fn hash_of_something_is_non_zero() {
        assert_ne!(hash_tokens(["fn"]), 0);
        assert_ne!(hash_tokens([""]), 0);
        assert_ne!(hash_tokens(["fn", "main"]), 0);
    }

    #[test]
    fn hash_is_stable() {
        assert_eq!(hash_tokens(["fn", "main"]), hash_tokens(["fn", "main"]));
        assert_eq!(hash_tokens(["a", "b"]), hash_tokens(["a", "b"]));
    }

    #[test]
    fn hash_depends_on_token_boundaries() {
        assert_ne!(hash_tokens(["ab", "c"]), hash_tokens(["a", "bc"]));
        assert_ne!(hash_tokens(["ab"]), hash_tokens(["a", "b"]));
    }

    #[test]
    fn hash_matches_the_reference_implementation() {
        let expected = reference_hash(&["fn", "main"]);
        assert_ne!(expected, 0);
        assert_eq!(hash_tokens(["fn", "main"]), expected);
    }

    #[test]
    fn sketch_of_nothing_is_empty() {
        assert!(sketch_tokens(std::iter::empty::<&str>()).is_empty());
        assert!(sketch_tokens(Vec::<&str>::new()).is_empty());
    }

    #[test]
    fn sketch_is_short_for_short_inputs() {
        assert_eq!(sketch_tokens(["only"]).len(), 1);
        assert_eq!(sketch_tokens(["two", "tokens"]).len(), 1);
        assert_eq!(sketch_tokens(["three", "whole", "tokens"]).len(), 1);
        assert_eq!(sketch_tokens(["four", "whole", "input", "tokens"]).len(), 2);
    }

    #[test]
    fn sketch_is_at_most_sixteen_values() {
        let long = words(200);
        let sketch = sketch_tokens(long.iter().map(String::as_str));
        assert_eq!(sketch.len(), 16);
    }

    #[test]
    fn sketch_is_ascending_and_distinct() {
        let long = words(50);
        let sketch = sketch_tokens(long.iter().map(String::as_str));
        assert!(sketch.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn sketch_is_deterministic() {
        let long = words(40);
        let first = sketch_tokens(long.iter().map(String::as_str));
        let second = sketch_tokens(long.iter().map(String::as_str));
        assert_eq!(first, second);
    }

    #[test]
    fn similarity_of_identical_sequences_is_one() {
        let long = words(60);
        let sketch = sketch_tokens(long.iter().map(String::as_str));
        assert!((sketch_similarity(&sketch, &sketch) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn similarity_of_disjoint_sequences_is_low() {
        let left: Vec<String> = words(60);
        let right: Vec<String> = (0..60).map(|index| format!("other{index}")).collect();
        let left_sketch = sketch_tokens(left.iter().map(String::as_str));
        let right_sketch = sketch_tokens(right.iter().map(String::as_str));

        let similarity = sketch_similarity(&left_sketch, &right_sketch);
        assert!(similarity < 0.2, "expected a low value, got {similarity}");
    }

    #[test]
    fn similarity_changes_little_when_one_token_changes() {
        let mut changed = words(60);
        changed[30] = "changed".to_owned();

        let original = words(60);
        let original_sketch = sketch_tokens(original.iter().map(String::as_str));
        let changed_sketch = sketch_tokens(changed.iter().map(String::as_str));

        let similarity = sketch_similarity(&original_sketch, &changed_sketch);
        assert!(similarity > 0.5, "expected a high value, got {similarity}");
    }

    #[test]
    fn similarity_of_two_empty_sketches_is_zero() {
        assert!((sketch_similarity(&[], &[]) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn similarity_with_one_empty_sketch_is_zero() {
        let sketch = sketch_tokens(["a", "b", "c"]);
        assert!((sketch_similarity(&sketch, &[]) - 0.0).abs() < f32::EPSILON);
        assert!((sketch_similarity(&[], &sketch) - 0.0).abs() < f32::EPSILON);
    }
}
