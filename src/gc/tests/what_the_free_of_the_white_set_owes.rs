//! A white-set member is freed carrying whatever count trial
//! deletion left, so the free writes the final zero itself: the
//! enumerators read that word as occupancy, and a slot they read as
//! live has a free-list link where the class pointer was. Everything
//! an entity holds outside its own slot goes back with it — an
//! array's table storage, a dynamic string's payload, the block a
//! class keeps outside its body — and an escape hold-count on an
//! arena object is dropped, or the reset promotes an escapee nobody
//! holds.

use super::*;
use crate::test_support::chunk_from_the_free_list;

/// A freed slot's header word is the enumerators' occupancy test:
/// `heap::for_each_entity_slot` and the epoch snapshot both read it and
/// treat a non-zero refcount as a live entity. An ordinary death drives
/// the count to zero on its way out, but a white-set member is freed
/// while its count is whatever trial deletion left, so the free has to
/// write the final zero itself.
///
/// The consequence is worse than an over-count. A freed object slot has
/// the free-list link at bytes 8-15, where the class pointer was, so a
/// walk that reads the slot as live follows a free-list link as a
/// `*const Class`.
#[test]
fn a_collected_member_leaves_a_slot_the_walk_reads_as_free() {
    let _g = crate::memory::block_pool::test_guard();

    let cls = ClassBuilder::new("WhiteRing")
        .prop("self", true)
        .prop("text", true)
        .build();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let s = unsafe { crate::string::ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"white") };
    assert!(!s.is_null());

    unsafe {
        assert!(ref_store(
            arena_ptr,
            obj as *mut RcHeader,
            Object::prop_at(obj, 16),
            std::ptr::null_mut(),
            Value::entity(Tag::Object, obj as *mut RcHeader),
        ));
        assert!(ref_store(
            arena_ptr,
            obj as *mut RcHeader,
            Object::prop_at(obj, 32),
            std::ptr::null_mut(),
            Value::entity(Tag::String, s as *mut RcHeader),
        ));
        assert!(!ll_release(s as *mut RcHeader));
        assert!(!ll_release(obj as *mut RcHeader));
    }

    let (obj_addr, string_addr) = (obj as usize, s as usize);
    let reclaimed = unsafe { collect_cycles() };
    assert!(reclaimed >= 2, "the self-ring was not collected");

    // Nothing allocates from this block between the collection and the
    // read: blocks are owner-allocated and this thread is the owner, so
    // the two slots are still on its free list.
    let refcount_at = |addr: usize| unsafe { *(addr as *const u32) };
    assert_eq!(
        refcount_at(obj_addr),
        0,
        "the collected object's slot still reads as a live entity"
    );
    assert_eq!(
        refcount_at(string_addr),
        0,
        "the collected string's slot still reads as a live entity"
    );

    arena.reset(|_| {});
}

/// The white set is freed by reclaiming each entity's own slot, and
/// an array's table storage is not in that slot. Without the arm the
/// storage is lost with no pointer left anywhere to it — a buffer
/// chunk holding its block's live count above zero for the life of
/// the process. `$obj->arr = [$obj]` reaches it with no new mechanism:
/// the object buffers as a candidate on kind 0 and `trace_entity`
/// pulls the array into the white set behind it.
///
/// Seen failing on the storage never coming back.
#[test]
fn a_collected_array_gives_its_table_storage_back() {
    use crate::array::table::Key;
    use crate::refcount::ll_retain;
    let _g = crate::memory::block_pool::test_guard();

    let cls = ClassBuilder::new("ArrayHolder").prop("arr", true).build();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let a = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };

    let (storage, capacity) = unsafe {
        // Retained before the entry is published, per `Table::insert`.
        ll_retain(obj as *mut RcHeader);
        crate::array::testing::insert(
            a,
            Key::Int(0),
            Value::entity(Tag::Object, obj as *mut RcHeader),
        );
        crate::array::testing::storage_and_capacity(a)
    };

    assert!(!storage.is_null(), "the insert allocated storage to lose");

    unsafe {
        let slot = Object::prop_at(obj, 16);
        assert!(ref_store(
            arena_ptr,
            obj as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, a as *mut RcHeader),
        ));
        // Both creation references go; the ring is all that holds
        // either of them now.
        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(obj as *mut RcHeader));
    }

    let reclaimed = unsafe { collect_cycles() };
    assert!(reclaimed >= 2, "the ring was not collected");

    assert_eq!(
        chunk_from_the_free_list(capacity),
        storage,
        "the array's table storage was never freed"
    );

    arena.reset(|_| {});
}

/// The same hole, one kind over. A dynamic string's payload is a
/// separate allocation too, and it has been reachable from cyclic
/// garbage since the layout landed — longer than the array has
/// existed. The critic pass that found the array half named only the
/// array; this is the rest of it.
///
/// A self-ring is enough: the object holds itself, so it is garbage,
/// and its string property is white behind it.
#[test]
fn a_collected_dynamic_string_gives_its_payload_back() {
    use crate::string::ll_string_new_dynamic;
    let _g = crate::memory::block_pool::test_guard();

    let cls = ClassBuilder::new("StringHolder")
        .prop("self", true)
        .prop("text", true)
        .build();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let s = unsafe {
        ll_string_new_dynamic(
            std::ptr::null_mut(),
            MemoryCategory::GcHeap,
            b"a payload",
            0,
        )
    };

    assert!(!s.is_null());
    let (payload, capacity) = unsafe { ((*s).data, (*s).capacity as usize) };
    assert!(!payload.is_null(), "the string has an out-of-line payload");

    unsafe {
        assert!(ref_store(
            arena_ptr,
            obj as *mut RcHeader,
            Object::prop_at(obj, 16),
            std::ptr::null_mut(),
            Value::entity(Tag::Object, obj as *mut RcHeader),
        ));
        assert!(ref_store(
            arena_ptr,
            obj as *mut RcHeader,
            Object::prop_at(obj, 32),
            std::ptr::null_mut(),
            Value::entity(Tag::String, s as *mut RcHeader),
        ));
        assert!(!ll_release(s as *mut RcHeader));
        assert!(!ll_release(obj as *mut RcHeader));
    }

    let reclaimed = unsafe { collect_cycles() };
    assert!(reclaimed >= 2, "the self-ring was not collected");

    assert_eq!(
        chunk_from_the_free_list(capacity),
        payload,
        "the string's payload was never freed"
    );

    arena.reset(|_| {});
}

/// The third shape of the same hole, and the one with no kind of its
/// own: a class may own counted cells outside its body, and the block
/// behind them is as separate an allocation as a table's storage. This
/// collector frees the white set itself and calls no `dispose`, so the
/// teardown's last act never runs and the group's own `free` is all
/// that reaches the block.
///
/// A self-ring through a cell of the block is enough, and it is also
/// the proof that the cell was traced at all: the object's whole count
/// is that reference, so a trial deletion blind to it would leave the
/// object live.
#[test]
fn a_collected_object_gives_its_outside_block_back() {
    use crate::test_support::outside_block;
    let _g = crate::memory::block_pool::test_guard();

    let cls = outside_block::class("WhiteWaker");
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe { outside_block::install_block(&mut ctx, obj) };
    let (block, capacity) = unsafe { outside_block::block_and_capacity(obj) };
    assert!(!block.is_null(), "the object has no block to lose");

    unsafe {
        assert!(
            outside_block::store_cell(
                arena_ptr,
                obj,
                0,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, obj as *mut RcHeader),
            ),
            "the barrier refused the ring this test is built on"
        );
        assert!(
            !ll_release(obj as *mut RcHeader),
            "the cell holds the object now"
        );
    }

    let reclaimed = unsafe { collect_cycles() };
    assert!(reclaimed >= 1, "the self-ring was not collected");

    assert_eq!(
        chunk_from_the_free_list(capacity),
        block,
        "the collected object's block was never freed"
    );

    arena.reset(|_| {});
}

/// A cyclic garbage holder that referenced an arena object still
/// owes it a `lose`. The trace never sees arena entities — only the
/// heap is traced — so freeing the white set has to drop those
/// hold-counts itself. Left standing, the count makes arena reset
/// believe a dead holder still holds the escapee, and reset promotes
/// it: a leak for the life of the process, and a live-looking object
/// nobody can reach.
#[test]
fn collecting_a_holder_drops_its_escape_hold_counts() {
    use crate::refcount::IS_ESCAPEE;

    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("EscHolder")
        .prop("peer", true)
        .prop("esc", true)
        .build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    unsafe {
        let a = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
        let b = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
        let escapee = new_constructed(&mut ctx, node_class(), MemoryCategory::RequestArena);

        link(&mut arena, a, 16, b); // a <-> b: a heap cycle
        link(&mut arena, b, 16, a);
        link(&mut arena, a, 32, escapee); // and a holds an arena object
        assert_ne!(
            (*escapee).rc.flags & IS_ESCAPEE,
            0,
            "the store made it an escapee"
        );

        assert!(!ll_release(a as *mut RcHeader));
        assert!(!ll_release(b as *mut RcHeader));
        assert_eq!(ll_gc_collect_cycles(), 2, "the cycle is garbage");

        assert_eq!(
            (*escapee).rc.flags & IS_ESCAPEE,
            0,
            "the dead holder let go of its escapee"
        );
    }

    arena.reset(|_| {});
}
