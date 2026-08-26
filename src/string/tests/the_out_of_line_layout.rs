//! The second layout keeps `len` at +8 and `hash` at +16, so either
//! can be read without deciding which layout this is. A string the
//! compiler proved single-owned is outside the COW rule, so an
//! append writes in place; an empty one has no payload at all and
//! has to answer without dereferencing `data`; and the two
//! categories it cannot live in are refused rather than redirected
//! to the heap.

use super::*;

/// The kind code alone decides where the bytes are read from, and
/// "read through `data`" is asserted as an address rather than as a
/// content match: the inline accessor over this entity would answer a
/// slice starting at `&(*s).data`, whose first eight bytes are the
/// payload pointer, and on a short payload that slice can still compare
/// equal to nothing anyone checked.
#[test]
fn an_out_of_line_string_is_read_through_data_by_its_kind_alone() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"grows", 0) };
    assert!(!s.is_null());

    let flags = unsafe { crate::refcount::mutator_flags(s as *const RcHeader) };
    assert_eq!(
        flags & ENTITY_KIND_MASK,
        EntityKind::StringDynamic.to_flags(),
        "the factory stamps the code that says where the bytes are"
    );

    let bytes = unsafe { string_bytes(s as *const LLString) };
    assert_eq!(
        bytes.as_ptr(),
        unsafe { (*s).data },
        "the slice starts at the payload, not inside the entity"
    );
    assert_eq!(bytes, b"grows");

    unsafe {
        assert!(crate::refcount::ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// The second layout's offsets, and the half of them it shares with
/// the first: `len` at +8 and `hash` at +16 in both, which is what
/// lets either be read without deciding which layout this is.
#[test]
fn the_dynamic_layout_shares_the_offsets_that_matter() {
    assert_eq!(std::mem::offset_of!(LLStringDynamic, rc), 0);
    assert_eq!(
        std::mem::offset_of!(LLStringDynamic, len),
        std::mem::offset_of!(LLString, len)
    );
    assert_eq!(std::mem::offset_of!(LLStringDynamic, capacity), 12);
    assert_eq!(
        std::mem::offset_of!(LLStringDynamic, hash),
        std::mem::offset_of!(LLString, hash)
    );
    assert_eq!(std::mem::offset_of!(LLStringDynamic, data), 24);
    assert_eq!(size_of::<LLStringDynamic>(), 32);
}

#[test]
fn a_dynamic_heap_string_holds_its_bytes_out_of_line() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"grows", 0) };
    assert!(!s.is_null());

    assert_eq!(
        unsafe { crate::refcount::entity_flags(s) } & ENTITY_KIND_MASK,
        EntityKind::StringDynamic.to_flags(),
        "the layout is the kind code"
    );
    assert!(
        crate::refcount::is_string(unsafe { crate::refcount::entity_flags(s) }),
        "and both codes still answer the one string test"
    );
    assert_eq!(
        unsafe { crate::refcount::entity_flags(s) } & COW,
        0,
        "and this factory builds the proved-single-owner form, which \
         is the non-COW one"
    );
    assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"grows");
    assert_eq!(
        unsafe { string_bytes(s as *const LLString) },
        b"grows",
        "and the layout-agnostic accessor agrees"
    );
    assert!(
        unsafe { (*s).capacity } >= 5,
        "the payload is allocated with its own capacity"
    );
    assert!(
        !unsafe { (*s).data }.is_null() && unsafe { (*s).data } as usize != s as usize + 24,
        "out of line, not inline"
    );

    unsafe {
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// A dynamic string is outside the COW rule, so an append writes in
/// place with no sharing test — even with a second holder, which for
/// an inline string would force a copy.
#[test]
fn an_append_grows_in_place_and_drops_the_cached_hash() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"one", 0) };
    let hash = unsafe { LLString::hash(s as *mut LLString) };
    assert_ne!(hash, 0);
    unsafe { crate::refcount::ll_retain(s as *mut RcHeader) };

    assert!(unsafe { ll_string_append(&mut ctx, s, b"-two") });
    assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"one-two");
    assert_eq!(unsafe { (*s).len }, 7);
    assert_eq!(
        unsafe { (*s).hash },
        0,
        "the old hash is not the new bytes'"
    );
    assert_eq!(
        unsafe { LLString::hash(s as *mut LLString) },
        hash_bytes(b"one-two"),
        "and recomputing gives the new content's — asserting merely \
         that it differs would pass on a hash of the payload address"
    );
    assert_ne!(hash_bytes(b"one-two"), hash);

    // Growth past the initial capacity: the payload may move, the
    // entity may not.
    let address = s as usize;
    let long = vec![b'x'; 4096];
    assert!(unsafe { ll_string_append(&mut ctx, s, &long) });
    assert_eq!(unsafe { (*s).len } as usize, 7 + 4096);
    assert_eq!(unsafe { LLStringDynamic::bytes(s) }[..7], *b"one-two");
    assert_eq!(s as usize, address, "the entity never moves");

    unsafe {
        assert!(!ll_release(s as *mut RcHeader));
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// The accumulator: `$s = ""` and then appends. An empty dynamic
/// string has no payload at all, so every read of it has to answer
/// without dereferencing `data` — `slice::from_raw_parts` requires a
/// non-null pointer even for a zero-length slice.
#[test]
fn an_empty_dynamic_string_has_no_payload_and_still_reads() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"", 0) };
    assert!(!s.is_null());
    assert!(unsafe { (*s).data }.is_null(), "nothing was allocated");
    assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"");
    assert_eq!(unsafe { string_bytes(s as *const LLString) }, b"");
    assert_eq!(
        unsafe { LLString::hash(s as *mut LLString) },
        hash_bytes(b"")
    );

    assert!(unsafe { ll_string_append(&mut ctx, s, b"first") });
    assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"first");

    // With a hint, the payload is there from the start — that is what
    // the hint is for, and the empty case is where it matters most.
    let hinted = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"", 4096) };
    assert!(!unsafe { (*hinted).data }.is_null());
    assert!(unsafe { (*hinted).capacity } >= 4096);
    assert_eq!(unsafe { (*hinted).len }, 0);

    unsafe {
        for p in [s as *mut RcHeader, hinted as *mut RcHeader] {
            assert!(ll_release(p));
            crate::object::ll_entity_die(p);
        }
    }

    arena.reset(|_| {});
}

/// The two categories a dynamic string may not have are refused, not
/// redirected. A debug-only check would vanish in release into the
/// heap arm and put an immortal-flagged entity in a GC entity block:
/// walked by the census, never released, pinned for the life of the
/// process.
#[test]
fn a_dynamic_string_refuses_the_categories_it_cannot_live_in() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    for category in [MemoryCategory::Immortal, MemoryCategory::LongLived] {
        assert!(
            unsafe { ll_string_new_dynamic(&mut ctx, category, b"no", 0) }.is_null(),
            "the mutable layout is heap or arena only"
        );
    }

    arena.reset(|_| {});
}
