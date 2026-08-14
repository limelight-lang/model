//! A survivor the reset's own drain kills is read no further by the
//! passes that follow, and the count of what it held comes out right
//! anyway. The two halves are one subject: skipping a corpse without
//! carrying its edges across settles a live entity at zero, and carrying
//! them without skipping leans on a corpse's slots staying readable
//! (`dev/DECISIONS.md`, "the reset reads no corpse").

use super::*;

/// The shape every test here is built on: an entity promoted by the
/// reset and killed by the same reset's release drain.
///
/// A heap reference box takes the entity — that is the escape, so it is
/// promoted — and the box goes into an arena slot, which is what logs the
/// box's release against the reset. The test drops the box's creation
/// reference, so the logged release is its last and the drain is what
/// kills both it and the entity behind it.
///
/// # Safety
/// `arena` is the live arena, `corpse_slot_owner` an arena object with a
/// traced property at offset 16, `victim` a live arena entity.
unsafe fn killed_by_the_drain(
    arena: *mut Arena,
    corpse_slot_owner: *mut Object,
    victim: *mut RcHeader,
    victim_tag: Tag,
) {
    unsafe { killed_by_the_drain_at(arena, corpse_slot_owner, 16, victim, victim_tag) }
}

/// [`killed_by_the_drain`] with the slot named, for a test that needs two
/// kills and therefore two records in one owner.
///
/// # Safety
/// As [`killed_by_the_drain`], with `offset` a traced property of the
/// owner.
unsafe fn killed_by_the_drain_at(
    arena: *mut Arena,
    corpse_slot_owner: *mut Object,
    offset: u32,
    victim: *mut RcHeader,
    victim_tag: Tag,
) {
    unsafe {
        let boxed = crate::reference::ll_reference_new();
        assert!(ref_store(
            arena,
            boxed as *mut RcHeader,
            &raw mut (*boxed).value,
            std::ptr::null_mut(),
            Value::entity(victim_tag, victim),
        ));
        let slot = Object::prop_at(corpse_slot_owner, offset);
        assert!(ref_store(
            arena,
            corpse_slot_owner as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Reference, boxed as *mut RcHeader),
        ));
        assert!(!crate::refcount::ll_release(boxed as *mut RcHeader));
    }
}

/// Two arena objects hold the same COW array; the drain kills one of
/// them. The array must come out of the reset held by exactly the one
/// that lived.
///
/// **Today the right answer arrives by a cancellation nothing
/// guarantees**: the corpse's slot still names the array, because
/// `ll_default_dispose` releases a child without nulling its slot, so the
/// edge walk counts it (+1) and the release the same teardown performed
/// is inside the delta (-1). The two sum to zero by accident of each
/// kind's teardown leaving slots readable and stale — a property written
/// nowhere, and a compiler-generated `dispose` may drop it. What pins the
/// count by construction is the step that replaces the accident, and this
/// test is what it must keep green.
#[test]
fn a_cow_child_of_a_holder_the_drain_killed_settles_to_its_live_holders() {
    use crate::array::entity::ll_array_new;
    let _g = crate::memory::block_pool::test_guard();

    let holder_cls = ClassBuilder::new("CorpseCowHolder")
        .prop("items", true)
        .build();
    let cache_cls = ClassBuilder::new("CorpseCowCache")
        .prop("kept", true)
        .build();
    let corpse_cls = ClassBuilder::new("CorpseCowSlot").prop("box", true).build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(&mut *context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let dying =
        unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let living =
        unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let corpse =
        unsafe { new_constructed(&mut *context_ptr, corpse_cls, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

    unsafe {
        assert!(crate::array::testing::push(array, Value::int(7)));

        // Both holders take the array, arena into arena, so it is shared
        // rather than copied and its count is the count of its holders.
        for holder in [dying, living] {
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
        }

        // The living holder survives by an ordinary escape into the heap.
        store_prop(arena_ptr, cache, 16, living);
        // The dying one survives too — and is then killed by the drain.
        killed_by_the_drain(arena_ptr, corpse, dying as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(&mut *arena_ptr) };
    set_current_context(std::ptr::null_mut());

    unsafe {
        assert_eq!(
            (*(array as *mut RcHeader)).memory_category(),
            MemoryCategory::GcHeap,
            "the array stayed behind in the dying arena"
        );
        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            1,
            "the array is held by the survivor that lived, and by nothing else"
        );

        // And the count is the truth rather than a number: the last
        // holder's death takes the array with it.
        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}

/// A survivor in a block of its own, killed by the drain, with a COW
/// survivor beside it so the reconciliation actually runs. Its run went
/// back to the system at its death, so every later reader of that address
/// reads memory the process no longer owns — which is what the reset
/// window holds it against.
///
/// Miri is the regression: the read passes `cargo test` by construction
/// (`dev/WORKFLOW.md`, Tests).
#[test]
fn a_large_survivor_killed_by_the_drain_is_not_read_by_the_reconcile() {
    use crate::array::entity::ll_array_new;
    use crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN;
    let _g = crate::memory::block_pool::test_guard();

    let wide = crate::test_support::wide_class("WideUnderTheReconcile", RUN_FILLERS, None);
    let cache_cls = ClassBuilder::new("WideReconcileCache")
        .prop("kept", true)
        .build();
    let holder_cls = ClassBuilder::new("WideReconcileHolder")
        .prop("items", true)
        .build();
    let corpse_cls = ClassBuilder::new("WideReconcileSlot")
        .prop("box", true)
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(&mut *context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let holder =
        unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let corpse =
        unsafe { new_constructed(&mut *context_ptr, corpse_cls, MemoryCategory::RequestArena) };
    let wide_obj =
        unsafe { new_constructed(&mut *context_ptr, wide, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

    let run = BlockHeader::of_ptr(wide_obj as *const u8) as usize;
    assert_eq!(
        unsafe { block_kind(wide_obj as *const u8) },
        BLOCK_KIND_ENTITY_LARGE_RUN,
        "the entity fits in a shared block, so this test proves nothing"
    );

    unsafe {
        // The COW survivor: without one, `reconcile_cow_counts` returns
        // on its first line and reads nothing at all.
        assert!(crate::array::testing::push(array, Value::int(3)));
        let slot = Object::prop_at(holder, 16);
        assert!(ref_store(
            arena_ptr,
            holder as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        store_prop(arena_ptr, cache, 16, holder);

        killed_by_the_drain(arena_ptr, corpse, wide_obj as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(&mut *arena_ptr) };
    set_current_context(std::ptr::null_mut());

    assert!(
        !crate::memory::large_entity::snapshot().contains(&run),
        "the run outlived the entity it was allocated for, so the window \
         never flushed it"
    );
    unsafe {
        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            1,
            "the surviving holder is the array's one holder"
        );
        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}

/// A COW child whose edge is **unset** before its holder dies. The
/// release the unset performed is inside the child's delta, and the
/// holder that owed the compensation is gone by the time the count is
/// settled — so without the promotion-time snapshot the same event is
/// subtracted twice and the child settles at zero, which
/// `retained::register` reads as an empty slot under a living holder.
#[test]
fn an_edge_unset_before_its_holder_dies_still_settles_its_child() {
    use crate::array::entity::ll_array_new;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _g = crate::memory::block_pool::test_guard();

    static DOOMED_HOLDER: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn unset_the_edge(_o: *mut Object) {
        let holder = DOOMED_HOLDER.load(Ordering::Relaxed) as *mut Object;
        unsafe {
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            store_prop(arena, holder, 16, std::ptr::null_mut());
        }
    }

    let holder_cls = ClassBuilder::new("UnsetHolder").prop("items", true).build();
    let cache_cls = ClassBuilder::new("UnsetCache").prop("kept", true).build();
    let slots_cls = ClassBuilder::new("UnsetSlots")
        .prop("first", true)
        .prop("second", true)
        .build();
    let trigger_cls = ClassBuilder::new("UnsetTrigger")
        .destructor(unset_the_edge as *const ())
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let slots = unsafe { new_constructed(context_ptr, slots_cls, MemoryCategory::RequestArena) };
    let doomed = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let living = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let trigger =
        unsafe { new_constructed(context_ptr, trigger_cls, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    DOOMED_HOLDER.store(doomed as usize, Ordering::Relaxed);

    unsafe {
        assert!(crate::array::testing::push(array, Value::int(5)));
        for holder in [doomed, living] {
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
        }

        store_prop(arena_ptr, cache, 16, living);
        // The trigger dies first and unsets the edge; the holder dies
        // after it, holding nothing. Order is the release log's, which is
        // the order these two records were written in.
        killed_by_the_drain_at(arena_ptr, slots, 16, trigger as *mut RcHeader, Tag::Object);
        killed_by_the_drain_at(arena_ptr, slots, 32, doomed as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(&mut *arena_ptr) };
    set_current_context(std::ptr::null_mut());

    unsafe {
        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            1,
            "the array is held by the survivor that lived, and by nothing else"
        );
        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}
