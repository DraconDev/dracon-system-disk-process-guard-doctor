//! Tests for zram.rs (NixOS config snippet generation).

#[test]
fn memory_percent_default_and_bounds() {
    assert_eq!(crate::validate_memory_percent(None).unwrap(), 200);
    assert_eq!(crate::validate_memory_percent(Some(10)).unwrap(), 10);
    assert_eq!(crate::validate_memory_percent(Some(400)).unwrap(), 400);
    assert!(crate::validate_memory_percent(Some(0)).is_err());
    assert!(crate::validate_memory_percent(Some(9)).is_err());
    assert!(crate::validate_memory_percent(Some(401)).is_err());
    assert!(crate::validate_memory_percent(Some(u32::MAX)).is_err());
}
