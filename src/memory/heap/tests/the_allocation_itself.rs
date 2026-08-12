//! A request picks a size class and comes back at that class's
//! stride, so it is at least as large as asked, and it returns to the
//! free list of the block it came from. A size past the largest class
//! is refused with null, and bulk-reserved cells are accounted live
//! until they are handed back.

use super::*;

#[test]
fn size_class_selection() {
    assert_eq!(size_class_index(1), Some(0));
    assert_eq!(size_class_index(16), Some(0));
    assert_eq!(size_class_index(17), Some(1));
    assert_eq!(size_class_index(8192), Some(SIZE_CLASSES.len() - 1));
    assert_eq!(size_class_index(8193), None);
}

/// Sixteen is the alignment every caller is entitled to, because
/// `ll_alloc` routes on it: a request of `align <= 16` goes to the
/// thread heap on the strength of this promise. Anything stricter
/// leaves the heap for the pooled path, which is `stdapi`'s test.
#[test]
fn alloc_is_aligned_and_sized() {
    let _g = crate::memory::block_pool::test_guard();
    let mut heap = Heap::new();
    let a = heap.alloc(40);
    let b = heap.alloc(40);
    assert!(!a.is_null());
    assert_eq!((b as usize).wrapping_sub(a as usize), 48);

    // Every class, and two slots of each: the first slot's alignment
    // comes from the block header's size and every later one from
    // the class's stride, which is the only other way a slot can
    // come back misaligned. It is a cheap assertion rather than a
    // probed one — `CLASS_LUT` is built in 16-byte steps, so a class
    // that is not a multiple of sixteen is never selected and cannot
    // be anyone's stride.
    for &size in SIZE_CLASSES.iter() {
        let first = heap.alloc(size);
        let second = heap.alloc(size);
        assert!(!first.is_null() && !second.is_null());
        for p in [first, second] {
            assert_eq!(
                p as usize % 16,
                0,
                "a slot of class {size} came back misaligned"
            );
        }

        unsafe {
            heap.free(first);
            heap.free(second);
        }
    }

    unsafe {
        heap.free(a);
        heap.free(b);
    }
}

#[test]
fn free_then_alloc_reuses_slot() {
    let _g = crate::memory::block_pool::test_guard();
    let mut heap = Heap::new();
    let a = heap.alloc(64);
    unsafe { heap.free(a) };
    let b = heap.alloc(64);
    assert_eq!(a, b, "a freed slot must be handed back");
    unsafe { heap.free(b) };
}

#[test]
fn too_large_returns_null() {
    let mut heap = Heap::new();
    assert!(heap.alloc(9000).is_null());
}

/// Cell reservation (`rfc/model/memory/bulk-operations.md`): the
/// manager answers with 0..=count cells, reports the leading
/// adjacent run honestly, accounts reserved cells as live, and
/// takes returned cells back into ordinary circulation.
#[test]
fn reserved_cells_are_accounted_returned_cells_recirculate() {
    let _g = crate::memory::block_pool::test_guard();
    let mut cells = [std::ptr::null_mut::<u8>(); 8];
    let mut contiguous = 0usize;
    let n = unsafe { ll_entity_reserve(48, 8, cells.as_mut_ptr(), &mut contiguous) };
    assert!(n >= 1 && n <= 8, "an answer between 0 and count; got {n}");
    assert!(contiguous <= n);
    // The reported run is honest: adjacent at a constant class
    // stride. Two things this used to get wrong, both invisible until
    // pool pressure made them real. It read `cells[1]` after asserting
    // only `n >= 1`, so a reserve that answered 1 subtracted from
    // null. And it took the stride unsigned, while the free list is
    // LIFO and hands cells back in descending address order, so the
    // honest stride is negative about as often as not. The run's
    // length is `contiguous`, not `n`.
    if contiguous >= 2 {
        let stride = cells[1] as isize - cells[0] as isize;
        for i in 1..contiguous {
            assert_eq!(
                cells[i] as isize - cells[i - 1] as isize,
                stride,
                "cell {i} breaks the reported run"
            );
        }
    }

    // Ordinary allocation must not hand out a reserved cell.
    let p = unsafe { entity_alloc(48) };
    assert!(
        !cells[..n].contains(&p),
        "a reserved cell was double-issued"
    );
    unsafe { crate::memory::stdapi::ll_free(p) };
    // Returned cells recirculate: the free-list is LIFO, so the
    // next allocation is the last cell returned.
    unsafe { ll_entity_cells_return(cells.as_ptr(), n) };
    let reused = unsafe { entity_alloc(48) };
    assert!(
        cells[..n].contains(&reused),
        "a returned cell did not recirculate"
    );
    unsafe { crate::memory::stdapi::ll_free(reused) };
}
