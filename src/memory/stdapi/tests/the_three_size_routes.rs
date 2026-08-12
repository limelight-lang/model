//! Size and alignment pick the route, and each of the three round
//! trips through the same size-less free: the small heap, one pooled
//! block, an OS-direct run.

use super::*;

#[test]
fn small_roundtrip() {
    let _g = crate::memory::block_pool::test_guard();
    unsafe {
        let p = ll_alloc(40, 16);
        assert!(!p.is_null());
        (p as *mut u64).write(0xDEAD_BEEF);
        assert_eq!(*(p as *mut u64), 0xDEAD_BEEF);
        ll_free(p);
    }
}

#[test]
fn large_single_block_roundtrip() {
    let _g = crate::memory::block_pool::test_guard();
    unsafe {
        let size = 20_000; // > 8 KB, < block payload
        let p = ll_alloc(size, 16);
        assert!(!p.is_null());
        // Writable across the whole request.
        std::ptr::write_bytes(p, 0xAB, size);
        assert_eq!(*p, 0xAB);
        assert_eq!(*p.add(size - 1), 0xAB);
        ll_free(p);
    }
}

#[test]
fn huge_os_direct_roundtrip() {
    let _g = crate::memory::block_pool::test_guard();
    unsafe {
        let size = 200_000; // > block payload -> OS-direct run
        let p = ll_alloc(size, 16);
        assert!(!p.is_null());
        assert_eq!(p as usize & BLOCK_MASK, LINE_SIZE, "run is block-aligned");
        std::ptr::write_bytes(p, 0xCD, size);
        assert_eq!(*p.add(size - 1), 0xCD);
        ll_free(p);
    }
}
