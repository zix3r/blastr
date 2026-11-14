//! Blastr's adapter to rust-analyzer.
//!
//! This is the only crate that depends on the `ra_ap_*` crates; every other
//! crate talks to it through plain Rust types, because no rust-analyzer type
//! ever appears in this crate's public API.

use ra_ap_syntax::{Edition, SourceFile};

/// Returns true when `text` parses as a Rust source file without syntax errors.
pub fn parse_ok(text: &str) -> bool {
    SourceFile::parse(text, Edition::CURRENT)
        .errors()
        .is_empty()
}

#[cfg(test)]
mod tests {
    use super::parse_ok;

    #[test]
    fn accepts_valid_snippet() {
        assert!(parse_ok("fn main() { let x = 1; }"));
    }

    #[test]
    fn accepts_empty_text() {
        assert!(parse_ok(""));
    }

    #[test]
    fn rejects_unclosed_parameter_list() {
        assert!(!parse_ok("fn main( {"));
    }

    #[test]
    fn rejects_struct_without_name() {
        assert!(!parse_ok("struct ;"));
    }
}
