//! Only reuse waits. The destructor runs at the death inside the
//! window, and the parked memory keeps its bytes: the walker reads a
//! slot's header in one pass and dereferences the class word in the
//! next, so a park link written into the slot would be followed as a
//! class pointer.

use super::*;

/// Review finding (2026-07-27): parking must not write the parked
/// memory. The walker reads a slot's header in one pass and
/// dereferences the class word at bytes 8–15 in the next; the
/// in-slot park link of the first draft landed exactly there. A
/// corpse must stay intact until the flush: header reading 0,
/// class word live.
#[test]
fn parking_leaves_the_corpse_bytes_intact() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("IntactCorpse").build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let addr = obj as usize;
    let class_word = unsafe { *((addr + 8) as *const usize) };
    assert_eq!(
        class_word, cls as usize,
        "the class word sits at bytes 8-15"
    );

    begin_epoch();
    unsafe {
        assert!(ll_release(obj as *mut RcHeader));
        crate::object::ll_object_die(obj);
    }

    assert_eq!(parked_count(), 1);
    unsafe {
        assert_eq!(
            (*(addr as *const RcHeader)).refcount,
            0,
            "header: reads free"
        );
        assert_eq!(
            *((addr + 8) as *const usize),
            class_word,
            "the class word survives parking — nothing wrote the corpse"
        );
    }

    end_epoch();
    assert_eq!(unsafe { flush() }, 1);
    arena.reset(|_| {});
}

/// Only reuse waits: the destructor runs at death, inside the window.
#[test]
fn a_parked_death_still_runs_its_destructor_on_time() {
    let _g = crate::memory::block_pool::test_guard();
    static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn counting(_o: *mut crate::object::Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    }

    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("ParkedDestructor")
        .destructor(counting as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };

    begin_epoch();
    unsafe {
        assert!(ll_release(obj as *mut RcHeader));
        crate::object::ll_object_die(obj);
    }

    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        1,
        "the entity dies on time; only the memory waits"
    );
    end_epoch();
    assert_eq!(unsafe { flush() }, 1);
    arena.reset(|_| {});
}
