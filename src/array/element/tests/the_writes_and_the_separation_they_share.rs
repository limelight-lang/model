//! Every write separates a shared array before it touches anything,
//! so a second holder sees none of it, and then publishes the copy,
//! spends its creation reference and drops the displaced original.
//! The order of those last two is `write_through`'s and is argued
//! there; what these tests read is the end state. An exclusively
//! owned array is written in place and hands the displaced element
//! back, and an arena holder's copy is an arena array too.

use super::*;

/// The store's whole composition, measured from both holders: a
/// store through one leaves the other's entries alone, the displaced
/// original ends at one holder so the next store to it writes in
/// place, the copy is held once, and the array takes a value
/// reference of its own without consuming the caller's.
#[test]
fn a_store_through_one_holder_leaves_the_other_holders_entries_alone() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    unsafe {
        crate::array::testing::insert(src, Key::Int(0), Value::int(10));
    }

    let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };
    let val = mk(b"forty-one");

    assert!(unsafe {
        set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot_a,
            Key::Int(1),
            Value::entity(Tag::String, val as *mut RcHeader),
        )
    });

    unsafe {
        let copy = (*slot_a).entity_ptr() as *mut LLArray;
        assert_ne!(copy, src, "the shared table separated");
        assert_eq!(
            crate::array::testing::get(copy, Key::Int(1))
                .unwrap()
                .entity_ptr(),
            val as *mut RcHeader
        );
        assert_eq!(
            crate::array::testing::get(copy, Key::Int(0))
                .unwrap()
                .as_int(),
            10,
            "the copy replayed the source"
        );
        assert!(
            crate::array::testing::get(src, Key::Int(1)).is_none(),
            "the other holder's entries changed"
        );
        assert_eq!(
            (*src).rc.refcount,
            1,
            "the displaced original keeps exactly its other holder"
        );
        assert_eq!((*copy).rc.refcount, 1, "the copy is held once, by the slot");
        assert_eq!(
            (*val).rc.refcount,
            2,
            "the array takes its own reference and leaves the caller's"
        );

        // The second store goes through the other holder, whose array
        // is now at count one: in place, no second separation.
        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot_b,
            Key::Int(1),
            Value::int(7),
        ));
        assert_eq!(
            (*slot_b).entity_ptr() as *mut LLArray,
            src,
            "a store to the displaced original separated again"
        );
        assert_eq!(
            crate::array::testing::get(src, Key::Int(1))
                .unwrap()
                .as_int(),
            7
        );

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert_eq!(
            (*val).rc.refcount,
            1,
            "the dying array did not give the value back"
        );
        assert!(ll_release(val as *mut RcHeader));
        crate::object::ll_entity_die(val as *mut RcHeader);
    }
}

/// The in-place arm, which the shared-array tests above never take:
/// an exclusively owned array takes a value reference of its own and
/// gives the displaced element back. One refcounted element
/// overwrites another, so both halves are measured on one entity
/// each.
#[test]
fn a_store_in_place_gives_the_displaced_element_back() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let class = ClassBuilder::new("InPlaceHolder").prop("a", true).build();
    let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
    let slot = unsafe { Object::prop_at(h, 16) };
    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            h as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        ll_release(src as *mut RcHeader);
    }

    let first = mk(b"first");
    let second = mk(b"second");
    let first_start = unsafe { (*first).rc.refcount };
    let second_start = unsafe { (*second).rc.refcount };

    unsafe {
        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(0),
            Value::entity(Tag::String, first as *mut RcHeader),
        ));
        assert_eq!(
            (*slot).entity_ptr() as *mut LLArray,
            src,
            "an exclusively owned array separated"
        );
        assert_eq!(
            (*first).rc.refcount,
            first_start + 1,
            "the array takes its own reference and leaves the caller's"
        );

        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(0),
            Value::entity(Tag::String, second as *mut RcHeader),
        ));
        assert_eq!(
            (*first).rc.refcount,
            first_start,
            "the displaced element kept the array's reference"
        );
        assert_eq!((*second).rc.refcount, second_start + 1);
        assert_eq!(
            crate::array::testing::get(src, Key::Int(0))
                .unwrap()
                .entity_ptr(),
            second as *mut RcHeader
        );

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert_eq!((*second).rc.refcount, second_start);
        for s in [first, second] {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
    }
}

/// The append's three clauses: it writes under the cursor's key, a
/// shared array separates so the other holder's length stays put,
/// and an exhausted cursor refuses instead of wrapping onto a live
/// entry.
#[test]
fn an_append_through_one_holder_leaves_the_other_holders_length_alone() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    unsafe {
        for i in 0..2i64 {
            crate::array::testing::insert(src, Key::Int(i), Value::int(10 + i));
        }
    }

    let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

    assert!(unsafe { append(context_ptr, MemoryCategory::GcHeap, slot_a, Value::int(99)) });

    unsafe {
        let copy = (*slot_a).entity_ptr() as *mut LLArray;
        assert_ne!(copy, src, "the shared table separated");
        assert_eq!(
            crate::array::testing::get(copy, Key::Int(2))
                .unwrap()
                .as_int(),
            99,
            "the append took the cursor's key"
        );
        assert_eq!(crate::array::testing::table(copy).len(), 3);
        assert_eq!(
            crate::array::testing::table(src).len(),
            2,
            "the other holder's length followed the append"
        );
        assert!(crate::array::testing::get(src, Key::Int(2)).is_none());

        // The original is exclusively `slot_b`'s now, so the highest
        // integer key goes straight in: the cursor has no successor
        // and the next append must refuse.
        crate::array::testing::insert(src, Key::Int(i64::MAX), Value::int(1));
        assert!(
            !append(context_ptr, MemoryCategory::GcHeap, slot_b, Value::int(0)),
            "an exhausted cursor appended anyway"
        );
        assert_eq!(
            crate::array::testing::table(src).len(),
            3,
            "a refused append wrote an entry"
        );
        assert_eq!(
            (*slot_b).entity_ptr() as *mut LLArray,
            src,
            "a refused append separated"
        );

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
    }
}

/// `unset` through one holder of a shared array: the copy loses the
/// entry, the other holder keeps it, and both of the table's
/// references come back — the key's by the table's ownership rule, the value's by
/// the barrier. The separation replays the entry and the removal
/// gives it back, so the measurement is a net zero on each entity.
#[test]
fn an_unset_gives_the_key_back_and_leaves_the_other_holder_standing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let key = mk(b"gone");
    let value = mk(b"payload");
    unsafe {
        crate::refcount::ll_retain(key as *mut RcHeader);
        crate::refcount::ll_retain(value as *mut RcHeader);
        crate::array::testing::insert(
            src,
            Key::Str(key),
            Value::entity(Tag::String, value as *mut RcHeader),
        );
    }

    let key_shared = unsafe { (*key).rc.refcount };
    let value_shared = unsafe { (*value).rc.refcount };
    let (h, slot_a, _slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

    assert!(unsafe { unset(context_ptr, MemoryCategory::GcHeap, slot_a, Key::Str(key)) });

    unsafe {
        let copy = (*slot_a).entity_ptr() as *mut LLArray;
        assert_ne!(copy, src, "the shared table separated");
        assert!(
            crate::array::testing::get(copy, Key::Str(key)).is_none(),
            "the copy kept the unset entry"
        );
        assert!(
            crate::array::testing::get(src, Key::Str(key)).is_some(),
            "the other holder lost its entry"
        );
        assert_eq!(
            (*key).rc.refcount,
            key_shared,
            "the removed key did not come back"
        );
        assert_eq!(
            (*value).rc.refcount,
            value_shared,
            "the removed element did not come back"
        );

        // An absent key is not an error, and it still separates:
        // `slot_a`'s array is exclusively its own by now, so the
        // observable part is only the report.
        assert!(unset(
            context_ptr,
            MemoryCategory::GcHeap,
            slot_a,
            Key::Int(7)
        ));

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert_eq!(
            (*key).rc.refcount,
            key_shared - 1,
            "the dying array kept it"
        );
        assert_eq!((*value).rc.refcount, value_shared - 1);
        for s in [key, value] {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
    }
}

/// The arena half of the operation, which every test above leaves
/// out: `separation_category` keeps an arena holder's copy in the
/// arena, so the store neither counts an escape nor logs a release,
/// and the reset reclaims both arrays.
#[test]
fn a_store_through_an_arena_holder_separates_into_the_arena() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    unsafe {
        crate::array::testing::insert(src, Key::Int(0), Value::int(10));
    }

    let class = ClassBuilder::new("ArenaHolder")
        .prop("a", true)
        .prop("b", true)
        .build();
    let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::RequestArena) };
    let slot_a = unsafe { Object::prop_at(h, 16) };
    let slot_b = unsafe { Object::prop_at(h, 32) };
    unsafe {
        for s in [slot_a, slot_b] {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                s,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
        }

        ll_release(src as *mut RcHeader);
    }

    assert!(unsafe {
        set(
            context_ptr,
            MemoryCategory::RequestArena,
            slot_a,
            Key::Int(1),
            Value::int(7),
        )
    });

    unsafe {
        let copy = (*slot_a).entity_ptr() as *mut LLArray;
        assert_ne!(copy, src, "the shared table separated");
        assert_eq!(
            crate::object::header_category(copy as *const RcHeader),
            MemoryCategory::RequestArena,
            "an arena holder's copy left the arena"
        );
        assert_eq!(
            crate::array::testing::get(copy, Key::Int(0))
                .unwrap()
                .as_int(),
            10
        );
        assert_eq!(
            crate::array::testing::get(copy, Key::Int(1))
                .unwrap()
                .as_int(),
            7
        );
        assert!(
            crate::array::testing::get(src, Key::Int(1)).is_none(),
            "the other holder's entries changed"
        );
        assert_eq!(
            (*src).rc.refcount,
            1,
            "the displaced original keeps exactly its other holder"
        );
        assert_eq!(
            (*copy).rc.flags & crate::refcount::IS_ESCAPEE,
            0,
            "an arena copy in an arena slot crossed no boundary"
        );
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());

    // Nothing was logged for the reset to release: the log exists for
    // a longer-lived entity entering an arena slot, and every entity
    // this store touched is the arena's own. Draining is what says
    // so — a spurious record would be freed by the reset below and
    // read as a clean run.
    let mut logged = 0usize;
    arena.drain_release_log(|_| logged += 1);
    assert_eq!(logged, 0, "an arena-to-arena store logged a release");

    arena.reset(|_| {});
}

/// The key-ownership half through `set` itself: a fresh string key is
/// consumed, an equal-bytes overwrite hands the operation's own
/// reference back, and a refused growth hands the published key
/// back. Each arm seen failing under a targeted revert of its
/// giveback.
#[test]
fn a_string_key_through_the_store_obeys_the_ownership_rule() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let class = ClassBuilder::new("KeyHolder").prop("a", true).build();
    let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
    let slot = unsafe { Object::prop_at(h, 16) };
    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            h as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        ll_release(src as *mut RcHeader);
    }

    let k1 = mk(b"key");
    let k2 = mk(b"key");
    assert_ne!(k1, k2, "two distinct entities, or the arms collapse");
    let k1_start = unsafe { (*k1).rc.refcount };
    let k2_start = unsafe { (*k2).rc.refcount };

    unsafe {
        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Str(k1),
            Value::int(1),
        ));
        assert_eq!(
            (*k1).rc.refcount,
            k1_start + 1,
            "a stored new key is consumed into the table's reference"
        );

        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Str(k2),
            Value::int(2),
        ));
        assert_eq!(
            (*k2).rc.refcount,
            k2_start,
            "the overwrite arm kept its published reference"
        );
        assert_eq!(
            crate::array::testing::get(src, Key::Str(k1))
                .unwrap()
                .as_int(),
            2
        );

        // Fill to capacity, so the next new key must grow — and the
        // growth is refused, so the published key must come back.
        for i in 0..7i64 {
            crate::array::testing::insert(src, Key::Int(i), Value::int(i));
        }

        let k3 = mk(b"other");
        let k3_start = (*k3).rc.refcount;
        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = exhaust_buffer_sources(DOUBLED_STORAGE_BYTES);
        let stored = set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Str(k3),
            Value::int(9),
        );
        FORCE_OOM.store(false, Ordering::Relaxed);
        free_fillers(fillers);
        assert!(!stored, "growth was meant to be refused");
        assert_eq!(
            (*k3).rc.refcount,
            k3_start,
            "the refused insert kept the published key"
        );

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert_eq!(
            (*k1).rc.refcount,
            k1_start,
            "the dying array did not give its key back"
        );
        for s in [k1, k2, k3] {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
    }
}
