use super::*;

/// Size and alignment pick the route, and each of the three round
/// trips through the same size-less free: the small heap, one pooled
/// block, an OS-direct run.
mod the_three_size_routes {
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
}

/// The heap's slots are 16-aligned, so anything stricter leaves it
/// for the pooled path — which must not touch a thread heap that may
/// not exist yet.
mod alignment_over_sixteen {
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
}

/// Refusal is reported as null on every door. The two bands are
/// different defects: a near-`usize::MAX` size would wrap to a tiny
/// run and under-allocate, while anything past `isize::MAX` is
/// refused by `Layout` and used to abort the process.
mod a_size_the_allocator_must_refuse {
    use super::*;

    #[test]
    fn huge_size_overflow_returns_null_not_underallocation() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            // size + LINE_SIZE and the block round-up both overflow usize:
            // the request must be refused, never wrapped to a small run.
            assert!(ll_alloc(usize::MAX, 16).is_null());
            assert!(ll_alloc(usize::MAX - 100, 16).is_null());
        }
    }

    /// A size past `isize::MAX` is refused rather than aborting the
    /// process. It sits in the band the checked arithmetic passes and
    /// `Layout` refuses, so the old `unwrap` panicked — and a panic
    /// crossing `extern "C"` aborts, which is a report no caller can
    /// receive. `0x8000_0000_0000_0000` is a caller that lost a sign;
    /// `isize::MAX` itself is refused too, the block round-up carrying it
    /// over the limit.
    #[test]
    fn a_size_past_isize_max_returns_null_rather_than_aborting() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            assert!(ll_alloc(0x8000_0000_0000_0000, 16).is_null());
            assert!(ll_alloc(isize::MAX as usize, 16).is_null());
            // The ABI door as well, which is where the abort would have
            // happened: `ll_alloc` panics into its Rust caller, while
            // `ll_malloc` is `extern "C"` and cannot unwind out.
            assert!(ll_malloc(0x8000_0000_0000_0000).is_null());

            // And the growth door, which reaches the same call through
            // `ll_alloc`. A refused growth keeps the original.
            let live = ll_alloc(64, 16);
            assert!(!live.is_null());
            assert!(ll_realloc(live, 0x8000_0000_0000_0000, 16).is_null());
            std::ptr::write_bytes(live, 0xAB, 64);
            assert_eq!(*live, 0xAB, "the original survived the refusal");
            ll_free(live);
        }
    }

    /// The pooled LARGE path is the middle band — bigger than a heap slot,
    /// smaller than a block payload — and it is the band the exhaustion
    /// contract was written for: null, never a dead process. It used to
    /// write the block header before looking at the pointer, so a refusal
    /// there was a null dereference.
    #[test]
    fn pooled_large_reports_exhaustion_instead_of_writing_through_null() {
        let _g = crate::memory::block_pool::test_guard();
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;

        FORCE_OOM.store(true, Ordering::Relaxed);
        let p = unsafe { ll_alloc(20_000, 16) };
        let aligned = unsafe { ll_alloc(40, 64) }; // align > 16 routes here too
        FORCE_OOM.store(false, Ordering::Relaxed);

        assert!(p.is_null(), "exhaustion must report, not abort");
        assert!(aligned.is_null(), "the over-aligned route reports too");

        let q = unsafe { ll_alloc(20_000, 16) };
        assert!(!q.is_null(), "the path survived the refusal");
        unsafe { ll_free(q) };
    }
}

/// `realloc`, `calloc` and the `GlobalAlloc` impl, each defined by
/// what it must preserve rather than by how it allocates.
mod the_rest_of_the_c_surface {
    use super::*;

    #[test]
    fn realloc_grows_and_preserves() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let p = ll_alloc(16, 16);
            std::ptr::copy_nonoverlapping(b"hello".as_ptr(), p, 5);
            let p2 = ll_realloc(p, 40, 16);
            assert_eq!(std::slice::from_raw_parts(p2, 5), b"hello");
            ll_free(p2);
        }
    }

    #[test]
    fn realloc_null_is_alloc() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let p = ll_realloc(std::ptr::null_mut(), 32, 16);
            assert!(!p.is_null());
            ll_free(p);
        }
    }

    #[test]
    fn calloc_zeroes() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let p = ll_calloc(10, 8);
            assert!(!p.is_null());
            for i in 0..80 {
                assert_eq!(*p.add(i), 0);
            }

            ll_c_free(p);
        }
    }

    #[test]
    fn global_alloc_drives_a_vec() {
        let _g = crate::memory::block_pool::test_guard();
        // Exercise the standard Rust interface end to end.
        let a = LimelightAlloc;
        unsafe {
            let layout = Layout::array::<u64>(1000).unwrap();
            let p = a.alloc(layout) as *mut u64;
            assert!(!p.is_null());
            for i in 0..1000 {
                p.add(i).write(i as u64);
            }

            assert_eq!(*p.add(999), 999);
            a.dealloc(p as *mut u8, layout);
        }
    }
}
