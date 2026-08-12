//! `realloc`, `calloc` and the `GlobalAlloc` impl, each defined by
//! what it must preserve rather than by how it allocates.

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
