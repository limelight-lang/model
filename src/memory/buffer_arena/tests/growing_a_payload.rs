//! A payload that is still the last thing bumped grows by moving the
//! bump, which is the case an append loop is in on every iteration;
//! one with something allocated after it is copied and leaves a hole
//! behind. Past a block payload the chunk is an OS-direct run.

use super::*;

/// A payload that is still the last thing bumped grows by moving the
/// bump, and the bytes already written stay where they are.
///
/// This is the case an append loop is in on every iteration, and it is
/// worth a test of its own because the alternative — reallocate and
/// copy — is correct, passes every other assertion in this file, and
/// costs a copy of everything written so far on each step.
#[test]
fn a_payload_at_the_bump_top_grows_without_moving() {
    let _g = crate::memory::block_pool::test_guard();
    let mut b = Buffer::new();

    buffer_ensure_longlived(&mut b, 64, 0);
    unsafe { std::ptr::copy_nonoverlapping(b"payload".as_ptr(), b.data, 7) };
    b.len = 7;
    let first = b.data;

    for step in 0..4 {
        let before = b.capacity;
        buffer_ensure_longlived(&mut b, before + 1, 0);
        assert_eq!(b.data, first, "step {step}: nothing was allocated after it");
        assert!(b.capacity > before, "step {step}: no room was gained");
    }

    assert_eq!(
        unsafe { std::slice::from_raw_parts(b.data, 7) },
        b"payload",
        "extending in place does not touch what was written"
    );

    unsafe { buffer_release_longlived(&mut b) };
}

/// A payload with something allocated after it is not at the bump top,
/// so growth has to move it — allocate, copy, free the old chunk — and
/// the chunk it leaves behind is a hole that `critical` mode reuses.
///
/// The spacer is what puts the payload off the top. Without it this
/// grows in place and there is no old chunk to recycle, which is the
/// case the test below covers instead.
#[test]
fn a_payload_off_the_bump_top_moves_and_leaves_a_reusable_hole() {
    let _g = crate::memory::block_pool::test_guard();
    let mut b = Buffer::new();

    buffer_ensure_longlived(&mut b, 64, 0);
    unsafe { std::ptr::copy_nonoverlapping(b"payload".as_ptr(), b.data, 7) };
    b.len = 7;
    let old = b.data;
    let old_capacity = b.capacity;

    let (spacer, spacer_size) = with_buffer_arena(|a| a.alloc(64));
    assert!(!spacer.is_null());

    set_pressure_mode(PressureMode::Critical);
    let grow_to = b.capacity + 1;
    buffer_ensure_longlived(&mut b, grow_to, 0);
    assert_ne!(b.data, old, "a payload off the bump top has to move");
    assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 7) }, b"payload");

    // The old chunk is a hole now: a fitting alloc must find it.
    let (p, _) = with_buffer_arena(|a| a.alloc(old_capacity));
    assert_eq!(p, old, "old payload must be reusable in critical mode");
    set_pressure_mode(PressureMode::Plenty);

    unsafe { buffer_release_longlived(&mut b) };
    with_buffer_arena(|a| unsafe {
        a.free(p, old_capacity);
        a.free(spacer, spacer_size);
    });
}

#[test]
fn over_block_payload_goes_os_direct_and_back() {
    let _g = crate::memory::block_pool::test_guard();
    let mut b = Buffer::new();

    buffer_ensure_longlived(&mut b, BLOCK_PAYLOAD * 2, 0);
    assert!(b.capacity >= BLOCK_PAYLOAD * 2);
    unsafe { std::ptr::write_bytes(b.data, 0xCD, b.capacity) };

    // Shrink-to-arena is not a thing; release routes by kind.
    unsafe { buffer_release_longlived(&mut b) };
    assert!(b.data.is_null());
    assert_eq!(b.capacity, 0);
}
