use super::*;
use crate::memory::arena::Arena;
use crate::refcount::{ENTITY_KIND_MASK, ll_release};

/// One allocation holds the header, `len` at +8, `hash` at +16 and
/// the bytes past the fixed fields, so the allocation is sized for
/// the content and the copy has to land there. A heap string dies by
/// its refcount and is walked as a leaf, a payload of bytes closing
/// no ring; an arena one is left to the reset and reaches no walk at
/// all, the enumerators skipping every block kind but the entity
/// heap's.
mod the_inline_layout {
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
    fn a_heap_string_is_a_cow_kind_1_entity_that_dies_by_refcount() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"hello") };
        assert!(!s.is_null());

        let rc = unsafe { &(*s).rc };
        assert_eq!(rc.refcount, 1);
        assert_eq!(rc.flags & ENTITY_KIND_MASK, EntityKind::String.to_flags());
        assert_ne!(
            rc.flags & COW,
            0,
            "the ordinary factory builds the COW form"
        );
        assert_eq!(rc.memory_category(), MemoryCategory::GcHeap);
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
            unsafe { (*s).rc.memory_category() },
            MemoryCategory::RequestArena
        );
        assert_eq!(unsafe { LLString::bytes(s) }, b"scoped");
        arena.reset(|_| {});
    }

    /// A heap string lands in an entity block, so the walker meets it.
    /// It must be counted as its own kind and contribute no edges: a
    /// string's payload is bytes, so it is a leaf and cannot close a ring
    /// (`walk::trace_entity`).
    #[test]
    fn the_walker_counts_a_heap_string_as_a_leaf() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let before = unsafe { crate::walk::heap_census() };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"walked") };
        let after = unsafe { crate::walk::heap_census() };

        let k = EntityKind::String as usize;
        assert_eq!(after.by_kind[k], before.by_kind[k] + 1);
        assert_eq!(after.edges, before.edges, "a string has no out-edges");

        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }

        let freed = unsafe { crate::walk::heap_census() };
        assert_eq!(freed.by_kind[k], before.by_kind[k], "and it goes away");
        arena.reset(|_| {});
    }
}

/// The field is computed once on demand and zero means "not
/// computed", so a string in a category a second thread can reach is
/// hashed before publication instead — two threads would race to
/// fill it. The hash is a function of the content alone, so both
/// layouts holding the same bytes answer alike, the empty string
/// included, and a copy starts unhashed: the write that separation
/// exists to serve is about to invalidate it.
mod the_cached_hash {
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

        // Poison the bytes: a second call that recomputed would notice.
        unsafe { (s.add(1) as *mut u8).write(b'L') };
        assert_eq!(unsafe { LLString::hash(s) }, first, "read from the cache");

        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }

        arena.reset(|_| {});
    }

    /// The lazy field's sentinel survives whatever the hash returns: the
    /// string side needs a hash that is never zero, and checking that it
    /// never is belongs to the hash module (`hash::tests`). What is
    /// checked here is the string's own reading of the field — a freshly
    /// hashed string reports a non-zero value, so the next read hits the
    /// cache instead of recomputing.
    #[test]
    fn a_hashed_string_never_reads_back_as_unhashed() {
        assert_ne!(hash_bytes(b"anything"), 0);
        assert_ne!(hash_bytes(&[]), 0);
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
        let dynamic =
            unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, content, 0) };
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
}

/// The flag, then the category, then the count, and the order is
/// what the branches are about: an immortal entity's count sits at 1
/// forever, so a count read first would call an interned name a sole
/// owner and rewrite a string the whole process shares. A long-lived
/// string separates for two reasons of its own — its count is
/// non-atomic while more than one request reaches it, and
/// `string_die` frees only `GcHeap`. The holder's whole composition
/// leaves exactly one holder on each side.
mod the_cow_rule_and_the_order_it_reads_in {
    use super::*;

    /// The four branches of the COW rule, in the order
    /// `rfc/model/values.md` fixes them. Each is checked through the
    /// generic barrier rather than through `separate`, because the order
    /// of the tests is the part that matters: a count read before the
    /// category would call an interned string a sole owner.
    #[test]
    fn a_sole_owner_in_the_heap_writes_in_place() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"mine") };
        let after = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, s as *mut RcHeader)
        };

        assert_eq!(after as usize, s as usize, "no copy for a lone holder");
        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }

        arena.reset(|_| {});
    }

    /// The reason the category is read before the count: retain and
    /// release return early on an immortal entity, so its count sits at 1
    /// forever. Read as "sole owner", that would rewrite an interned name
    /// shared by the whole process.
    #[test]
    fn an_immortal_string_separates_although_its_count_says_one() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let name = crate::intern::intern_str("Order") as *mut LLString;
        assert_eq!(unsafe { (*name).rc.refcount }, 1);

        let copy = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, name as *mut RcHeader)
        } as *mut LLString;
        assert_ne!(copy as usize, name as usize);
        assert_eq!(unsafe { LLString::bytes(copy) }, b"Order");
        assert_eq!(
            unsafe { (*copy).rc.memory_category() },
            MemoryCategory::GcHeap,
            "a heap holder gets a heap copy"
        );
        assert_eq!(
            unsafe { LLString::bytes(name) },
            b"Order",
            "the name is intact"
        );

        // The same interned name, written through an arena holder: the
        // copy is a bump in the arena the reset reclaims, not a heap
        // allocation with a release-at-reset record behind it.
        let local = unsafe {
            crate::object::ll_cow_separate(
                &mut ctx,
                MemoryCategory::RequestArena,
                name as *mut RcHeader,
            )
        } as *mut LLString;
        assert_eq!(
            unsafe { (*local).rc.memory_category() },
            MemoryCategory::RequestArena,
            "the holder's category decides, not the original's"
        );

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
        }

        arena.reset(|_| {});
    }

    /// A long-lived string separates although its count is maintained
    /// and may read as one. `values.md` justifies the category test with
    /// "the count is pinned", which is true of immortal and false here —
    /// `ll_retain` takes neither early return for a COW entity. The
    /// reasons that do hold: the count is non-atomic while the entity is
    /// reachable from more than one request, and `string_die` frees only
    /// `GcHeap`, so an in-place write would land somewhere nothing
    /// reclaims.
    #[test]
    fn a_long_lived_string_separates_although_its_count_is_real() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::LongLived, b"cached") };
        assert_eq!(unsafe { (*s).rc.refcount }, 1);
        unsafe { crate::refcount::ll_retain(s as *mut RcHeader) };
        assert_eq!(
            unsafe { (*s).rc.refcount },
            2,
            "counted, unlike an immortal entity"
        );
        unsafe { assert!(!ll_release(s as *mut RcHeader)) };

        let copy = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, s as *mut RcHeader)
        } as *mut LLString;
        assert_ne!(copy as usize, s as usize, "sole holder and still a copy");
        assert_eq!(unsafe { LLString::bytes(copy) }, b"cached");
        assert_eq!(
            unsafe { (*copy).rc.memory_category() },
            MemoryCategory::GcHeap
        );

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
        }

        arena.reset(|_| {});
    }

    /// The two branches no entity in the crate can currently reach, so
    /// they are checked on the predicate rather than on a fabricated
    /// header: flipping `COW` on a live string would change its value
    /// semantics under holders that already have it and, under
    /// `rc-walk`, race the collector's byte stores.
    ///
    /// **`IS_ESCAPEE`**: not an arm of the rule. The store barrier copies
    /// a COW value out of the arena rather than counting an escape into
    /// it, so the combination cannot occur (`dev/DECISIONS.md`,
    /// 2026-08-04); it is asserted where it is produced,
    /// `barrier::tests::a_cow_value_leaving_the_arena_is_copied_rather_than_counted`.
    ///
    /// **`COW = 0`**: the form the compiler allocates for a proved single
    /// owner, which `ll_string_new_dynamic` builds and no lowering emits
    /// yet.
    #[test]
    fn the_rule_reads_the_flag_before_the_category_and_the_count() {
        use crate::refcount::{IS_ESCAPEE, cow_separation_needed};
        let cow = COW | MemoryCategory::GcHeap as u32;

        assert!(!cow_separation_needed(cow, 1), "sole owner writes in place");
        assert!(cow_separation_needed(cow, 2), "a second holder copies");
        assert!(
            cow_separation_needed(COW | MemoryCategory::Immortal as u32, 1),
            "immortal copies at any count"
        );
        assert!(
            cow_separation_needed(COW | MemoryCategory::LongLived as u32, 1),
            "long-lived copies at any count"
        );

        // Not COW: outside the rule entirely, whatever else is set.
        for flags in [
            MemoryCategory::GcHeap as u32,
            MemoryCategory::Immortal as u32,
            MemoryCategory::RequestArena as u32 | IS_ESCAPEE,
        ] {
            assert!(
                !cow_separation_needed(flags, 9),
                "a non-COW entity is never copied by the write barrier"
            );
        }
    }

    /// The whole composition a holder performs, with the counts checked
    /// at the end — separation, the store that retains, the drop of what
    /// the slot displaced, and the release of the copy's creation
    /// reference. That last one is the step whose absence leaves the copy
    /// at two for one holder: not merely leaked, but reading as shared on
    /// every later write, so the value would separate forever.
    #[test]
    fn separating_then_storing_leaves_exactly_one_holder_on_each_side() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        // Two holders of one string: a slot, and the local that made it.
        let original = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"shared") };
        let mut slot: *mut RcHeader = std::ptr::null_mut();
        unsafe {
            assert!(crate::memory::barrier::store_ptr(
                &raw mut arena,
                MemoryCategory::GcHeap,
                &raw mut slot,
                original as *mut RcHeader,
            ));
        };

        assert_eq!(unsafe { (*original).rc.refcount }, 2);

        let copy =
            unsafe { crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, slot) };
        assert_ne!(copy as usize, original as usize, "two holders, so a copy");
        unsafe {
            assert!(crate::memory::barrier::store_ptr(
                &raw mut arena,
                MemoryCategory::GcHeap,
                &raw mut slot,
                copy,
            ));
            assert!(!ll_release(copy), "the creation reference, spent");
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, original as *mut RcHeader);
        }

        assert_eq!(unsafe { (*copy).refcount }, 1, "the slot alone holds it");
        assert_eq!(
            unsafe { (*original).rc.refcount },
            1,
            "and the local alone holds the original"
        );
        assert_eq!(unsafe { LLString::bytes(copy as *mut LLString) }, b"shared");

        unsafe {
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, slot);
            assert!(ll_release(original as *mut RcHeader));
            crate::object::ll_entity_die(original as *mut RcHeader);
        }

        arena.reset(|_| {});
    }
}

/// The second layout keeps `len` at +8 and `hash` at +16, so either
/// can be read without deciding which layout this is. A string the
/// compiler proved single-owned is outside the COW rule, so an
/// append writes in place; an empty one has no payload at all and
/// has to answer without dereferencing `data`; and the two
/// categories it cannot live in are refused rather than redirected
/// to the heap.
mod the_out_of_line_layout {
    use super::*;

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

        let rc = unsafe { &(*s).rc };
        assert_eq!(rc.flags & ENTITY_KIND_MASK, EntityKind::String.to_flags());
        assert_ne!(
            rc.flags & STRING_OUT_OF_LINE,
            0,
            "the layout is its own bit"
        );
        assert_eq!(
            rc.flags & COW,
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
}

/// In the GC heap and the request arena, content past what the
/// category packs in one slot goes out of line and keeps `COW`, the
/// layout being a bit of its own: a string dynamic by size has the
/// semantics an inline one has, so a second holder forces a copy and
/// that copy reaches the size-choosing factory rather than the inline
/// one. The arena's limit is a whole block payload rather than a size
/// class. The other two categories answer otherwise and no test here
/// asks them: past the same limit a long-lived string is refused
/// outright and an immortal one keeps the inline layout in a run of
/// its own (`string::placement`).
mod the_layout_size_chooses {
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

        let s =
            unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, &vec![b'a'; last_inline]) };
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
}

/// A heap payload is a buffer-arena chunk that teardown gives back,
/// while an arena string leaves both halves to the reset. A survivor
/// takes its payload with it by the two routes the design fixes: an
/// in-block payload is copied, its block going back to the pool, and
/// an OS-direct run transfers, which is why nothing can refuse it.
/// An append loop moves its payload once, measured at one against
/// nine for the same 256 appends.
mod the_payload_and_who_frees_it {
    use super::*;

    /// An arena dynamic string takes its payload from the arena, so the
    /// reset reclaims both halves and teardown must not hand the payload
    /// to the long-lived free routine — a block of the wrong kind.
    #[test]
    fn an_arena_dynamic_string_leaves_both_halves_to_the_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s =
            unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::RequestArena, b"scoped", 0) };
        assert_eq!(
            unsafe { (*s).rc.memory_category() },
            MemoryCategory::RequestArena
        );
        assert!(unsafe { ll_string_append(&mut ctx, s, b" and grown") });
        assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"scoped and grown");

        let payload = unsafe { (*s).data };
        unsafe { string_die(s as *mut LLString) };
        // The payload is still the arena's, and still intact. Handing it
        // to the long-lived free routine instead would have written a
        // free-list link — `{ next, size }`, 16 bytes — over the front of
        // it, so reading the content back is what catches that.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(payload, 16) },
            b"scoped and grown",
            "an arena payload belongs to the reset, and teardown left it alone"
        );
        arena.reset(|_| {});
    }

    /// An accumulator built in the arena and stored into a heap holder:
    /// the entity survives the reset and its payload comes with it. An
    /// in-block payload is copied, because the block it sits in goes back
    /// to the pool.
    #[test]
    fn an_escaped_arena_string_carries_its_payload_through_the_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe {
            ll_string_new_dynamic(&mut ctx, MemoryCategory::RequestArena, b"accumulated", 0)
        };

        let arena_payload = unsafe { (*s).data };

        let mut heap_slot: *mut RcHeader = std::ptr::null_mut();
        unsafe {
            assert!(crate::memory::barrier::store_ptr(
                &raw mut arena,
                MemoryCategory::GcHeap,
                &raw mut heap_slot,
                s as *mut RcHeader,
            ));
            crate::promote::arena_reset_full(&raw mut arena);
        }

        let s = heap_slot as *mut LLStringDynamic;
        assert_eq!(
            unsafe { (*s).rc.memory_category() },
            MemoryCategory::GcHeap,
            "promoted"
        );
        assert_ne!(
            unsafe { (*s).data },
            arena_payload,
            "an in-block payload is copied: its block went back to the pool"
        );
        assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"accumulated");

        unsafe {
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, s as *mut RcHeader);
        }
    }

    /// The same, with a payload too large for a block. There the arena
    /// only owns an OS-direct run, so ownership transfers instead of
    /// being copied — the pointer does not move, nothing is allocated,
    /// and the reset therefore has no way to refuse.
    #[test]
    fn an_os_direct_payload_transfers_instead_of_being_copied() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let big = vec![b'z'; crate::memory::block_pool::BLOCK_PAYLOAD + 64];
        let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::RequestArena, &big, 0) };
        let os_direct = unsafe { (*s).data };
        assert!(unsafe { (*s).capacity } as usize > crate::memory::block_pool::BLOCK_PAYLOAD);

        let mut heap_slot: *mut RcHeader = std::ptr::null_mut();
        unsafe {
            assert!(crate::memory::barrier::store_ptr(
                &raw mut arena,
                MemoryCategory::GcHeap,
                &raw mut heap_slot,
                s as *mut RcHeader,
            ));
            crate::promote::arena_reset_full(&raw mut arena);
        }

        let s = heap_slot as *mut LLStringDynamic;
        assert_eq!(
            unsafe { (*s).data },
            os_direct,
            "the run is handed over, not copied"
        );
        assert_eq!(unsafe { LLStringDynamic::bytes(s) }, &big[..]);

        unsafe {
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, s as *mut RcHeader);
        }
    }

    /// Teardown returns a heap dynamic string's payload to the arena it
    /// came from, and this is the assertion that says so — deleting the
    /// payload half of `string_die` leaves every other test in this file
    /// green. The proof is the buffer arena's own: in critical mode a
    /// freed chunk goes on the block's free list and a fitting allocation
    /// finds it, so the same address coming back means the chunk was
    /// really returned.
    #[test]
    fn teardown_returns_a_heap_payload_to_the_buffer_arena() {
        use crate::memory::buffer::{PressureMode, set_pressure_mode};
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let content = vec![b'p'; 64];
        let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, &content, 0) };
        let payload = unsafe { (*s).data };
        let capacity = unsafe { (*s).capacity } as usize;
        assert!(!payload.is_null());

        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }

        set_pressure_mode(PressureMode::Critical);
        let (reused, _) = crate::memory::buffer_arena::with_buffer_arena(|a| a.alloc(capacity));
        set_pressure_mode(PressureMode::Plenty);
        assert_eq!(
            reused, payload,
            "the payload was not returned: the free list has no chunk of that size"
        );

        crate::memory::buffer_arena::with_buffer_arena(|a| unsafe { a.free(reused, capacity) });
        arena.reset(|_| {});
    }

    /// An append loop on a GC-heap string moves its payload once — for the
    /// first allocation — and never again.
    ///
    /// The buffer arena extends a payload that is still the last chunk it
    /// bumped, and this is what says that the string path reaches that,
    /// rather than only the arena's own unit test. Measured both ways on
    /// 2026-08-05: one move with the in-place path, nine without it, for
    /// the same 256 appends of 16 bytes.
    ///
    /// Nine moves are nine copies of everything written so far, which is
    /// the cost this exists to keep at one. The benchmark could not resolve
    /// the difference (`dev/BENCHMARKS.md`), so the count is the evidence,
    /// not the clock.
    #[test]
    fn an_append_loop_moves_its_payload_once() {
        let _g = crate::memory::block_pool::test_guard();
        let s =
            unsafe { ll_string_new_dynamic(std::ptr::null_mut(), MemoryCategory::GcHeap, b"", 0) };
        assert!(!s.is_null());

        let chunk = [b'x'; 16];
        let mut moves = 0;
        let mut last = unsafe { (*s).data };
        for _ in 0..256 {
            assert!(unsafe { ll_string_append(std::ptr::null_mut(), s, &chunk) });
            let now = unsafe { (*s).data };
            if now != last {
                moves += 1;
                last = now;
            }
        }

        assert_eq!(moves, 1, "the payload was reallocated instead of extended");
        assert_eq!(unsafe { (*s).len }, 256 * 16);
        assert!(
            unsafe { LLStringDynamic::bytes(s) }
                .iter()
                .all(|&b| b == b'x'),
            "extending in place must not disturb what was written"
        );

        unsafe {
            if ll_release(s as *mut RcHeader) {
                crate::object::ll_entity_die(s as *mut RcHeader);
            }
        }
    }
}

/// The 4 GiB gate every creation and growth path passes refuses
/// rather than truncating, and the string it refused is left exactly
/// as it was.
mod the_length_gate {
    use super::*;

    #[test]
    fn an_append_past_the_cap_is_refused_with_the_string_untouched() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"near", 0) };
        // Claim a length just under the cap without owning the bytes:
        // the gate is arithmetic on `len`, and this is the only way to
        // reach it without four gigabytes.
        unsafe { (*s).len = MAX_LEN as u32 - 1 };
        assert!(
            !unsafe { ll_string_append(&mut ctx, s, b"xx") },
            "4 GiB is a refusal, not a truncation"
        );
        assert_eq!(unsafe { (*s).len }, MAX_LEN as u32 - 1, "untouched");

        unsafe {
            (*s).len = 4;
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }

        arena.reset(|_| {});
    }

    #[test]
    fn content_past_the_cap_is_refused_rather_than_truncated() {
        assert!(fits(MAX_LEN));
        assert!(!fits(MAX_LEN + 1));
    }
}
