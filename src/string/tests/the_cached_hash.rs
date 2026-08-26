//! The field is computed once on demand and zero means "not
//! computed", so a string in a category a second thread can reach is
//! hashed before publication instead — two threads would race to
//! fill it. The hash is a function of the content alone, so both
//! layouts holding the same bytes answer alike, the empty string
//! included, and a copy starts unhashed: the write that separation
//! exists to serve is about to invalidate it.

use super::*;

#[test]
fn the_hash_is_computed_once_on_demand_and_never_zero() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"lazy") };
    assert_eq!(unsafe { (*s).hash }, 0, "not computed at allocation");

    let first = unsafe { LLString::hash(s) };
    assert_ne!(first, 0, "zero is the sentinel, never a value");
    assert_eq!(unsafe { (*s).hash }, first, "cached in the entity");

    // Poison the bytes: a second call that recomputed would notice. The
    // write lands at `size_of::<LLString>()`, which is the first byte
    // only in the inline layout — in the other it is the `data` pointer —
    // so the layout this assumes is asserted rather than assumed.
    assert_eq!(
        unsafe { crate::refcount::mutator_flags(s as *const RcHeader) }
            & crate::refcount::ENTITY_KIND_MASK,
        EntityKind::String.to_flags(),
        "the poisoning below writes at an inline string's first byte"
    );
    unsafe { (s.add(1) as *mut u8).write(b'L') };
    assert_eq!(unsafe { LLString::hash(s) }, first, "read from the cache");

    unsafe {
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// A string in a category a second thread can reach arrives already
/// hashed, so no reader ever takes the lazy branch's plain store.
/// The field is read directly rather than through `LLString::hash`,
/// which would compute one and hide the difference. The two
/// single-owner categories stay lazy, which is what makes this a
/// property of the category rather than a hash on every creation.
#[test]
fn a_string_two_threads_can_reach_is_hashed_at_creation() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    for category in [MemoryCategory::Immortal, MemoryCategory::LongLived] {
        let s = unsafe { ll_string_new(&mut ctx, category, b"shared") };
        assert_eq!(
            unsafe { (*s).hash },
            hash_bytes(b"shared"),
            "left to whichever thread reads first"
        );
    }

    for category in [MemoryCategory::GcHeap, MemoryCategory::RequestArena] {
        let s = unsafe { ll_string_new(&mut ctx, category, b"owned") };
        assert_eq!(
            unsafe { (*s).hash },
            0,
            "a single owner still hashes lazily"
        );
    }
}

/// The empty string is the one content on which the two layouts do not
/// merely differ in where the bytes live — the dynamic one has no
/// payload at all and returns its slice without reading `data`, which
/// is null. Both must still reach the same hash as each other and as
/// `hash_bytes` of no bytes.
///
/// It is also the content most likely to expose a lazy field that never
/// settles: the cached hash means "not computed" when it is zero, so a
/// hash function returning zero for the empty input would recompute on
/// every read forever. The remap in `hash::hash_bytes` is what makes
/// that unreachable rather than unlikely, and the second read below is
/// what would catch it.
#[test]
fn an_empty_string_hashes_alike_in_both_layouts_and_caches() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let inline = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"") };
    let dynamic = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"", 0) };
    assert!(unsafe { (*dynamic).data }.is_null(), "no payload at all");

    let from_inline = unsafe { LLString::hash(inline) };
    assert_eq!(from_inline, hash_bytes(b""));
    assert_eq!(from_inline, unsafe {
        LLString::hash(dynamic as *mut LLString)
    });

    assert_ne!(from_inline, 0, "zero would mean the field is not computed");

    // Computed once and kept: the field now reads back as itself rather
    // than as the sentinel.
    assert_ne!(unsafe { (*inline).hash }, 0);
    assert_eq!(unsafe { LLString::hash(inline) }, from_inline);

    unsafe {
        for p in [inline as *mut RcHeader, dynamic as *mut RcHeader] {
            if ll_release(p) {
                crate::object::ll_entity_die(p);
            }
        }
    }
}

/// The hash is a function of the content and of nothing else, so the
/// two layouts holding the same bytes hash the same. Computing it
/// through the inline accessor on a dynamic string would hash the
/// `data` field — an address — and this is the assertion that says so.
#[test]
fn the_two_layouts_hash_the_same_content_the_same() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let content = b"a string long enough to reach past the fixed fields";

    let inline = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, content) };
    let dynamic = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, content, 0) };
    assert_eq!(
        unsafe { LLString::hash(inline) },
        unsafe { LLString::hash(dynamic as *mut LLString) },
        "same bytes, same hash, whichever layout holds them"
    );
    assert_eq!(unsafe { LLString::hash(inline) }, hash_bytes(content));

    // Two dynamic strings with equal content agree as well — they
    // would not if the hash were taken over the payload pointer.
    let other = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, content, 0) };
    assert_ne!(unsafe { (*other).data }, unsafe { (*dynamic).data });
    assert_eq!(unsafe { LLString::hash(other as *mut LLString) }, unsafe {
        LLString::hash(dynamic as *mut LLString)
    });

    unsafe {
        for p in [
            inline as *mut RcHeader,
            dynamic as *mut RcHeader,
            other as *mut RcHeader,
        ] {
            assert!(ll_release(p));
            crate::object::ll_entity_die(p);
        }
    }

    arena.reset(|_| {});
}

/// The copy's hash starts unset even though its bytes are the
/// original's: the write that separation exists to serve is about to
/// invalidate it, and a carried hash that someone forgets to clear
/// would propagate into every later copy of that value — nothing
/// recomputes a non-zero one.
#[test]
fn a_copy_starts_without_a_hash() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"hashed") };
    let hash = unsafe { LLString::hash(s) };
    assert_ne!(hash, 0);
    unsafe { crate::refcount::ll_retain(s as *mut RcHeader) };

    let copy = unsafe {
        crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, s as *mut RcHeader)
    } as *mut LLString;
    assert_eq!(unsafe { (*copy).hash }, 0, "not carried over");
    assert_eq!(
        unsafe { LLString::hash(copy) },
        hash,
        "same bytes, so same value"
    );

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(!ll_release(s as *mut RcHeader));
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    arena.reset(|_| {});
}
