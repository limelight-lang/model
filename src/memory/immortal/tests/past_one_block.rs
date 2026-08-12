//! A request over a block payload takes an OS-direct run of its own,
//! which is a second shape beside the bump — and the bump has to be
//! left where it was when one is taken.

use super::*;

/// An allocation larger than one block payload used to hit an
/// `assert!`, which under `panic = "abort"` kills the process. That is
/// only a defensible reading of "caller bug" while no caller forwards
/// input, and a class's `[Class][vtbl][itables]` train has no such
/// bound. It now takes an OS-direct run, which still answers
/// `of_ptr` because the run is block-aligned.
#[test]
fn oversized_immortal_takes_an_os_direct_run() {
    let _g = crate::memory::block_pool::test_guard();

    let size = BLOCK_PAYLOAD * 3 + 7;
    let p = immortal_alloc(size);
    assert!(!p.is_null(), "an oversized immortal must not refuse here");
    assert_eq!(p as usize % 8, 0);

    let block = BlockHeader::of_ptr(p);
    assert_eq!(
        unsafe { (*block).kind.load(Ordering::Relaxed) },
        BLOCK_KIND_IMMORTAL
    );

    // Writable end to end, and the tail is really ours.
    unsafe {
        std::ptr::write_bytes(p, 0xA5, size);
        assert_eq!(*p, 0xA5);
        assert_eq!(*p.add(size - 1), 0xA5);
    }

    // A free is still a no-op, as for every other immortal pointer.
    unsafe { crate::memory::stdapi::ll_free(p) };
    assert_eq!(
        unsafe { (*block).kind.load(Ordering::Relaxed) },
        BLOCK_KIND_IMMORTAL
    );
    assert_eq!(unsafe { *p }, 0xA5);
}

/// The bump region must survive an oversized request: the run is its
/// own allocation and must not disturb the current block's cursor.
#[test]
fn an_oversized_run_does_not_disturb_the_bump_region() {
    let _g = crate::memory::block_pool::test_guard();

    let a = immortal_alloc(16);
    let big = immortal_alloc(BLOCK_PAYLOAD + 1);
    let b = immortal_alloc(16);

    assert!(!big.is_null());
    assert_ne!(BlockHeader::of_ptr(big), BlockHeader::of_ptr(a));
    assert_eq!(BlockHeader::of_ptr(a), BlockHeader::of_ptr(b));
    assert_eq!(b as usize - a as usize, 16);
}
