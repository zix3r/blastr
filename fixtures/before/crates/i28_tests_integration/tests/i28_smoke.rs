//! An integration test of the library.

/// Calls the library function.
#[test]
fn adds_one() {
    assert_eq!(i28_tests_integration::inc(1), 2);
}
