//! What a non-final decrement leaves behind: an entry naming the entity
//! and the bit that says one does.
//!
//! The pair is Y12 clause 4, and neither half is worth anything alone —
//! a bit with no entry reserves an examination that never comes, and an
//! entry with no bit is enrolled again by the next decrement.

use super::*;

/// The ordinary case, and the only one where both halves land.
#[test]
fn a_non_final_decrement_puts_the_entity_in_this_thread_s_queue() {
    let _g = test_guard();
    reset();
    let _ = refill_spares();

    let mut header = candidate(2);
    let entity = &raw mut header;

    assert_eq!(candidate_count(), 0, "the queue starts empty");
    assert!(unsafe { !release(entity) });

    assert_eq!(candidate_count(), 1);
    assert_eq!(
        write_segment_entry(0),
        entity,
        "the entry names the entity itself"
    );
    assert_ne!(
        unsafe { mutator_flags(entity) } & CANDIDATE_BIT,
        0,
        "the bit says a queue entry names it"
    );

    reset();
}

/// The bit is the gate's fifth clause, so a second decrement of the same
/// entity adds nothing — which is what keeps one entity to one entry.
#[test]
fn a_second_decrement_of_a_registered_entity_writes_no_second_entry() {
    let _g = test_guard();
    reset();
    let _ = refill_spares();

    let mut header = candidate(3);
    let entity = &raw mut header;

    assert!(unsafe { !release(entity) });
    assert_eq!(candidate_count(), 1);

    assert!(unsafe { !release(entity) });
    assert_eq!(
        candidate_count(),
        1,
        "the candidate bit refuses the second decrement at the gate"
    );

    reset();
}

/// A decrement the gate refuses leaves the queue alone, which is what
/// makes the queue's population the candidate set rather than the
/// release traffic.
#[test]
fn a_decrement_the_gate_refuses_leaves_no_entry() {
    let _g = test_guard();
    reset();
    let _ = refill_spares();

    let mut acyclic = candidate_with(2, ACYCLIC_GATE);
    assert!(unsafe { !release(&raw mut acyclic) });

    let mut dying = candidate(1);
    assert!(
        unsafe { release(&raw mut dying) },
        "reaching zero is a death, not a candidate"
    );

    assert_eq!(candidate_count(), 0);

    reset();
}
