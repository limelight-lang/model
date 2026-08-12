//! A copy unwraps a box nobody else names and shares one a live `&`
//! still binds, which is where PHP collapses a reference and the
//! only place it does — so a write through a shared box reaches both
//! holders. In the arena the count is an upper bound, so the copy
//! errs toward sharing: a count above the holders can only share,
//! never unwrap a box a live name still reaches.

use super::*;

/// Four cases against php 8.3.6, in both memory
/// categories. The copy unwraps a box nobody else names and shares
/// one a live `&` still binds — which is where PHP collapses a
/// reference, and the only place it does.
#[test]
fn a_copy_unwraps_a_box_with_a_single_holder() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    for category in [MemoryCategory::GcHeap, MemoryCategory::RequestArena] {
        assert_eq!(
            unsafe { reference_then_copy(context_ptr, arena_ptr, category, false, false) },
            (1, 3),
            "no reference: {category:?}"
        );
        assert_eq!(
            unsafe { reference_then_copy(context_ptr, arena_ptr, category, true, false) },
            (1, 3),
            "a dead reference must not alias the copy: {category:?}"
        );
        assert_eq!(
            unsafe { reference_then_copy(context_ptr, arena_ptr, category, true, true) },
            (3, 3),
            "a live reference must alias the copy: {category:?}"
        );
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    unsafe { crate::promote::arena_reset_full(arena_ptr) };
}

/// A box is identity, so the deep copy out of the arena **shares**
/// it rather than boxing a second one, and the escape hold-count is
/// what keeps the arena box alive for the longer-lived copy.
#[test]
fn a_copy_over_an_arena_source_shares_the_box() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    let boxed = unsafe {
        crate::array::testing::insert(src, Key::Int(0), Value::int(7));
        let boxed = box_element(src, arena_ptr, Key::Int(0));
        assert!(!boxed.is_null());
        boxed
    };

    assert_eq!(
        unsafe { crate::object::header_category(boxed as *const RcHeader) },
        MemoryCategory::GcHeap,
        "an arena array's box is still a heap entity"
    );
    assert_eq!(
        unsafe { (*boxed).rc.flags } & crate::refcount::IS_ESCAPEE,
        0,
        "a heap box is never an escapee"
    );
    assert_eq!(
        unsafe { (*boxed).rc.refcount },
        1,
        "the source's entry is the box's one holder"
    );

    let copy = unsafe {
        crate::object::escape_copy(arena_ptr, MemoryCategory::GcHeap, src as *mut RcHeader)
    } as *mut LLArray;
    assert!(!copy.is_null());

    unsafe {
        assert_eq!(
            crate::array::testing::get(copy, Key::Int(0))
                .unwrap()
                .entity_ptr(),
            boxed as *mut RcHeader,
            "the copy boxed a second reference instead of sharing this one"
        );
        // A heap box is counted like any other heap entity, which is
        // the whole reason the box lives there: that count is what
        // the copy reads to decide between sharing and unwrapping. It
        // stood at one before this copy, and the copy shared anyway,
        // because an escape copy is a store crossing a lifetime
        // boundary rather than a duplication and collapses nothing
        // (`entity::CopyReason`).
        assert_eq!(
            (*boxed).rc.refcount,
            2,
            "the copy took no hold of its own on the shared box"
        );

        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert_eq!(
            (*boxed).rc.refcount,
            1,
            "the dying copy kept its hold on the box"
        );

        // The source's own reference is the reset's to give back, and
        // the record is what makes that happen. Draining it here is
        // the reset's release, done by hand so the box's death is
        // visible to this test.
        let mut logged = Vec::new();
        arena.drain_release_log(|e| logged.push(e));
        assert!(
            logged.contains(&(boxed as *mut RcHeader)),
            "the arena entry holding a heap box logged no release"
        );
        for e in logged {
            if ll_release(e) {
                crate::object::ll_entity_die(e);
            }
        }
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// The sequence that separates an exact holder count from an upper
/// bound, pinned in both categories because the two answers differ
/// and the difference is decided rather than accidental
/// (`dev/DECISIONS.md`, "a copy collapses a reference nobody else
/// names, and the arena reads that count as an upper bound").
///
/// `$a=[1]; $r=&$a[0]; $b=$a; $b[0]=3; unset($b); unset($r);
/// $c=$a; $c[0]=9;` then `($a[0], $c[0])`. php 8.3.6 answers
/// `(3, 9)`: by the third copy the box has one holder and is
/// collapsed. The heap agrees. The arena answers `(9, 9)`, because
/// `unset($b)` gives nothing back there — an arena container is not
/// counted, so it dies at the reset and its hold on the box stands
/// until then. The copy therefore errs toward sharing, which is the
/// safe direction: a count above the holders can only share, never
/// unwrap a box a live name still reaches.
#[test]
fn the_arena_reads_a_box_count_as_an_upper_bound() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    for (category, expected) in [
        (MemoryCategory::GcHeap, (3, 9)),
        (MemoryCategory::RequestArena, (9, 9)),
    ] {
        let class = ClassBuilder::new("UpperBoundHolder")
            .prop("a", true)
            .prop("b", true)
            .prop("c", true)
            .build();
        let holder = unsafe { new_constructed(context_ptr, class, category) };
        let slot_a = unsafe { Object::prop_at(holder, 16) };
        let slot_b = unsafe { Object::prop_at(holder, 32) };
        let slot_c = unsafe { Object::prop_at(holder, 48) };
        let a = unsafe { ll_array_new(category) };
        let answer = unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot_a,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, a as *mut RcHeader),
            ));
            ll_release(a as *mut RcHeader);
            assert!(set(
                context_ptr,
                category,
                slot_a,
                Key::Int(0),
                Value::int(1)
            ));

            // `$r = &$a[0]`, then a copy taken while it is alive.
            let r = make_ref(context_ptr, category, slot_a, Key::Int(0));
            assert!(!r.is_null());
            crate::refcount::ll_retain(r as *mut RcHeader);
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot_b,
                std::ptr::null_mut(),
                *slot_a,
            ));
            assert!(set(
                context_ptr,
                category,
                slot_b,
                Key::Int(0),
                Value::int(3)
            ));

            // `unset($b)` through the holder's own category, which is
            // the step the arena defers, and `unset($r)` through the
            // frame's.
            let held_b = (*slot_b).entity_ptr();
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot_b,
                held_b,
                Value::null(),
            ));
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, r as *mut RcHeader);

            // `$c = $a; $c[0] = 9;`
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot_c,
                std::ptr::null_mut(),
                *slot_a,
            ));
            assert!(set(
                context_ptr,
                category,
                slot_c,
                Key::Int(0),
                Value::int(9)
            ));

            let read_a = get(slot_a, Key::Int(0)).expect("the key is there").as_int();
            let read_c = get(slot_c, Key::Int(0)).expect("the key is there").as_int();
            if category == MemoryCategory::GcHeap {
                assert!(ll_release(holder as *mut RcHeader));
                ll_object_die(holder);
            }

            (read_a, read_c)
        };

        assert_eq!(answer, expected, "{category:?}");
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    unsafe { crate::promote::arena_reset_full(arena_ptr) };
}

/// A copy shares the box, so a write through one holder of a
/// once-shared array reaches the other: `$a=['x'=>1]; $r=&$a['x'];
/// $b=$a; $b['x']=3;` leaves both at 3, which is PHP's rule and the
/// reason its manual warns about copying an array holding a
/// reference.
#[test]
fn a_write_through_a_shared_box_reaches_both_holders() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let class = ClassBuilder::new("SharedBoxHolder")
        .prop("a", true)
        .prop("b", true)
        .build();
    let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
    let slot_a = unsafe { Object::prop_at(h, 16) };
    let slot_b = unsafe { Object::prop_at(h, 32) };
    unsafe {
        crate::array::testing::insert(src, Key::Int(0), Value::int(1));
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            h as *mut RcHeader,
            slot_a,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        ll_release(src as *mut RcHeader);
    }

    // `$r = &$a['x']` while `$a` is the only holder: nothing to
    // separate, so the box goes into the array both will share.
    let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot_a, Key::Int(0)) };
    assert!(!r.is_null());
    // `$r` is a name, and a name is a holder. The layer hands the box
    // back at the element's count and leaves the caller's reference to
    // the caller; without it the box has one holder and the copy below
    // would unwrap it rather than share it.
    unsafe { crate::refcount::ll_retain(r as *mut RcHeader) };
    assert_eq!(
        unsafe { (*slot_a).entity_ptr() } as *mut LLArray,
        src,
        "an exclusively owned array separated"
    );

    unsafe {
        // `$b = $a`, then `$b['x'] = 3`.
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            h as *mut RcHeader,
            slot_b,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot_b,
            Key::Int(0),
            Value::int(3)
        ));

        let copy = (*slot_b).entity_ptr() as *mut LLArray;
        assert_ne!(copy, src, "the shared table separated");
        assert_eq!(
            crate::array::testing::get(copy, Key::Int(0))
                .unwrap()
                .entity_ptr(),
            r as *mut RcHeader,
            "the copy boxed a second reference instead of sharing this one"
        );
        assert_eq!((*r).value.as_int(), 3);
        assert_eq!(
            get(slot_a, Key::Int(0)).unwrap().as_int(),
            3,
            "the shared box did not carry the write to the other holder"
        );
        assert_eq!(get(slot_b, Key::Int(0)).unwrap().as_int(), 3);

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        // `$r` goes out of scope last, and it is the box's final
        // holder once both arrays are gone.
        assert!(ll_release(r as *mut RcHeader));
        crate::object::ll_entity_die(r as *mut RcHeader);
    }
}
