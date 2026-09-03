//! The abort is free, and "free" is a claim about the heap rather than
//! about the arena: mark and scan write shadow rows, a row-initialization
//! bitmap and a worklist, all of them in memory the reset gives back, so a
//! collection that gave up halfway has nothing to undo in the heap.
//!
//! The refusal is forced **past the first descent**. An arena refused at
//! its first allocation proves the claim over a mark that never ran; the
//! one below has met two entities and subtracted an edge before the
//! memory runs out, which is the state a partial mark actually leaves.
//!
//! **What is folded is every live entity rather than every byte of a
//! touched block.** A block's payload runs past its bump cursor into
//! memory nothing has written, so a fold over the whole payload reads
//! uninitialised bytes and Miri stops the run — and this test is one of
//! the few that can see a stray write at all. The entity fold covers
//! more blocks and fewer bytes of each: every entity block in the
//! process, not only the ones this trace touched.

use super::*;

/// Bytes the arena hands out for `bytes`, which is the request rounded
/// up to its eight-byte grain (`TraceScratchArena::alloc`). The fixture leaves
/// the arena an exact remainder, so it has to count in the same units.
fn granted(bytes: usize) -> usize {
    bytes.next_multiple_of(8)
}

#[test]
fn a_refusal_two_entities_deep_leaves_the_heap_byte_identical() {
    let _g = test_guard();
    crate::memory::critical::drain_for_test();

    // Two size classes, so the chain's third entity lives in a block of
    // its own and its first touch is an allocation the arena has to
    // serve — which is where the fixture puts the refusal.
    let narrow = ClassBuilder::new("MarkAbortNarrow")
        .prop("next", true)
        .build();
    let wide = ClassBuilder::new("MarkAbortWide")
        .prop("a", true)
        .prop("b", true)
        .prop("c", true)
        .prop("d", true)
        .prop("e", true)
        .prop("f", true)
        .prop("g", true)
        .prop("next", true)
        .build();
    let narrow_size = unsafe { (*narrow).object_size } as usize;
    let wide_size = unsafe { (*wide).object_size } as usize;

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let head = unsafe { new_constructed(&mut context, narrow, MemoryCategory::GcHeap) };
    let middle = unsafe { new_constructed(&mut context, narrow, MemoryCategory::GcHeap) };
    let tail = unsafe { new_constructed(&mut context, wide, MemoryCategory::GcHeap) };

    unsafe {
        store_prop(&mut arena, head, prop_offset(0), middle);
        store_prop(&mut arena, middle, prop_offset(0), tail);
    }

    let near_block = crate::memory::block_pool::BlockHeader::of_ptr(head as *const u8) as *mut u8;
    let far_block = crate::memory::block_pool::BlockHeader::of_ptr(tail as *const u8) as *mut u8;
    assert_eq!(
        crate::memory::block_pool::BlockHeader::of_ptr(middle as *const u8) as *mut u8,
        near_block,
        "the two narrow entities share a block, so the descent reaches the second without asking for memory"
    );
    assert_ne!(
        near_block, far_block,
        "and the third is in a block of its own"
    );

    let headers_before = unsafe { every_header_folded() };
    let bytes_before = unsafe {
        object_bytes_folded(&[
            (head, narrow_size),
            (middle, narrow_size),
            (tail, wide_size),
        ])
    };

    // What the mark may have and no more: the near block's rows, and the one
    // worklist segment the first push draws. The far block's rows are the
    // allocation that finds the arena empty and both allocation paths
    // refusing.
    let room = granted(shadow::bytes_for(unsafe {
        crate::memory::heap::collector_block_slots(near_block)
    })) + granted(crate::cycle::stack::SEGMENT_BYTES);
    let mut shadow_arena = crate::cycle::testing::open_arena();
    let fill = shadow_arena.room_left() - room;
    assert!(!shadow_arena.alloc(fill).is_null());

    let oom = force_oom();
    assert!(
        BlockPool::global().get().is_null(),
        "the ordinary allocation path is refusing"
    );
    assert_eq!(
        crate::memory::critical::blocks_held(),
        0,
        "and the reserve allocation path has nothing to serve"
    );

    let answer = unsafe { mark(&mut shadow_arena, head as *mut RcHeader) };
    drop(oom);

    assert_eq!(answer, MarkResult::AllocationFailed);
    assert_eq!(
        unsafe { working_count(middle) },
        1,
        "the descent had reached the second entity and taken its in-edge off a count of two"
    );
    assert_eq!(
        shadow_arena.touched_blocks(),
        1,
        "and had touched the near block alone, the far one's rows being what was refused"
    );

    assert_eq!(
        unsafe { every_header_folded() },
        headers_before,
        "no entity's counted state moved"
    );
    assert_eq!(
        unsafe {
            object_bytes_folded(&[
                (head, narrow_size),
                (middle, narrow_size),
                (tail, wide_size),
            ])
        },
        bytes_before,
        "and no cell of the traced graph moved either"
    );

    shadow_arena.reset();
    unsafe {
        store_prop(&mut arena, head, prop_offset(0), std::ptr::null_mut());
        store_prop(&mut arena, middle, prop_offset(0), std::ptr::null_mut());
        for entity in [head, middle, tail] {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }

    crate::memory::critical::drain_for_test();
}
