//! A pass that rewrites an entry of R keeps the deferred lane's mark
//! ([`REOFFERED_MARK`]): the root it names was read live before the turn that
//! re-offered it, and under `hold-by-generation` the mark is the one record
//! that it has outlived an epoch (`dev/plans/S65.md`, S65.33, the Sage's G2).

use super::*;

/// The entry `entity` holds in R, as stored.
fn entry_of(entity: *mut RcHeader) -> usize {
    let stored = candidate_entries();
    assert_eq!(stored.len(), 1, "one entry: {stored:x?}");
    assert_eq!(entry_entity(stored[0]), entity);
    stored[0]
}

/// A retirement pass over R keeps a root that did not die with its mark on.
#[test]
fn a_retirement_pass_keeps_the_lanes_mark() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");
    let mut root = candidate(2);
    let entity = &raw mut root;
    assert!(unsafe { !release(entity) });
    mark_as_reoffered(entity);

    unsafe { retire_candidates() };
    assert_eq!(
        entry_of(entity) & ENTRY_MARK_BITS,
        REOFFERED_MARK,
        "kept, the lane's mark on"
    );

    reset();
}

/// The close's marking for the deferred lane keeps the lane's mark beside its
/// own, and on a root it does not defer.
#[test]
fn the_marking_for_deferral_keeps_the_lanes_mark() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");
    let mut root = candidate(2);
    let entity = &raw mut root;
    assert!(unsafe { !release(entity) });
    mark_as_reoffered(entity);

    let mut batch = read_batch();
    assert_eq!(batch.mark_for_deferral(|_| true), 1);
    assert_eq!(
        entry_of(entity) & ENTRY_MARK_BITS,
        DEFERRED_MARK | REOFFERED_MARK
    );
    assert_eq!(batch.mark_for_deferral(|_| false), 0);
    assert_eq!(entry_of(entity) & ENTRY_MARK_BITS, REOFFERED_MARK);
    dispose_candidates(batch, 0);
    assert_eq!(
        entry_of(entity) & ENTRY_MARK_BITS,
        REOFFERED_MARK,
        "the close kept the root in R, the lane's mark on"
    );

    reset();
}

/// A root the close marks for the deferred lane stays in R when neither spare
/// cell holds a block for the lane's head, and keeps the lane's mark.
#[test]
fn a_refused_deferral_keeps_the_lanes_mark() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");
    let mut root = candidate(2);
    let entity = &raw mut root;
    assert!(unsafe { !release(entity) });
    mark_as_reoffered(entity);
    let mut batch = read_batch();
    assert_eq!(batch.mark_for_deferral(|_| true), 1);

    // Nothing to draw from: the cells spent by hand, the reserve drained.
    let mutator_state = unsafe { mutator_state_ref(mutator_state()) };
    loop {
        let spare = take_spare(mutator_state);
        if spare.is_null() {
            break;
        }
        crate::memory::gc_metadata::release(spare);
    }
    crate::memory::critical::drain_for_test();
    assert_eq!(spare_count(), 0);

    dispose_candidates(batch, 0);
    assert_eq!(deferred_count(), 0, "no cell for the lane's head");
    assert_eq!(
        entry_of(entity) & ENTRY_MARK_BITS,
        REOFFERED_MARK,
        "kept in R, the lane's mark on"
    );

    reset();
}
