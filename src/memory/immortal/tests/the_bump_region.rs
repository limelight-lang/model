//! Immortal memory is a bump over pooled blocks: addresses walk
//! forward and aligned, a full block is replaced rather than reused,
//! and nothing is ever given back.

use super::*;

#[test]
fn allocations_are_sequential_and_aligned() {
    let _g = crate::memory::block_pool::test_guard();

    let a = immortal_alloc(24);
    let b = immortal_alloc(1);
    assert_eq!(a as usize % 8, 0);
    // Same block => bump distance; fresh block => still 8-aligned.
    if BlockHeader::of_ptr(a) == BlockHeader::of_ptr(b) {
        assert_eq!(b as usize - a as usize, 24);
    }

    assert_eq!(
        unsafe { (*BlockHeader::of_ptr(a)).kind.load(Ordering::Relaxed) },
        BLOCK_KIND_IMMORTAL
    );
}

#[test]
fn refills_into_fresh_block_when_full() {
    let _g = crate::memory::block_pool::test_guard();

    let first = immortal_alloc(8);
    // Exhaust whatever remains of the current block.
    loop {
        let p = immortal_alloc(BLOCK_PAYLOAD / 4);
        if BlockHeader::of_ptr(p) != BlockHeader::of_ptr(first) {
            assert_eq!(
                unsafe { (*BlockHeader::of_ptr(p)).kind.load(Ordering::Relaxed) },
                BLOCK_KIND_IMMORTAL
            );
            break;
        }
    }
}

#[test]
fn ll_free_on_immortal_is_a_no_op() {
    let _g = crate::memory::block_pool::test_guard();

    let p = immortal_alloc(64);
    unsafe { (p as *mut u64).write(0xC0FFEE) };
    unsafe { crate::memory::stdapi::ll_free(p) };

    // The block was not recycled: kind untouched, data intact.
    let block = BlockHeader::of_ptr(p);
    assert_eq!(
        unsafe { (*block).kind.load(Ordering::Relaxed) },
        BLOCK_KIND_IMMORTAL
    );
    assert_ne!(
        unsafe { (*block).kind.load(Ordering::Relaxed) },
        BLOCK_KIND_FREE
    );
    assert_eq!(unsafe { *(p as *mut u64) }, 0xC0FFEE);
}
