//! A payload over a block payload is an OS-direct run, which is a
//! second allocation shape: it outlives the reset the same way and it
//! grows by copying between runs.

use super::*;

#[test]
fn payload_larger_than_a_block_goes_os_direct_and_survives_until_reset() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = arena();
    let mut b = Buffer::new();

    buffer_append(&mut a, &mut b, b"start");
    let big = BLOCK_PAYLOAD * 3;
    buffer_ensure(&mut a, &mut b, big, 0);
    assert!(b.capacity >= big);

    // The whole payload is writable and the prefix survived the move.
    unsafe {
        std::ptr::write_bytes(b.data.add(b.len), 0xEE, b.capacity - b.len);
    }

    assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 5) }, b"start");

    a.reset(|_| {}); // frees the tracked OS-direct payload; no crash
}

#[test]
fn growth_in_large_payload_copies_between_runs() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = arena();
    let mut b = Buffer::new();

    buffer_ensure(&mut a, &mut b, BLOCK_PAYLOAD * 2, 0);
    unsafe { std::ptr::write_bytes(b.data, 0xAB, BLOCK_PAYLOAD * 2) };
    b.len = BLOCK_PAYLOAD * 2;

    buffer_ensure(&mut a, &mut b, BLOCK_PAYLOAD * 5, 0);
    assert_eq!(unsafe { *b.data }, 0xAB);
    assert_eq!(unsafe { *b.data.add(BLOCK_PAYLOAD * 2 - 1) }, 0xAB);

    a.reset(|_| {});
}
