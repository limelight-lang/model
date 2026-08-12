//! Four allocations on a write can be refused: the separation's
//! copy, the table's growth, the escape copy of a value crossing
//! into a longer-lived array, and the box. Each reports its refusal
//! with every array reading as it did before the call — `false` from
//! the three writes, and a null box from `make_ref`, whose result is
//! a pointer. A copy
//! destroyed mid-write gives its children back at once rather than
//! waiting for `ll_release`'s verdict, which on an arena copy never
//! reports a death.

use super::*;

/// A table's first storage: 8 index slots and 8 entries.
const FIRST_STORAGE_BYTES: usize = 288;

/// The separation's refusal: `false`, and nothing observable moved —
/// the slot, the original's count, its entries and the caller's value
/// reference all read as before the call.
#[test]
fn a_refused_separation_reports_and_changes_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    unsafe {
        crate::array::testing::insert(src, Key::Int(0), Value::int(10));
    }

    let (h, slot_a, _slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };
    let val = mk(b"unstored");

    FORCE_OOM.store(true, Ordering::Relaxed);
    let fillers = unsafe { exhaust_buffer_sources(FIRST_STORAGE_BYTES) };
    let stored = unsafe {
        set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot_a,
            Key::Int(1),
            Value::entity(Tag::String, val as *mut RcHeader),
        )
    };

    FORCE_OOM.store(false, Ordering::Relaxed);
    free_fillers(fillers);
    assert!(!stored, "the copy's storage was meant to be refused");

    unsafe {
        assert_eq!(
            (*slot_a).entity_ptr() as *mut LLArray,
            src,
            "a refused store moved the slot"
        );
        assert_eq!((*src).rc.refcount, 2, "a refused store moved a count");
        assert_eq!(crate::array::testing::table(src).len(), 1);
        assert!(crate::array::testing::get(src, Key::Int(1)).is_none());
        assert_eq!(
            (*val).rc.refcount,
            1,
            "a refused store kept the caller's value reference"
        );
        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert!(ll_release(val as *mut RcHeader));
        crate::object::ll_entity_die(val as *mut RcHeader);
    }
}

/// The table's own refusal, on an exclusively owned array: growth
/// cannot allocate, the store reports, and every entry reads as
/// before.
#[test]
fn a_refused_growth_reports_with_the_table_unchanged() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let class = ClassBuilder::new("OneHolder").prop("a", true).build();
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
        // Fill to capacity, so the next insert must grow.
        for i in 0..8i64 {
            crate::array::testing::insert(src, Key::Int(i), Value::int(i));
        }
    }

    FORCE_OOM.store(true, Ordering::Relaxed);
    let fillers = unsafe { exhaust_buffer_sources(DOUBLED_STORAGE_BYTES) };
    let stored = unsafe {
        set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(100),
            Value::int(1),
        )
    };

    FORCE_OOM.store(false, Ordering::Relaxed);
    free_fillers(fillers);
    assert!(!stored, "growth was meant to be refused");

    unsafe {
        assert_eq!(
            crate::array::testing::table(src).len(),
            8,
            "a refused growth moved an entry"
        );
        assert!(crate::array::testing::get(src, Key::Int(100)).is_none());
        for i in 0..8i64 {
            assert_eq!(
                crate::array::testing::get(src, Key::Int(i))
                    .unwrap()
                    .as_int(),
                i
            );
        }

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
    }
}

/// The table's refusal inside a private copy: the copy dies whole,
/// and the slot, the original and the caller's value all read as
/// before — the second refusal of the criterion, one array further
/// in than the separation's.
#[test]
fn a_growth_refusal_inside_the_copy_destroys_the_copy_alone() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    // Empty and shared: the separation's replay allocates nothing,
    // so the forced refusal lands on the copy's own first storage.
    let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let (h, slot_a, _slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };
    let val = mk(b"unstored");

    FORCE_OOM.store(true, Ordering::Relaxed);
    let fillers = unsafe { exhaust_buffer_sources(FIRST_STORAGE_BYTES) };
    // The separation must not be the refusal, or this measures
    // `a_refused_separation_reports_and_changes_nothing` a second
    // time — its assertions are these. The copy's entity comes from
    // the object heap, which the exhaustion above does not reach, so
    // prove a slot is there and hand it straight back.
    unsafe {
        let probe = ll_array_new(MemoryCategory::GcHeap);
        assert!(!probe.is_null(), "the copy's entity slot was refused");
        assert!(ll_release(probe as *mut RcHeader));
        crate::object::ll_entity_die(probe as *mut RcHeader);
    }

    let stored = unsafe {
        set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot_a,
            Key::Int(0),
            Value::entity(Tag::String, val as *mut RcHeader),
        )
    };

    FORCE_OOM.store(false, Ordering::Relaxed);
    free_fillers(fillers);
    assert!(!stored, "the copy's storage was meant to be refused");

    unsafe {
        assert_eq!((*slot_a).entity_ptr() as *mut LLArray, src);
        assert_eq!((*src).rc.refcount, 2);
        assert!(crate::array::testing::table(src).is_empty());
        assert_eq!(
            (*val).rc.refcount,
            1,
            "the giveback did not balance the copy's publication"
        );
        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert!(ll_release(val as *mut RcHeader));
        crate::object::ll_entity_die(val as *mut RcHeader);
    }
}

/// The refusal teardown cannot wait for `ll_release`'s verdict: on
/// an arena copy the release reports no death, and a verdict-gated
/// branch leaves every reference the replay published sitting on a
/// corpse until the reset — a shared COW child then reads an extra
/// holder and separates on every write for the rest of the request.
/// Seen failing exactly there with the gated teardown.
#[test]
fn a_destroyed_arena_copy_gives_its_children_back() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    let child = unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"cow") };
    unsafe {
        crate::refcount::ll_retain(child as *mut RcHeader);
        crate::array::testing::insert(
            src,
            Key::Int(0),
            Value::entity(Tag::String, child as *mut RcHeader),
        );
        crate::refcount::ll_retain(src as *mut RcHeader);
    }

    let before = unsafe { (*child).rc.refcount };

    let copy = unsafe {
        crate::array::entity::separate(
            src,
            MemoryCategory::RequestArena,
            arena_ptr,
            crate::array::entity::CopyReason::Duplication,
        )
    };

    assert!(!copy.is_null());
    unsafe {
        assert_eq!(
            (*child).rc.refcount,
            before + 1,
            "the replay was meant to take a reference of its own"
        );
        destroy_unpublished(copy as *mut RcHeader);
        assert_eq!(
            (*child).rc.refcount,
            before,
            "the corpse kept the replay's reference"
        );
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// The third refusal, which is neither the separation's nor the
/// table's: publishing an arena COW value into a longer-lived array
/// copies it out through `escape_copy`, and that copy is an
/// allocation no reserve funds. The array is exclusively owned, so
/// no separation runs, and its storage already exists, so no growth
/// runs.
#[test]
fn a_refused_escape_copy_of_the_value_reports_and_changes_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let class = ClassBuilder::new("EscapeHolder").prop("a", true).build();
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
        crate::array::testing::insert(src, Key::Int(0), Value::int(0));
    }

    let value = unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"arena") };
    let before = unsafe { (*value).rc.refcount };

    FORCE_OOM.store(true, Ordering::Relaxed);
    let fillers = unsafe { exhaust_string_entities(b"arena".len()) };
    let stored = unsafe {
        set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(1),
            Value::entity(Tag::String, value as *mut RcHeader),
        )
    };

    FORCE_OOM.store(false, Ordering::Relaxed);
    free_string_fillers(fillers);
    assert!(!stored, "the value's escape copy was meant to be refused");

    unsafe {
        assert_eq!((*slot).entity_ptr() as *mut LLArray, src);
        assert_eq!(crate::array::testing::table(src).len(), 1);
        assert!(crate::array::testing::get(src, Key::Int(1)).is_none());
        assert_eq!(
            (*value).rc.refcount,
            before,
            "a refused store kept the caller's value reference"
        );
        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// A refused box leaves the array exactly as it was, the key the
/// reference would have created included. The exclusively-owned path
/// has no private copy to throw away, so that rollback is explicit
/// and this is what holds it.
#[test]
fn a_refused_box_takes_the_vivified_element_back_out() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let class = ClassBuilder::new("RefusedRefHolder")
        .prop("a", true)
        .build();
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
        // One entry buys the storage, so the vivified insert below
        // needs no growth and the refusal lands on the box alone.
        crate::array::testing::insert(src, Key::Int(0), Value::int(1));
    }

    FORCE_OOM.store(true, Ordering::Relaxed);
    // A reference box is 24 bytes, the size class an empty inline
    // string takes.
    let fillers = unsafe { exhaust_string_entities(0) };
    let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot, Key::Int(9)) };
    FORCE_OOM.store(false, Ordering::Relaxed);
    free_string_fillers(fillers);
    assert!(r.is_null(), "the box was meant to be refused");

    unsafe {
        assert!(
            !crate::array::testing::contains(src, Key::Int(9)),
            "the refusal left the vivified element behind"
        );
        assert_eq!(crate::array::testing::table(src).len(), 1);
        assert_eq!(
            (*slot).entity_ptr() as *mut LLArray,
            src,
            "a refused reference separated"
        );
        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
    }
}
