//! The owner's category is a parameter rather than a header read, so
//! a headerless static block is a valid destination and still gets
//! the escape barrier. An arena reference entering a longer-lived
//! slot is recorded as an escapee, a COW value is copied instead
//! — it is value-like, and a copy holds no arena memory — a heap
//! reference entering an arena slot waits for the reset, and an
//! immortal value costs nothing at all.

use super::*;

/// The most ordinary string store in the language: `$o->name = $s`,
/// a heap object taking an arena string. A COW entity is value-like,
/// so the holder takes a **copy** in the heap rather than a hold on
/// arena memory. Before this, the store went down the escape counter
/// and overwrote a live holder count with a hold-count of one —
/// caught by a `debug_assert` in debug and silently wrong in release.
#[test]
fn a_cow_value_leaving_the_arena_is_copied_rather_than_counted() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext {
        arena: &raw mut arena,
    };

    let s = unsafe {
        crate::string::ll_string_new(&raw mut ctx, MemoryCategory::RequestArena, b"name")
    } as *mut RcHeader;
    let mut slot: *mut RcHeader = std::ptr::null_mut();

    assert!(unsafe { store_ptr(&raw mut arena, MemoryCategory::GcHeap, &mut slot, s) });

    assert_ne!(slot, s, "the heap slot must not hold arena memory");
    assert_eq!(
        unsafe { crate::object::header_category(slot) },
        MemoryCategory::GcHeap,
        "the copy lands where its holder lives"
    );
    assert_eq!(
        unsafe { crate::string::string_bytes(slot as *const crate::string::LLString) },
        b"name",
        "and it is the same value"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(slot) },
        1,
        "the slot is its only holder"
    );
    unsafe {
        assert_eq!(
            crate::refcount::entity_flags(s) & IS_ESCAPEE,
            0,
            "a COW entity never escapes"
        );
        assert_eq!(
            crate::refcount::entity_refcount(s),
            1,
            "the original keeps the count it had"
        );
    }

    let mut escapees = Vec::new();
    arena.reset_with(|_| {}, |e| escapees.push(e));
    assert!(escapees.is_empty(), "nothing was logged as an escapee");

    unsafe { drop_ref(MemoryCategory::GcHeap, slot) };
}

/// `owner_cat` is passed, not read from an owner header — so a
/// headerless destination (a static block, A6) is a valid store target
/// and still gets the escape barrier. A long-lived slot taking an arena
/// reference counts the escape exactly as a heap owner would, with no
/// owner entity anywhere.
#[test]
fn owner_cat_parameter_drives_the_escape_barrier_without_an_owner_header() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let obj = arena.alloc(16) as *mut RcHeader;
    unsafe { obj.write(entity(MemoryCategory::RequestArena)) };
    let mut slot: *mut RcHeader = std::ptr::null_mut();

    assert!(unsafe { store_ptr(&mut arena, MemoryCategory::LongLived, &mut slot, obj) });
    assert_ne!(
        unsafe { crate::refcount::entity_flags(obj) } & IS_ESCAPEE,
        0,
        "arena ref escaped into a long-lived slot"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(obj) },
        1,
        "one holder, counted in the escapee"
    );

    let mut escapees = Vec::new();
    arena.reset_with(|_| {}, |e| escapees.push(e));
    assert_eq!(
        escapees,
        vec![obj],
        "the escapee itself, no slot dereferenced"
    );
}

#[test]
fn arena_ref_into_heap_owner_is_recorded_as_an_escapee() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut owner = Holder::new(MemoryCategory::GcHeap);

    // The escapee lives in real arena memory, as it would in life.
    let obj = arena.alloc(16) as *mut RcHeader;
    unsafe { obj.write(entity(MemoryCategory::RequestArena)) };

    unsafe { owner.store(&mut arena, obj) };
    // The escape is counted in the entity itself (the IS_ESCAPEE
    // hold-count), not by remembering the holder's slot.
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(obj) },
        1,
        "one heap holder"
    );
    assert_ne!(
        unsafe { crate::refcount::entity_flags(obj) } & crate::refcount::IS_ESCAPEE,
        0,
        "marked as an escapee"
    );

    // Reset sees the escapee entity directly — no slot is dereferenced.
    let mut escapees = Vec::new();
    arena.reset_with(|_| {}, |e| escapees.push(e));
    assert_eq!(escapees, vec![obj], "the escapee itself, not its slot");
}

#[test]
fn heap_ref_into_arena_owner_defers_all_releases_to_reset() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut owner = Holder::new(MemoryCategory::RequestArena);
    let mut a = entity(MemoryCategory::GcHeap);
    let mut b = entity(MemoryCategory::GcHeap);

    unsafe { owner.store(&mut arena, &mut a) };
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut a) }, 2);

    // Overwrite: A must NOT be released here — its log record owns
    // the release.
    unsafe { owner.store(&mut arena, &mut b) };
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(&raw mut a) },
        2,
        "no release on overwrite in an arena slot"
    );
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut b) }, 2);

    // Store A again: a second retain and a second log record.
    unsafe { owner.store(&mut arena, &mut a) };
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut a) }, 3);
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut b) }, 2);

    // Reset releases once per record: A twice, B once. Balanced.
    arena.reset(|_| {});
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut a) }, 1);
    assert_eq!(unsafe { crate::refcount::entity_refcount(&raw mut b) }, 1);
}

#[test]
fn immortal_values_touch_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut owner = Holder::new(MemoryCategory::GcHeap);
    let mut s = entity(MemoryCategory::Immortal);

    unsafe { owner.store(&mut arena, &mut s) };
    assert_eq!(
        unsafe { crate::refcount::entity_refcount(&raw mut s) },
        1,
        "immortals are never counted"
    );
    assert_eq!(owner.entity_ptr(), &mut s as *mut _);

    let mut escapes = 0;
    arena.reset_with(|_| {}, |_| escapes += 1);
    assert_eq!(escapes, 0, "no logs for immortal stores");
}
