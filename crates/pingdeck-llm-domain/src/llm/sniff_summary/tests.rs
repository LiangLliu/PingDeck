//! The field cleaning the scan writes and the provider record reads back.

use super::clean_field;

#[test]
fn fields_are_cleaned_and_cut() {
    let long = "x".repeat(200);
    assert_eq!(clean_field(&long).chars().count(), 64);
    assert_eq!(clean_field("  Acme\nRelay\t "), "AcmeRelay");
    // Control characters go first, so the spaces they separated are trimmed too.
    assert_eq!(clean_field("\u{7}  x  "), "x");
}
