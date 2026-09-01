//! A read yields what the box holds and a store goes through it, so
//! the entry still names the box afterwards and a refused store
//! leaves it holding what it held. `&$a[k]` separates a shared table
//! before it boxes, and an absent key is created as null first: the
//! null `box_element` reports means absent, and the layer above must
//! not forward it.

use super::*;

/// The by-value read of an element in a reference state yields what
/// the box holds rather than the box, and reading separates nothing:
/// both holders still name the one array afterwards.
#[test]
fn a_read_goes_through_a_reference_box_and_separates_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    unsafe {
        crate::array::testing::insert(src, Key::Int(0), Value::int(5));
        let boxed = box_element(src, arena_ptr, Key::Int(0));
        assert!(!boxed.is_null(), "the element was meant to be boxed");
        assert_eq!(
            crate::array::testing::get(src, Key::Int(0)).unwrap().tag(),
            Tag::Reference,
            "the entry does not hold a box, so the read proves nothing"
        );
    }

    let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

    unsafe {
        let read = get(slot_a, Key::Int(0)).expect("the key is there");
        assert_eq!(read.tag(), Tag::Int, "the read handed the box back");
        assert_eq!(read.as_int(), 5);
        assert!(get(slot_a, Key::Int(1)).is_none(), "an absent key answered");
        assert_eq!(
            (*slot_a).entity_ptr() as *mut LLArray,
            src,
            "the read separated"
        );
        assert_eq!((*slot_b).entity_ptr() as *mut LLArray, src);

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
    }
}

/// A store into an element in a reference state goes **through** the
/// box: the entry still names the box afterwards, the box holds the
/// new value, and the value it displaced came back.
#[test]
fn a_store_into_a_boxed_element_goes_through_the_box() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let class = ClassBuilder::new("BoxHolder").prop("a", true).build();
    let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
    let slot = unsafe { Object::prop_at(h, 16) };
    let first = mk(b"first");
    let boxed = unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            h as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        ll_release(src as *mut RcHeader);
        crate::refcount::ll_retain(first as *mut RcHeader);
        crate::array::testing::insert(
            src,
            Key::Int(0),
            Value::entity(Tag::String, first as *mut RcHeader),
        );
        let boxed = box_element(src, arena_ptr, Key::Int(0));
        assert!(!boxed.is_null(), "the element was meant to be boxed");
        boxed
    };

    let first_held = unsafe { crate::refcount::entity_refcount(first) };

    let second = mk(b"second");
    let second_start = unsafe { crate::refcount::entity_refcount(second) };
    assert!(unsafe {
        set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(0),
            Value::entity(Tag::String, second as *mut RcHeader),
        )
    });

    unsafe {
        assert_eq!(
            crate::array::testing::get(src, Key::Int(0))
                .unwrap()
                .entity_ptr(),
            boxed as *mut RcHeader,
            "the store replaced the box instead of writing through it"
        );
        assert_eq!((*boxed).value.entity_ptr(), second as *mut RcHeader);
        assert_eq!(
            get(slot, Key::Int(0)).unwrap().entity_ptr(),
            second as *mut RcHeader
        );
        assert_eq!(
            crate::refcount::entity_refcount(first),
            first_held - 1,
            "the value the box displaced did not come back"
        );
        assert_eq!(
            crate::refcount::entity_refcount(second),
            second_start + 1,
            "the box took no reference of its own"
        );

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert_eq!(crate::refcount::entity_refcount(second), second_start);
        for s in [first, second] {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
    }
}

/// The barrier publishes before it releases, so a store through the
/// box that the barrier refuses leaves the box holding exactly what
/// it held — the displaced value keeps its reference rather than
/// being dropped for a store that never happened.
#[test]
fn a_refused_store_through_the_box_keeps_the_displaced_value() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let class = ClassBuilder::new("RefusedBoxHolder")
        .prop("a", true)
        .build();
    let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
    let slot = unsafe { Object::prop_at(h, 16) };
    let held = mk(b"held");
    let boxed = unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            h as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        ll_release(src as *mut RcHeader);
        crate::refcount::ll_retain(held as *mut RcHeader);
        crate::array::testing::insert(
            src,
            Key::Int(0),
            Value::entity(Tag::String, held as *mut RcHeader),
        );
        let boxed = box_element(src, arena_ptr, Key::Int(0));
        assert!(!boxed.is_null());
        boxed
    };

    let held_start = unsafe { crate::refcount::entity_refcount(held) };

    // An arena COW value crossing into the heap box is copied out,
    // and that copy is the allocation the refusal lands on.
    let crossing = unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"crossing") };
    let crossing_start = unsafe { crate::refcount::entity_refcount(crossing) };

    let oom = force_oom();
    let fillers = unsafe { exhaust_string_entities(b"crossing".len()) };
    let stored = unsafe {
        set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(0),
            Value::entity(Tag::String, crossing as *mut RcHeader),
        )
    };

    drop(oom);
    free_string_fillers(fillers);
    assert!(!stored, "the crossing value's copy was meant to be refused");

    unsafe {
        assert_eq!(
            (*boxed).value.entity_ptr(),
            held as *mut RcHeader,
            "a refused store moved the box"
        );
        assert_eq!(
            crate::refcount::entity_refcount(held),
            held_start,
            "the displaced value was released for a store that never happened"
        );
        assert_eq!(crate::refcount::entity_refcount(crossing), crossing_start);

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert!(ll_release(held as *mut RcHeader));
        crate::object::ll_entity_die(held as *mut RcHeader);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// An absent key is null here, and the layer above is what turns that
/// into a vivified element ([`make_ref`]): the two nulls mean
/// different things and only one of them is a refusal.
#[test]
fn box_element_reports_on_an_absent_key() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;

    let a = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    unsafe {
        crate::array::testing::insert(a, Key::Int(1), Value::int(1));
        assert!(box_element(a, arena_ptr, Key::Int(2)).is_null());

        let absent = mk(b"nope");
        assert!(box_element(a, arena_ptr, Key::Str(absent)).is_null());
        assert!(ll_release(absent as *mut RcHeader));
        crate::object::ll_entity_die(absent as *mut RcHeader);

        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_entity_die(a as *mut RcHeader);
    }
}

/// `$r = &$a['nope']` creates the element as null and references it,
/// which is PHP's rule and the reason the layer cannot forward the
/// boxing step's null: that one means "absent".
///
/// Read through the array rather than out of the entry: what the
/// caller is owed is that `$a[5]` is null afterwards and that a write
/// through `$r` is visible there. Which entity the entry holds to
/// achieve it is this layer's to change.
#[test]
fn a_reference_to_an_absent_key_creates_it_as_null() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let class = ClassBuilder::new("VivifyHolder").prop("a", true).build();
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

    let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot, Key::Int(5)) };

    unsafe {
        assert!(!r.is_null(), "an absent key was reported as a refusal");
        assert_eq!(
            get(slot, Key::Int(5))
                .expect("the absent key was not created")
                .tag(),
            Tag::Null,
            "the vivified element reads as something other than null"
        );

        // The write `$r = 7` makes goes into the box's own slot,
        // which is where a reference-state element is written.
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            r as *mut RcHeader,
            &raw mut (*r).value,
            std::ptr::null_mut(),
            Value::int(7),
        ));
        let read = get(slot, Key::Int(5)).expect("the key stopped existing");
        assert_eq!(
            read.tag(),
            Tag::Int,
            "the write through the reference is not visible at the key"
        );
        assert_eq!(read.as_int(), 7);

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
    }
}

/// The whole of the shared-box rule: `$a=['x'=>1]; $b=$a; $r=&$b['x']; $r=2`
/// leaves `$a['x']` at 1 and `$b['x']` at 2. The shared table is
/// separated before the box is written, so `$a` never names the box
/// and the reference is not refused.
#[test]
fn taking_a_reference_separates_the_shared_table_first() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let x = mk(b"x");
    unsafe {
        crate::refcount::ll_retain(x as *mut RcHeader);
        crate::array::testing::insert(src, Key::Str(x), Value::int(1));
    }

    let x_shared = unsafe { crate::refcount::entity_refcount(x) };
    // `slot_a` is `$a`, `slot_b` is `$b`.
    let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

    let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot_b, Key::Str(x)) };
    assert!(!r.is_null(), "the reference was refused");

    unsafe {
        assert_ne!(
            (*slot_b).entity_ptr() as *mut LLArray,
            src,
            "the shared table was boxed without separating"
        );
        assert_eq!(
            (*slot_a).entity_ptr() as *mut LLArray,
            src,
            "the other holder followed the separation"
        );
        assert!(
            crate::array::testing::get(src, Key::Str(x)).unwrap().tag() != Tag::Reference,
            "the original's element was boxed too"
        );

        // `$r = 2` through the public entry point: `$b['x']` is in a
        // reference state, so the store finds the box and writes
        // into it.
        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot_b,
            Key::Str(x),
            Value::int(2)
        ));
        assert_eq!((*r).value.as_int(), 2, "the store missed the box");
        assert_eq!(
            get(slot_a, Key::Str(x)).unwrap().as_int(),
            1,
            "the write through the reference reached the other holder"
        );
        assert_eq!(get(slot_b, Key::Str(x)).unwrap().as_int(), 2);

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert_eq!(crate::refcount::entity_refcount(x), x_shared - 1);
        assert!(ll_release(x as *mut RcHeader));
        crate::object::ll_entity_die(x as *mut RcHeader);
    }
}
