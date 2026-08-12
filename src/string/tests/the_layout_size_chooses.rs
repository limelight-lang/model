//! In the GC heap and the request arena, content past what the
//! category packs in one slot goes out of line and keeps `COW`, the
//! layout being a bit of its own: a string dynamic by size has the
//! semantics an inline one has, so a second holder forces a copy and
//! that copy reaches the size-choosing factory rather than the inline
//! one. The arena's limit is a whole block payload rather than a size
//! class, while the long-lived heap shares the GC heap's. The other two
//! categories answer otherwise past their own limit, each for its own
//! reason: a long-lived string is refused
//! outright, nothing there being able to free a payload, and an
//! immortal one keeps the inline layout in a run of its own
//! (`string::placement`).

use super::*;

/// Past what the heap's size classes pack, a string is built out of
/// line and **stays copy-on-write**: the layout is a bit of its own,
/// so a string dynamic by size keeps the semantics an inline one has
/// (`rfc/model/memory/large-entities.md`).
#[test]
fn a_heap_string_past_the_size_class_is_out_of_line_and_still_cow() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let big = vec![b'x'; crate::memory::heap::MAX_SMALL];
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, &big) };
    assert!(!s.is_null(), "an oversize string is served, not refused");

    let flags = unsafe { crate::refcount::header_flags(s as *const RcHeader) };
    assert_ne!(
        flags & crate::refcount::STRING_OUT_OF_LINE,
        0,
        "the bytes did not fit one slot, so they are out of line"
    );
    assert_ne!(flags & COW, 0, "and it is copy-on-write all the same");
    assert_eq!(unsafe { string_bytes(s) }, &big[..]);

    unsafe {
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// Where the choice happens, pinned from both sides: the largest
/// content one slot holds stays inline, and one byte more does not.
/// A field added to `LLString` moves that line, and no other test
/// would notice.
#[test]
fn the_layout_switches_at_the_slot_limit_and_not_a_byte_earlier() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    // From the bound the factory compares against, not from the size
    // class that happens to equal it today.
    let last_inline =
        crate::memory::routing::slot_limit(MemoryCategory::GcHeap) - size_of::<LLString>();

    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, &vec![b'a'; last_inline]) };
    assert!(!s.is_null());
    assert_eq!(
        unsafe { crate::refcount::header_flags(s as *const RcHeader) }
            & crate::refcount::STRING_OUT_OF_LINE,
        0,
        "exactly one slot's worth stays inline"
    );
    let big = unsafe {
        ll_string_new(
            &mut ctx,
            MemoryCategory::GcHeap,
            &vec![b'a'; last_inline + 1],
        )
    };

    assert!(!big.is_null());
    assert_ne!(
        unsafe { crate::refcount::header_flags(big as *const RcHeader) }
            & crate::refcount::STRING_OUT_OF_LINE,
        0,
        "and one byte more does not"
    );

    unsafe {
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
        assert!(ll_release(big as *mut RcHeader));
        crate::object::ll_entity_die(big as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// The clause the layout split exists for: a second holder forces a
/// copy, and the copy is oversize too, so separation reaches the
/// size-choosing factory rather than the inline one.
#[test]
fn a_shared_oversize_string_separates_on_write() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let big = vec![b'y'; crate::memory::heap::MAX_SMALL * 2];
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, &big) };
    assert!(!s.is_null());
    unsafe { crate::refcount::ll_retain(s as *mut RcHeader) };

    let copy = unsafe {
        crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, s as *mut RcHeader)
    };

    assert!(!copy.is_null());
    assert_ne!(copy as usize, s as usize, "a shared COW string separates");
    let copy_flags = unsafe { crate::refcount::header_flags(copy) };
    assert_ne!(
        copy_flags & crate::refcount::STRING_OUT_OF_LINE,
        0,
        "the copy is as oversize as the original"
    );
    assert_eq!(unsafe { string_bytes(copy as *const LLString) }, &big[..]);
    assert_eq!(
        unsafe { string_bytes(s) },
        &big[..],
        "and the other holder still reads the original"
    );

    unsafe {
        assert!(ll_release(copy));
        crate::object::ll_entity_die(copy);
        assert!(!ll_release(s as *mut RcHeader));
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// The arena's limit is a whole block payload rather than a size
/// class, and past it the same choice is made — with the counting a
/// COW arena entity gets, which the non-COW dynamic form does not.
#[test]
fn an_arena_string_past_a_block_payload_is_out_of_line_and_counted() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let big = vec![b'q'; crate::memory::block_pool::BLOCK_PAYLOAD];
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::RequestArena, &big) };
    assert!(!s.is_null(), "an oversize arena string is served");

    let flags = unsafe { crate::refcount::header_flags(s as *const RcHeader) };
    assert_ne!(flags & crate::refcount::STRING_OUT_OF_LINE, 0);
    assert_ne!(flags & COW, 0);
    assert_eq!(unsafe { string_bytes(s) }, &big[..]);

    unsafe {
        crate::refcount::ll_retain(s as *mut RcHeader);
        assert_eq!(
            crate::refcount::header_refcount(s as *mut RcHeader),
            2,
            "a COW arena string is counted, unlike the non-COW form, \
             whose retain is a no-op"
        );
        // Both verdicts are false whatever the count does: an arena
        // entity is reclaimed by the reset, so no caller tears it down.
        assert!(!ll_release(s as *mut RcHeader));
        assert!(!ll_release(s as *mut RcHeader));
        assert_eq!(crate::refcount::header_refcount(s as *mut RcHeader), 0);
        string_die(s as *mut LLString);
    }

    arena.reset(|_| {});
}

/// A by-size arena string escaping into a longer-lived holder is a
/// COW entity, so the barrier copies it out rather than counting an
/// escape — and the copy is oversize too, so it lands out of line in
/// the heap.
#[test]
fn an_escaping_oversize_arena_string_is_copied_into_the_heap() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let big = vec![b'z'; crate::memory::block_pool::BLOCK_PAYLOAD];
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::RequestArena, &big) };
    assert!(!s.is_null());

    let mut heap_slot: *mut RcHeader = std::ptr::null_mut();
    unsafe {
        assert!(crate::memory::barrier::store_ptr(
            &raw mut arena,
            MemoryCategory::GcHeap,
            &raw mut heap_slot,
            s as *mut RcHeader,
        ));
    }

    assert_ne!(
        heap_slot as usize, s as usize,
        "a COW entity is copied out, never held"
    );
    let copy_flags = unsafe { crate::refcount::header_flags(heap_slot) };
    assert_ne!(copy_flags & crate::refcount::STRING_OUT_OF_LINE, 0);
    assert_ne!(copy_flags & COW, 0);
    assert_eq!(
        unsafe { crate::object::header_category(heap_slot) },
        MemoryCategory::GcHeap
    );
    assert_eq!(
        unsafe { string_bytes(heap_slot as *const LLString) },
        &big[..]
    );

    unsafe {
        crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, heap_slot);
        crate::promote::arena_reset_full(&raw mut arena);
    }
}

/// The long-lived heap has no payload machinery — `string_die`
/// reclaims the GC heap alone — so past one slot no layout is left
/// and the factory refuses. It refuses rather than leaving the size
/// to the allocator, which serves a slot that large instead of
/// refusing one, and the payload would then have no owner at all.
/// Both factories are asked, because a single answer for the two is
/// what `placement` exists for: they decided this separately once and
/// disagreed within a day.
///
/// One slot's worth is served first through each factory, so a
/// `placement` that refused the category outright fails here rather
/// than passes. And the rule itself is read, not only its effect: a
/// null from a factory says a refusal happened, never where — with
/// debug assertions off, an out-of-line answer for this category
/// reaches `body_ensure`, gets null, and reports the same null, an
/// entity slot leaked behind it.
#[test]
fn a_long_lived_string_past_the_slot_limit_is_refused_by_both_factories() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let last_inline =
        crate::memory::routing::slot_limit(MemoryCategory::LongLived) - size_of::<LLString>();

    let served = unsafe {
        ll_string_new(
            &mut ctx,
            MemoryCategory::LongLived,
            &vec![b'l'; last_inline],
        )
    };
    assert!(!served.is_null(), "one slot's worth is served");
    assert_eq!(
        unsafe { crate::refcount::header_flags(served as *const RcHeader) }
            & crate::refcount::STRING_OUT_OF_LINE,
        0,
        "and it is inline, the only layout this category has"
    );

    assert!(
        unsafe {
            ll_string_new(
                &mut ctx,
                MemoryCategory::LongLived,
                &vec![b'l'; last_inline + 1],
            )
        }
        .is_null(),
        "one byte more has nowhere to go and is refused"
    );
    assert!(
        unsafe { crate::string::new_uninit(&mut ctx, MemoryCategory::LongLived, last_inline + 1) }
            .is_null(),
        "and the assemble-in-place factory answers the same"
    );

    // The same slot's worth through that factory, because its only
    // production caller is `flatten` and no test drives that with this
    // category: a `new_uninit` carrying the blanket refusal
    // `ll_string_new_dynamic` has, one function away, would otherwise
    // pass everything.
    let reserved =
        unsafe { crate::string::new_uninit(&mut ctx, MemoryCategory::LongLived, last_inline) };
    assert!(!reserved.is_null(), "and serves one slot's worth");
    unsafe {
        std::ptr::write_bytes(reserved.bytes(), b'u', last_inline);
        let filled = crate::string::publish_uninit(reserved, MemoryCategory::LongLived);
        assert_eq!(string_bytes(filled).len(), last_inline);
    }

    // The rule itself, which no null can report: refused here rather
    // than answered out of line and refused one layer down.
    assert!(matches!(
        placement(
            MemoryCategory::LongLived,
            size_of::<LLString>() + last_inline + 1
        ),
        Placement::Refused
    ));

    arena.reset(|_| {});
}

/// The immortal region keeps the inline layout whole past the same
/// limit, in the block-aligned run `immortal_alloc` serves above one
/// block payload: an immortal string is never freed, so the payload
/// machinery would buy it nothing.
///
/// **The byte comparison is not a bound on the allocation.** It reads
/// back the addresses `init_at` wrote, so an overrunning write and an
/// overrunning read agree; a run sized short of its content is Miri's
/// to catch, and that the run is taken at all rather than the bump
/// region is `immortal::tests`'. What is pinned here is the choice
/// `placement` makes and the content surviving it.
///
/// What this test allocates it cannot give back: the region has no
/// free, by construction. One run per run of the suite.
#[test]
fn an_immortal_string_past_a_block_payload_keeps_the_inline_layout() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let big = vec![b'i'; crate::memory::block_pool::BLOCK_PAYLOAD];
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::Immortal, &big) };
    assert!(!s.is_null(), "an oversize immortal string is served");
    assert_eq!(
        unsafe { crate::refcount::header_flags(s as *const RcHeader) }
            & crate::refcount::STRING_OUT_OF_LINE,
        0,
        "the run holds the inline layout rather than a payload pointer"
    );
    assert_eq!(unsafe { string_bytes(s) }, &big[..]);

    arena.reset(|_| {});
}
