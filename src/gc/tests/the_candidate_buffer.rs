//! A buffered entity that dies has to leave the buffer, or the next
//! collection traces freed memory as a root — a duty the teardown
//! doors owe, and one the drain owes for a nested array it tears
//! down itself. Buffering is deduplicated by a flag, a refused entry
//! leaves the entity unmarked rather than permanently unbufferable,
//! and `swap_remove` moves a candidate's recorded position with it.

use super::*;

#[test]
fn buffering_is_deduplicated_and_death_forgets_the_candidate() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = node_class();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        crate::refcount::ll_retain(a as *mut RcHeader);
        crate::refcount::ll_retain(a as *mut RcHeader); // rc=3
        assert!(!ll_release(a as *mut RcHeader)); // buffered
        assert!(!ll_release(a as *mut RcHeader)); // deduplicated
    }

    let buffered = unsafe { &*candidate_buffer() }
        .iter()
        .filter(|&&p| p == a as *mut RcHeader)
        .count();

    assert_eq!(buffered, 1, "one buffer entry per object");

    // The last reference dies through plain RC: the candidate must
    // be forgotten, and a later collection must not touch freed
    // memory.
    unsafe {
        assert!(ll_release(a as *mut RcHeader));
        crate::object::ll_object_die(a);
    }

    assert_eq!(unsafe { collect_cycles() }, 0);
    arena.reset(|_| {});
}

/// A buffer that cannot grow refuses the candidate instead of taking
/// the process down with it. The entity must come out of that
/// unmarked — a buffered bit with no entry behind it would make the
/// object permanently unbufferable and, worse, make `forget_candidate`
/// hunt for something that was never there.
#[test]
fn a_refused_candidate_is_left_unmarked_and_arms_a_collection() {
    // `FORCE_BUFFER_REFUSAL` is process-global, so this has to hold
    // the test lock like any other fault injection here.
    let _g = crate::memory::block_pool::test_guard();
    use std::sync::atomic::Ordering;

    let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
    let p = &mut e as *mut RcHeader;
    COLLECT_PENDING.with(|f| f.set(false));

    FORCE_BUFFER_REFUSAL.store(true, Ordering::Relaxed);
    unsafe { buffer_candidate(p) };
    FORCE_BUFFER_REFUSAL.store(false, Ordering::Relaxed);

    assert!(
        unsafe { (*candidate_buffer()).is_empty() },
        "nothing was recorded"
    );
    assert_eq!(
        e.flags & CYCLE_COLLECTOR_BUFFERED,
        0,
        "and nothing was claimed"
    );
    assert!(COLLECT_PENDING.with(|f| f.get()), "a refusal arms instead");

    // Still bufferable once there is room again.
    unsafe { buffer_candidate(p) };
    assert_eq!(unsafe { (*candidate_buffer()).len() }, 1);
    unsafe { forget_candidate(p) };
    COLLECT_PENDING.with(|f| f.set(false));
}

/// `swap_remove` moves the tail candidate, so its recorded position
/// has to move with it. A stale one cannot corrupt the buffer — the
/// slot is checked before removal and a mismatch falls back to the
/// scan — so this asserts the position itself, not just the outcome.
#[test]
fn forgetting_a_candidate_keeps_the_moved_one_findable() {
    let mut h: Vec<RcHeader> = (0..4)
        .map(|_| RcHeader::new(MemoryCategory::GcHeap, 0))
        .collect();
    let p: Vec<*mut RcHeader> = h.iter_mut().map(|e| e as *mut RcHeader).collect();
    let buffer = || unsafe { (*candidate_buffer()).clone() };

    unsafe {
        for &e in &p {
            buffer_candidate(e);
        }

        assert_eq!(buffer(), p, "buffered in order");

        // Removes index 1 and moves p[3] into it.
        forget_candidate(p[1]);
        assert_eq!(buffer(), vec![p[0], p[3], p[2]]);
        assert_eq!(
            decode_index(p[3]),
            Some(1),
            "the moved candidate knows where it is"
        );
        assert_eq!(
            decode_index(p[1]),
            None,
            "the removed one no longer claims a slot"
        );

        forget_candidate(p[3]);
        assert_eq!(buffer(), vec![p[0], p[2]], "the moved candidate was found");

        forget_candidate(p[0]);
        forget_candidate(p[2]);
        assert!(buffer().is_empty());
        assert!(h.iter().all(|e| e.flags & CYCLE_COLLECTOR_BUFFERED == 0));
    }
}

/// An array that dies through plain refcounting leaves the candidate
/// buffer on the way out. The duty used to live inside
/// `ll_default_dispose`, which no array ever runs, so a buffered
/// array would die leaving its pointer behind and the next
/// collection would trace freed memory as a root.
///
/// Seen failing under Miri on the read through the stale root; under
/// plain `cargo test` the reused slot answers plausibly and the
/// assertion below is what catches it.
#[test]
fn a_dying_array_forgets_its_candidacy() {
    use crate::array::entity::ll_array_new;
    use crate::refcount::ll_retain;
    let _g = crate::memory::block_pool::test_guard();

    let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    unsafe {
        ll_retain(a as *mut RcHeader);
        assert!(!ll_release(a as *mut RcHeader));
        assert!(
            (*candidate_buffer()).contains(&(a as *mut RcHeader)),
            "the non-zero decrement buffered it"
        );

        assert!(ll_release(a as *mut RcHeader), "the last reference");
        crate::object::ll_entity_die(a as *mut RcHeader);
    }

    assert!(
        !unsafe { (*candidate_buffer()).contains(&(a as *mut RcHeader)) },
        "the buffer kept a root pointing at freed memory"
    );
    assert_eq!(unsafe { collect_cycles() }, 0);
}

/// The same duty, one level down and owed by a different party. A
/// nested array is torn down by the drain inside
/// `array::entity::array_die`, never by `ll_entity_die`, so the
/// door's candidate-forget does not run for it and the drain owes it
/// instead. Left out, the buffer keeps a root into freed memory —
/// the state the door's duty was added to prevent.
///
/// Seen failing on the candidacy assertion with the drain's
/// `leave_the_candidate_buffer` call removed.
#[test]
fn a_nested_array_forgets_its_candidacy_when_the_drain_takes_it() {
    use crate::array::entity::ll_array_new;
    use crate::array::table::Key;
    use crate::refcount::ll_retain;
    use crate::value::Tag;
    let _g = crate::memory::block_pool::test_guard();

    let outer = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let inner = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    unsafe {
        // A non-zero decrement is what buffers the inner array; the
        // entry that follows takes its creation reference, so the
        // count is one and the entry backs it.
        ll_retain(inner as *mut RcHeader);
        assert!(!ll_release(inner as *mut RcHeader));
        assert!(
            (*candidate_buffer()).contains(&(inner as *mut RcHeader)),
            "the non-zero decrement buffered the inner array"
        );
        crate::array::testing::insert(
            outer,
            Key::Int(0),
            Value::entity(Tag::Array, inner as *mut RcHeader),
        );

        assert!(ll_release(outer as *mut RcHeader), "the last reference");
        crate::object::ll_entity_die(outer as *mut RcHeader);
    }

    assert!(
        !unsafe { (*candidate_buffer()).contains(&(inner as *mut RcHeader)) },
        "the buffer kept a root pointing at freed memory"
    );
    assert_eq!(unsafe { collect_cycles() }, 0);
}
