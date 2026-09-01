//! Storing a key consumes the caller's reference and removing one
//! hands it back, while an overwrite keeps the entry's original key,
//! so the caller's stays the caller's. In an arena table the reset
//! log owes a heap key's release, so the giveback goes through
//! `drop_ref` rather than a bare `ll_release`, which would let the
//! reset drive a string somebody still holds to death — and a child
//! the copy counted as escaping has to lose that hold-count the same
//! way.

use super::*;

/// The table's key-ownership rule, both arms measured. Storing a new string
/// key consumes the caller's reference; the overwrite arm keeps the
/// entry's original key, so the caller's reference stays the
/// caller's; removing hands the stored key's reference back. Two
/// distinct entities with equal bytes, because one entity can
/// measure only one arm: the stored key catches the remove leak, the
/// overwriting key catches the stranded retain.
#[test]
fn a_stored_key_is_consumed_and_a_dropped_key_comes_back() {
    let _g = crate::memory::block_pool::test_guard();
    let a = mk(b"key");
    let b = mk(b"key");
    assert_ne!(a, b, "two distinct entities, or neither arm is measured");
    let e = hash_arr();
    let a0 = unsafe { crate::refcount::entity_refcount(a) };
    let b0 = unsafe { crate::refcount::entity_refcount(b) };

    unsafe {
        crate::refcount::ll_retain(a as *mut RcHeader);
        let (added, old) = crate::array::testing::insert(e, Key::Str(a), Value::int(1)).unwrap();
        assert!(added, "the first insert stores a new key");
        assert!(old.is_none());

        crate::refcount::ll_retain(b as *mut RcHeader);
        let (added, old) = crate::array::testing::insert(e, Key::Str(b), Value::int(2)).unwrap();
        assert!(!added, "equal bytes overwrite rather than add");
        assert_eq!(old.unwrap().as_int(), 1);
        // `added == false`: the caller's key was not stored, so the
        // retain above is still the caller's to give back.
        assert!(!ll_release(b as *mut RcHeader));

        let (v, key) = crate::array::testing::remove(e, Key::Str(b)).unwrap();
        assert_eq!(v.as_int(), 2);
        assert_eq!(key, a, "the entry kept its original key entity");
        assert!(!ll_release(key as *mut RcHeader), "the table's reference");

        assert_eq!(
            crate::refcount::entity_refcount(a),
            a0,
            "the stored key's references balance"
        );
        assert_eq!(
            crate::refcount::entity_refcount(b),
            b0,
            "the overwriting key's references balance"
        );

        assert!(ll_release(e as *mut RcHeader));
        crate::object::ll_entity_die(e as *mut RcHeader);
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_entity_die(a as *mut RcHeader);
        assert!(ll_release(b as *mut RcHeader));
        crate::object::ll_entity_die(b as *mut RcHeader);
    }
}

/// The ownership rule's cross-category half: in an arena table a
/// heap key's one release is owed by the reset log — the barrier
/// records it at publication — so the caller gives the returned key
/// up through `drop_ref`, which leaves log-owned references alone.
/// A bare `ll_release` there is the double free `Table::remove`'s
/// contract names: the reset's own release then drives the string to
/// death while the test still holds it. Seen failing exactly that
/// way.
#[test]
fn an_arena_tables_key_release_is_owed_by_the_reset_log() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let key = mk(b"heap key in an arena table");
    let e = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };

    unsafe {
        crate::refcount::ll_retain(key as *mut RcHeader);
        let published = crate::memory::barrier::store_category_barrier(
            arena_ptr,
            MemoryCategory::RequestArena,
            key as *mut RcHeader,
        );
        assert_eq!(
            published, key as *mut RcHeader,
            "a heap entity entering an arena slot is logged, not copied"
        );
        let (added, old) = crate::array::testing::insert(e, Key::Str(key), Value::int(1)).unwrap();
        assert!(added);
        assert!(old.is_none());

        let (v, k) = crate::array::testing::remove(e, Key::Str(key)).unwrap();
        assert_eq!(v.as_int(), 1);
        assert_eq!(k, key);
        // The table's reference is the log's to release at reset;
        // `drop_ref` knows that where a bare release would not.
        crate::memory::barrier::drop_ref(MemoryCategory::RequestArena, k as *mut RcHeader);
        assert_eq!(
            crate::refcount::entity_refcount(key),
            2,
            "the log still holds its one reference"
        );
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});

    unsafe {
        assert_eq!(
            crate::refcount::entity_refcount(key),
            1,
            "the reset's one release balanced the barrier's one record"
        );
        assert!(ll_release(key as *mut RcHeader));
        crate::object::ll_entity_die(key as *mut RcHeader);
    }
}

/// A refusal mid-copy gives a published child back through the
/// barrier, and the difference from a bare release is an escape
/// hold-count: the copy's barrier counted the non-COW arena child as
/// escaping into a heap destination, so the giveback must
/// `escape_lose` it — a bare `ll_release` no-ops on an arena entity
/// and leaves the count stuck, and the reset then treats a child
/// nobody holds as an escapee. Seen failing on the escapee flag.
///
/// **The entry's key is what the refusal lands on**, because the copy's
/// element goes across before its key does (`fill_table_from`) and the
/// giveback under test is the one that follows a published element. The
/// key is a COW arena string, which crossing into a heap destination is
/// copied and therefore refusable; the element is a dynamic one, which
/// is counted rather than copied and is the escapee the assertion reads.
/// The copy's own two allocations are served ahead of it — its entity
/// slot from the warmed block, its presized storage from the warmed
/// buffer arena — so neither of them is what refuses.
#[test]
fn a_refused_heap_copy_gives_an_escaped_child_back_through_the_barrier() {
    use crate::memory::block_pool::force_oom;

    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    // Warm a heap entity block, so the forced refusal below lands on
    // the key's crossing and not on the copy's own slot.
    let warm = arr();
    let _held = warm_the_buffer_arena();

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    let d = unsafe {
        crate::string::ll_string_new_dynamic(context_ptr, MemoryCategory::RequestArena, b"p", 0)
    };

    let k =
        unsafe { crate::string::ll_string_new(context_ptr, MemoryCategory::RequestArena, b"key") };

    assert!(!d.is_null() && !k.is_null());
    unsafe {
        crate::refcount::ll_retain(d as *mut RcHeader);
        crate::refcount::ll_retain(k as *mut RcHeader);
        crate::array::testing::insert(
            src,
            Key::Str(k),
            Value::entity(crate::value::Tag::String, d as *mut RcHeader),
        );
        crate::refcount::ll_retain(src as *mut RcHeader);
    }

    let oom = force_oom();
    // Every heap string slot the thread can still serve. The flag refuses
    // the pool a fresh block; a block this thread already holds would
    // serve the key's crossing in silence, and whether it holds one
    // depends on what ran on this thread before.
    let mut fillers: Vec<*mut crate::string::LLString> = Vec::new();
    loop {
        let s =
            unsafe { crate::string::ll_string_new(context_ptr, MemoryCategory::GcHeap, b"filler") };
        if s.is_null() {
            break;
        }

        fillers.push(s);
    }

    let copy = unsafe {
        separate(
            src,
            MemoryCategory::GcHeap,
            arena_ptr,
            CopyReason::Duplication,
        )
    };

    drop(oom);
    for s in fillers {
        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
    }

    assert!(
        copy.is_null(),
        "the copy was meant to be refused and was not"
    );

    unsafe {
        assert_eq!(
            crate::refcount::mutator_flags(d as *const RcHeader) & crate::refcount::IS_ESCAPEE,
            0,
            "the refused copy left the child counted as an escapee"
        );
        crate::refcount::ll_release(src as *mut RcHeader);
        assert!(ll_release(warm as *mut RcHeader));
        crate::object::ll_entity_die(warm as *mut RcHeader);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
