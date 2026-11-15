//! A constant whose type changes.

/// The page size in bytes.
pub const PAGE: u32 = 4096;

/// The number of pages for a size in bytes.
pub fn pages() -> u32 {
    PAGE / 512
}

/// The size of a word in bytes.
pub const WORD: u32 = 8;

/// Reads the word size.
pub fn word() -> u32 {
    WORD
}
