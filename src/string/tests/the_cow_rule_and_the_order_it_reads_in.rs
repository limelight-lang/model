//! The flag, then the category, then the count, and the order is
//! what the branches are about: an immortal entity's count sits at 1
//! forever, so a count read first would call an interned name a sole
//! owner and rewrite a string the whole process shares. A long-lived
//! string separates for two reasons of its own — its count is
//! non-atomic while more than one request reaches it, and
//! `string_die` frees only `GcHeap`. The holder's whole composition
//! leaves exactly one holder on each side.

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
/// semantics under holders that already have it, and race a
/// collector's byte stores into the same word.
///
/// **`IS_ESCAPEE`**: not an arm of the rule. The store barrier copies
/// a COW value out of the arena rather than counting an escape into
/// it, so the combination cannot occur (`dev/DECISIONS.md`, "a COW
/// value is copied out of the arena, and the store barrier can say
/// no"); it is asserted where it is produced,
/// `barrier::tests::what_crossing_a_category_boundary_costs::a_cow_value_leaving_the_arena_is_copied_rather_than_counted`.
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

    let copy = unsafe { crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, slot) };
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
