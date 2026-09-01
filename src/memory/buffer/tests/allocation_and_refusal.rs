//! A payload is rounded up on the way in, and a refused growth leaves
//! the buffer exactly as it was — capacity included, since a stamped
//! capacity would promise memory the buffer does not own.

use super::*;

#[test]
fn ensure_allocates_rounded_and_empty_stays_empty() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = arena();
    let mut b = Buffer::new();

    let p = buffer_ensure(&mut a, &mut b, 20, 0);
    assert!(!p.is_null());
    assert_eq!(b.len, 0);
    assert!(b.capacity >= 20);
    assert_eq!(b.capacity % 8, 0);
}

/// A refused growth must leave the buffer as it was. Copying into the
/// null payload was a write through null; stamping the new capacity in
/// would have left a buffer promising memory it does not own.
#[test]
fn refused_growth_leaves_the_buffer_intact() {
    let _g = crate::memory::block_pool::test_guard();
    use crate::memory::block_pool::force_oom;

    let mut a = arena();
    let mut b = Buffer::new();
    assert!(buffer_append(&mut a, &mut b, b"hello"));
    // Not the bump top any more, so growth must allocate; and the rest
    // of the block is too small for the request below, so the
    // allocation has to ask the pool — which is what is refused.
    let _intruder = a.alloc(40_000);
    let (data, capacity) = (b.data, b.capacity);

    let oom = force_oom();
    let p = buffer_ensure(&mut a, &mut b, 40_000, 0);
    let appended = buffer_append(&mut a, &mut b, &[0u8; 40_000]);
    drop(oom);

    assert!(p.is_null(), "exhaustion must report, not abort");
    assert!(!appended, "a refused append reports instead of writing");
    assert_eq!((b.data, b.capacity, b.len), (data, capacity, 5));
    assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 5) }, b"hello");
}
