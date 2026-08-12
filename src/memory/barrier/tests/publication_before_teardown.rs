//! The slot holds the new value before the displaced one's
//! `__destruct` runs, so user code that collects from inside it
//! cannot walk an edge the refcount has already given up.
//!
//! rc-trace only, for the second half of the scenario below: an
//! `rc-walk` build has no candidate buffer for the destructor's fire
//! point to reclaim from. The order itself is strategy-independent.

use super::*;

/// Audit C1: the displaced value's `__destruct` is user code and may
/// collect. If the slot still pointed at the value being torn down,
/// the collector would walk an edge the refcount has already given up.
///
/// The damage needs the owner to be garbage itself: then nothing
/// restores the subtracted count, the dying value goes white with it,
/// and `collect_white` frees it **while its own teardown is running** —
/// a free of memory the caller is still inside, followed by a second
/// free when teardown finishes. Publishing the slot first removes the
/// edge, so there is nothing to walk.
///
/// **The slot is read from inside the destructor**, rather than
/// inferred from what a collection there reclaims. That inference was
/// the original instrument and it stopped measuring on 2026-08-07,
/// when a fire point inside a teardown became a no-op
/// (`dev/DECISIONS.md`): the count is zero now whatever the slot
/// holds. Reading the slot states the property directly and needs no
/// collection to expose it.
#[test]
fn a_collecting_destructor_cannot_see_the_slot_it_is_being_removed_from() {
    use crate::class::ClassBuilder;
    use crate::gc::{ll_gc_collect_cycles, set_test_threshold};
    use crate::memory::context::LLContext;
    use crate::object::{Object, new_constructed};
    use crate::value::{Tag, Value};

    /// The owner's `next` slot, read from inside the destructor of
    /// the value being removed from it. Null until the destructor
    /// runs — `Value::null()` is not a legal reading of an
    /// unvisited slot, so the assertion below cannot pass by
    /// accident of the destructor never firing.
    static SEEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);
    /// The owner, so the destructor can find the slot it is being
    /// removed from.
    static OWNER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "C" fn read_the_slot(_obj: *mut Object) {
        let owner = OWNER.load(std::sync::atomic::Ordering::Relaxed) as *mut Object;
        let held = unsafe { Object::prop_at(owner, 16).read() };
        let entity = if held.is_refcounted() {
            held.entity_ptr() as usize
        } else {
            0
        };

        SEEN.store(entity, std::sync::atomic::Ordering::Relaxed);
        // The fire point a destructor may carry, which since
        // 2026-08-07 collects nothing from inside a teardown
        // (`dev/DECISIONS.md`). Kept here because this test exists
        // for what such a collection would have walked.
        assert_eq!(
            unsafe { ll_gc_collect_cycles() },
            0,
            "a collection fired from a destructor reclaims nothing"
        );
    }

    let _g = crate::memory::block_pool::test_guard();
    let owner_cls = ClassBuilder::new("C1Owner")
        .prop("next", true)
        .prop("mine", true)
        .build();
    let dying_cls = ClassBuilder::new("C1Dying")
        .destructor(read_the_slot as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    unsafe {
        let owner = new_constructed(&mut ctx, owner_cls, MemoryCategory::GcHeap);
        let old = new_constructed(&mut ctx, dying_cls, MemoryCategory::GcHeap);
        let next = Object::prop_at(owner, 16);
        let mine = Object::prop_at(owner, 32);
        OWNER.store(owner as usize, std::sync::atomic::Ordering::Relaxed);

        // owner --mine--> owner: a self-cycle, so the owner is garbage
        // held up only by its own edge.
        assert!(ref_store(
            &mut arena,
            owner as *mut RcHeader,
            mine,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, owner as *mut RcHeader),
        ));
        // owner --next--> old, then drop the creation reference: the
        // slot holds the only one left.
        assert!(ref_store(
            &mut arena,
            owner as *mut RcHeader,
            next,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, old as *mut RcHeader),
        ));
        assert!(!ll_release(old as *mut RcHeader));

        // Drop the owner's external reference too: now it is a
        // buffered candidate root, so the owner is exactly the shape
        // a collection would walk — through `next`, if the old value
        // were still visible there.
        set_test_threshold(usize::MAX); // arm nothing, fire only from the destructor
        assert!(!ll_release(owner as *mut RcHeader));

        // Displaces `old`: last reference gone, teardown runs, the
        // destructor collects from inside it.
        assert!(
            ref_store(
                &mut arena,
                owner as *mut RcHeader,
                next,
                old as *mut RcHeader,
                Value::null(),
            ),
            "the barrier refused the displacement this test is built on"
        );

        // The store barrier publishes before it drops, so by the time
        // `old`'s teardown runs the slot holds the new value. A
        // reading of `old` here is the edge still standing into an
        // object at refcount zero, which anything walking the owner —
        // a collection, another destructor — would follow.
        assert_eq!(
            SEEN.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the slot must be published before the displaced value's teardown"
        );
        set_test_threshold(crate::gc::CANDIDATE_THRESHOLD);
    }

    arena.reset(|_| {});
}
