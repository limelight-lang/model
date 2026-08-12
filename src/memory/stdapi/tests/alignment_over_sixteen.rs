//! The heap's slots are 16-aligned, so anything stricter leaves it
//! for the pooled path — which must not touch a thread heap that may
//! not exist yet.

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

#[test]
fn aligned_alloc_on_a_fresh_thread_does_not_deref_a_null_heap() {
    let _g = crate::memory::block_pool::test_guard();
    // A thread that never called `ll_thread_init`: an `align > 16` small
    // request must not route to the (null) thread heap.
    std::thread::spawn(|| unsafe {
        let p = ll_alloc(40, 64);
        assert!(!p.is_null());
        assert_eq!((p as usize) % 64, 0);
        ll_free(p);
    })
    .join()
    .unwrap();
}
