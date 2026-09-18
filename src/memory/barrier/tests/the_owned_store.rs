//! A store into a compiler-proven slot moves the ownership mark: the entity
//! the slot held loses it, the entity the slot now names gains it, and the
//! counts move exactly as the plain store moves them. The same entity stored
//! into its own slot keeps the mark, a null store only clears the old
//! occupant's, and an occupant outside the GC heap — or one held outside it —
//! takes none, because the one teardown that honours the mark is the GC-heap
//! holder's.

use super::*;
use crate::refcount::{entity_flags, entity_refcount, is_owned};

#[test]
fn an_owned_store_marks_the_occupant_and_an_overwrite_moves_the_mark() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut a = entity(MemoryCategory::GcHeap);
    let mut b = entity(MemoryCategory::GcHeap);
    let (pa, pb): (*mut RcHeader, *mut RcHeader) = (&mut a, &mut b);
    let mut slot: *mut RcHeader = std::ptr::null_mut();

    assert!(unsafe { store_ptr_owned(&mut arena, MemoryCategory::GcHeap, &mut slot, pa) });
    assert_eq!(slot, pa);
    assert!(
        is_owned(unsafe { entity_flags(pa) }),
        "the occupant of a proven slot"
    );
    assert_eq!(
        unsafe { entity_refcount(pa) },
        2,
        "initial + the slot's reference"
    );

    let old = slot;
    assert!(unsafe { store_ptr_owned(&mut arena, MemoryCategory::GcHeap, &mut slot, pb) });
    assert!(
        !is_owned(unsafe { entity_flags(pa) }),
        "displaced before it is dropped, so the release path reads it unmarked"
    );
    assert!(is_owned(unsafe { entity_flags(pb) }));

    unsafe { drop_ref(MemoryCategory::GcHeap, old) };
    assert_eq!(
        unsafe { entity_refcount(pa) },
        1,
        "the plain drop: one release"
    );
    assert_eq!(unsafe { entity_refcount(pb) }, 2);
}

#[test]
fn storing_the_occupant_into_its_own_slot_keeps_the_mark() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut a = entity(MemoryCategory::GcHeap);
    let pa: *mut RcHeader = &mut a;
    let mut slot: *mut RcHeader = std::ptr::null_mut();

    assert!(unsafe { store_ptr_owned(&mut arena, MemoryCategory::GcHeap, &mut slot, pa) });
    let old = slot;
    assert!(unsafe { store_ptr_owned(&mut arena, MemoryCategory::GcHeap, &mut slot, pa) });
    unsafe { drop_ref(MemoryCategory::GcHeap, old) };

    assert!(
        is_owned(unsafe { entity_flags(pa) }),
        "still the slot's occupant"
    );
    assert_eq!(
        unsafe { entity_refcount(pa) },
        2,
        "the retain and the release paired"
    );
}

#[test]
fn a_null_store_clears_the_mark_of_the_old_occupant() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut a = entity(MemoryCategory::GcHeap);
    let pa: *mut RcHeader = &mut a;
    let mut slot: *mut RcHeader = std::ptr::null_mut();

    assert!(unsafe { store_ptr_owned(&mut arena, MemoryCategory::GcHeap, &mut slot, pa) });
    let old = slot;
    assert!(unsafe {
        store_ptr_owned(
            &mut arena,
            MemoryCategory::GcHeap,
            &mut slot,
            std::ptr::null_mut(),
        )
    });
    assert!(slot.is_null());
    assert!(!is_owned(unsafe { entity_flags(pa) }));

    unsafe { drop_ref(MemoryCategory::GcHeap, old) };
    assert_eq!(unsafe { entity_refcount(pa) }, 1);
}

#[test]
fn the_box_form_moves_the_mark_the_same_way() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut a = entity(MemoryCategory::GcHeap);
    let mut b = entity(MemoryCategory::GcHeap);
    let (pa, pb): (*mut RcHeader, *mut RcHeader) = (&mut a, &mut b);
    let mut slot = Value::null();

    let boxed = |entity: *mut RcHeader| Value::entity(crate::value::Tag::Object, entity);
    assert!(unsafe { store_box_owned(&mut arena, MemoryCategory::GcHeap, &mut slot, boxed(pa)) });
    assert!(is_owned(unsafe { entity_flags(pa) }));

    assert!(unsafe { store_box_owned(&mut arena, MemoryCategory::GcHeap, &mut slot, boxed(pb)) });
    assert!(!is_owned(unsafe { entity_flags(pa) }));
    assert!(is_owned(unsafe { entity_flags(pb) }));
    assert_eq!(slot.entity_ptr(), pb);

    unsafe { drop_ref(MemoryCategory::GcHeap, pa) };
    assert!(unsafe {
        store_box_owned(&mut arena, MemoryCategory::GcHeap, &mut slot, Value::null())
    });
    assert!(!is_owned(unsafe { entity_flags(pb) }));
    unsafe { drop_ref(MemoryCategory::GcHeap, pb) };
    assert_eq!(unsafe { entity_refcount(pa) }, 1);
    assert_eq!(unsafe { entity_refcount(pb) }, 1);
}

/// An arena entity in a heap holder is an escapee: its count is the hold
/// count and its death is the reset's, so the mark that would make a
/// `dispose` tear it down at any count must not land on it. A heap entity in
/// an arena holder is released by the reset log, and no `dispose` of the
/// holder ever reads the mark, so it takes none either.
#[test]
fn an_occupant_outside_the_gc_heap_or_held_outside_it_takes_no_mark() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    // The escapee lives in arena memory, so the reset can read it as one.
    let pe = arena.alloc(16) as *mut RcHeader;
    unsafe { pe.write(entity(MemoryCategory::RequestArena)) };
    let mut heap_entity = entity(MemoryCategory::GcHeap);
    let ph: *mut RcHeader = &mut heap_entity;

    let mut heap_slot: *mut RcHeader = std::ptr::null_mut();
    assert!(unsafe { store_ptr_owned(&mut arena, MemoryCategory::GcHeap, &mut heap_slot, pe) });
    assert_eq!(
        heap_slot, pe,
        "an arena object escapes in place, it is not copied"
    );
    assert!(
        !is_owned(unsafe { entity_flags(pe) }),
        "an escapee takes no mark"
    );

    let mut arena_slot: *mut RcHeader = std::ptr::null_mut();
    assert!(unsafe {
        store_ptr_owned(
            &mut arena,
            MemoryCategory::RequestArena,
            &mut arena_slot,
            ph,
        )
    });
    assert!(
        !is_owned(unsafe { entity_flags(ph) }),
        "an arena holder's slot marks nothing"
    );

    unsafe { drop_ref(MemoryCategory::GcHeap, pe) };
    arena.reset_with(|_| {}, |_| {});
    assert_eq!(
        unsafe { entity_refcount(ph) },
        1,
        "the reset log released the heap entity"
    );
}

/// What the mark is for, read through the gate's own counter: a marked
/// entity's non-final decrement registers nothing, and once the slot has
/// displaced it, the same decrement registers it again.
#[test]
fn a_displaced_entity_registers_again_and_a_marked_one_does_not() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut a = entity(MemoryCategory::GcHeap);
    let pa: *mut RcHeader = &mut a;
    let mut slot: *mut RcHeader = std::ptr::null_mut();

    assert!(unsafe { store_ptr_owned(&mut arena, MemoryCategory::GcHeap, &mut slot, pa) });
    crate::refcount::take_admissions();
    unsafe { ll_retain(pa) };
    assert!(!unsafe { ll_release(pa) });
    assert_eq!(
        crate::refcount::take_admissions(),
        0,
        "marked: the gate refuses"
    );

    let old = slot;
    assert!(unsafe {
        store_ptr_owned(
            &mut arena,
            MemoryCategory::GcHeap,
            &mut slot,
            std::ptr::null_mut(),
        )
    });
    unsafe { drop_ref(MemoryCategory::GcHeap, old) };
    assert_eq!(
        crate::refcount::take_admissions(),
        1,
        "the drop's decrement is the displacement itself: 2 to 1, and unmarked, it registers"
    );
}

/// A COW value leaving the arena is copied by the publish, so the slot names
/// the copy: the mark lands on the copy, the arena original takes none.
#[test]
fn a_copy_leaving_the_arena_takes_the_mark_and_the_original_none() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext {
        arena: &raw mut arena,
    };
    let s = unsafe {
        crate::string::ll_string_new(&raw mut ctx, MemoryCategory::RequestArena, b"name")
    } as *mut RcHeader;
    let mut slot: *mut RcHeader = std::ptr::null_mut();

    assert!(unsafe { store_ptr_owned(&raw mut arena, MemoryCategory::GcHeap, &mut slot, s) });
    assert_ne!(slot, s, "the heap slot holds the copy");
    assert!(
        is_owned(unsafe { entity_flags(slot) }),
        "the copy is the occupant"
    );
    assert!(
        !is_owned(unsafe { entity_flags(s) }),
        "the original stayed in the arena"
    );

    let copy = slot;
    assert!(unsafe {
        store_ptr_owned(
            &raw mut arena,
            MemoryCategory::GcHeap,
            &mut slot,
            std::ptr::null_mut(),
        )
    });
    unsafe { drop_ref(MemoryCategory::GcHeap, copy) };
    arena.reset(|_| {});
}

/// A store the copy path refuses moves nothing: the slot, both counts and the
/// occupant's mark stand as they were.
///
/// The refusal is a real one and its allocation is named. `store_ptr_owned`
/// answers `false` only through [`store_ptr`], which answers it only where
/// `escape_copy` could not allocate — and that copy is refused here by giving
/// the thread a block budget of zero, on a thread whose GC heap has served
/// nothing yet, so the copy's first size class has no block and must ask the
/// pool. The pool-request count is what says so: a `false` on its own is
/// what every early return of the function also produces.
///
/// **The mark clause cannot fail here, and the case says so rather than
/// claiming it.** A refused store leaves the slot holding the entity it
/// already held, so `move_ownership_mark` would be called with the displaced
/// entity and the occupant being one and the same, which its own contract
/// leaves marked as it was. The early return above it is therefore
/// unobservable through the mark: a build that moves the mark before reading
/// the store's answer passes this case. What the case does hold is the branch
/// itself — the answer, the untouched slot, the untouched counts, and which
/// allocator refused.
#[test]
fn a_refused_copy_leaves_the_mark_the_slot_and_the_counts_alone() {
    let _g = crate::memory::block_pool::test_guard();

    let (answered, requests, slot_kept_the_holder, holder_stayed_marked, string_count) =
        std::thread::spawn(|| {
            assert!(
                crate::memory::heap::ll_thread_init(),
                "the pool served this thread"
            );
            let mut arena = Arena::new();
            let mut ctx = LLContext {
                arena: &raw mut arena,
            };
            // The value that would be copied: a COW string in arena memory,
            // which a GC-heap slot may not hold as it stands.
            let cow = unsafe {
                crate::string::ll_string_new(&raw mut ctx, MemoryCategory::RequestArena, b"name")
            } as *mut RcHeader;

            // The occupant the slot already holds, and the mark a refused
            // store must leave on it.
            let mut held = entity(MemoryCategory::GcHeap);
            let ph: *mut RcHeader = &mut held;
            let mut slot: *mut RcHeader = std::ptr::null_mut();
            assert!(unsafe {
                store_ptr_owned(&raw mut arena, MemoryCategory::GcHeap, &mut slot, ph)
            });
            assert!(
                is_owned(unsafe { entity_flags(ph) }),
                "the occupant is marked"
            );

            let _budgeted = crate::memory::block_pool::budget_blocks(0);
            // The class the copy will ask for, emptied with the copy's own
            // call, so that the refusal is a block draw and not the pool's
            // warmth (`fill_the_class_until_refused`).
            let filled = unsafe { fill_the_class_until_refused(&raw mut ctx, b"name") };
            let _ = crate::memory::block_pool::take_pool_requests();
            let answered =
                unsafe { store_ptr_owned(&raw mut arena, MemoryCategory::GcHeap, &mut slot, cow) };
            let requests = crate::memory::block_pool::take_pool_requests();
            drop(_budgeted);
            for taken in filled {
                unsafe { crate::refcount::ll_release(taken) };
            }

            let answer = (
                answered,
                requests,
                slot == ph,
                is_owned(unsafe { entity_flags(ph) }),
                unsafe { entity_refcount(cow) },
            );
            unsafe { drop_ref(MemoryCategory::GcHeap, slot) };
            answer
        })
        .join()
        .unwrap();

    assert!(
        !answered,
        "the copy could not be allocated, so the store refused"
    );
    assert!(
        requests > 0,
        "the refusal is the pool's: the copy asked for a block and was told no"
    );
    assert!(slot_kept_the_holder, "the slot was not written");
    assert!(
        holder_stayed_marked,
        "the occupant keeps its mark: nothing displaced it"
    );
    assert_eq!(string_count, 1, "the value keeps the count it had");
}
