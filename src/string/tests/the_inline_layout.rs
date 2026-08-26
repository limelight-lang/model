//! One allocation holds the header, `len` at +8, `hash` at +16 and
//! the bytes past the fixed fields, so the allocation is sized for
//! the content and the copy has to land there. A heap string dies by
//! its refcount and is walked as a leaf, a payload of bytes closing
//! no ring; an arena one is left to the reset and reaches no walk at
//! all, the enumerators skipping every block kind but the entity
//! heap's.

use super::*;

/// The offsets the second layout has to match: a dynamic string
/// (`rfc/model/strings.md`) puts `len` and `hash` in the same places,
/// so reading either does not require deciding which layout this is.
/// Swapping the two fields still compiles and still passes every
/// other test here, which is why the contract is pinned.
#[test]
fn layout_matches_the_string_design() {
    assert_eq!(size_of::<RcHeader>(), 8, "header must stay 8 bytes");
    assert_eq!(std::mem::offset_of!(LLString, rc), 0);
    assert_eq!(std::mem::offset_of!(LLString, len), 8);
    let probe = LLString {
        rc: RcHeader::new(MemoryCategory::Immortal, COW),
        len: 0,
        hash: 0,
    };

    assert_eq!(
        std::mem::size_of_val(&probe.len),
        4,
        "len is 32-bit: the 4 GiB cap"
    );
    assert_eq!(
        std::mem::offset_of!(LLString, hash),
        16,
        "+12 stays free for the dynamic layout's capacity"
    );
    assert_eq!(size_of::<LLString>(), 24, "bytes start right after");
}

#[test]
fn a_heap_string_is_a_cow_kind_8_entity_that_dies_by_refcount() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"hello") };
    assert!(!s.is_null());

    assert_eq!(unsafe { crate::refcount::entity_refcount(s) }, 1);
    assert_eq!(
        unsafe { crate::refcount::entity_flags(s) } & ENTITY_KIND_MASK,
        EntityKind::String.to_flags()
    );
    assert_ne!(
        unsafe { crate::refcount::entity_flags(s) } & COW,
        0,
        "the ordinary factory builds the COW form"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_category(s) },
        MemoryCategory::GcHeap
    );
    assert_eq!(unsafe { LLString::bytes(s) }, b"hello");
    assert_eq!(unsafe { (*s).len }, 5);

    unsafe {
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// The bytes live past `size_of::<LLString>()`, so the allocation has
/// to be sized for them and the copy has to land there. A string one
/// byte longer than the fixed fields would pass a size-class check
/// either way; content is what catches a wrong base.
#[test]
fn bytes_are_inline_and_survive_a_second_string_landing_beside_them() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let long = vec![b'x'; 100];
    let a = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, &long) };
    let b = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"beside") };
    assert_eq!(unsafe { LLString::bytes(a) }, &long[..]);
    assert_eq!(unsafe { LLString::bytes(b) }, b"beside");
    unsafe {
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_entity_die(a as *mut RcHeader);
        assert!(ll_release(b as *mut RcHeader));
        crate::object::ll_entity_die(b as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// A string that lives in the request arena is reclaimed by the
/// reset, not by its own teardown: the same entity, a different
/// owner of its memory (`rfc/model/memory/arenas.md`).
#[test]
fn an_arena_string_is_left_to_the_reset() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::RequestArena, b"scoped") };
    assert_eq!(
        unsafe { crate::refcount::entity_category(s) },
        MemoryCategory::RequestArena
    );
    assert_eq!(unsafe { LLString::bytes(s) }, b"scoped");
    arena.reset(|_| {});
}

/// A heap string lands in an entity block, so the walker meets it.
/// It must be counted as its own kind and contribute no edges: a
/// string's payload is bytes, so it is a leaf and cannot close a ring
/// (`cells::trace_entity`).
#[test]
fn the_walker_counts_a_heap_string_as_a_leaf() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let before = unsafe { crate::cells::heap_census() };
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"walked") };
    let after = unsafe { crate::cells::heap_census() };

    // Both codes, because what this pins is that a string of any
    // representation is walked and has no out-edges, not which of the two
    // the factory chose here.
    let strings = |c: &crate::cells::Census| {
        c.by_kind[EntityKind::String as usize] + c.by_kind[EntityKind::StringDynamic as usize]
    };
    assert_eq!(strings(&after), strings(&before) + 1);
    assert_eq!(after.edges, before.edges, "a string has no out-edges");

    unsafe {
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    let freed = unsafe { crate::cells::heap_census() };
    assert_eq!(strings(&freed), strings(&before), "and it goes away");
    arena.reset(|_| {});
}
