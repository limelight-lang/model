//! The heap's slots are 16-aligned, so anything stricter leaves it
//! for the pooled path — which must not touch the thread heap, a started
//! thread whose heap the OS refused having none.

use super::*;

#[test]
fn aligned_alloc_over_16_honors_alignment() {
    let _g = crate::memory::block_pool::test_guard();
    unsafe {
        // align > 16 for a small size must be honored (the heap gives
        // only 16); several in a row so a mis-aligned heap slot would
        // show. Pooled payloads sit at +256, satisfying up to MAX_ALIGN.
        for align in [32usize, 64, 128, 256] {
            let ptrs: Vec<*mut u8> = (0..4).map(|_| ll_alloc(40, align)).collect();
            for &p in &ptrs {
                assert!(!p.is_null());
                assert_eq!((p as usize) % align, 0, "align {align} honored");
            }

            for p in ptrs {
                ll_free(p);
            }
        }

        // Above MAX_ALIGN is unsupported → null.
        assert!(ll_alloc(40, 512).is_null());
    }
}

/// An `align > 16` small request takes the pooled path and not the heap's:
/// the block it comes back in carries the pooled kind. What the routing
/// protects is a started thread whose heap the OS refused, which has no
/// heap to route to; no seam refuses the heap on demand, so the routing is
/// read off the block instead.
#[test]
fn an_over_aligned_small_request_takes_the_pooled_path() {
    let _g = crate::memory::block_pool::test_guard();
    unsafe {
        let p = ll_alloc(40, 64);
        assert!(!p.is_null());
        assert_eq!((p as usize) % 64, 0);
        let block = BlockHeader::of_ptr(p);
        assert_eq!(
            load_block_kind(&raw const (*block).kind),
            BLOCK_KIND_LARGE,
            "a small request over 16-aligned went to the heap"
        );
        ll_free(p);
    }
}
