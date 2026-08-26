//! Size, offsets and flag-bit values are ABI: compiled PHP reads them
//! as constants, so they are pinned by value and not only by name.

use super::*;

#[test]
fn box_is_16_bytes_with_fixed_offsets() {
    assert_eq!(size_of::<Value>(), 16);
    assert_eq!(core::mem::offset_of!(Value, payload), 0);
    assert_eq!(core::mem::offset_of!(Value, tag), 8);
    assert_eq!(core::mem::offset_of!(Value, flags), 9);
}

/// The flags byte's bit assignments are ABI (generated code tests
/// them by constant), so the values are pinned, not just the names.
#[test]
fn flag_bits_are_pinned_and_undef_is_not_null() {
    assert_eq!(VALUE_REFCOUNTED, 1);
    assert_eq!(VALUE_UNDEF, 2);

    let u = Value::undef();
    assert!(u.is_undef());
    assert!(!u.is_refcounted(), "undef is never traced or counted");
    assert!(
        !Value::null().is_undef(),
        "an all-zero Box is null, not undef"
    );
}
