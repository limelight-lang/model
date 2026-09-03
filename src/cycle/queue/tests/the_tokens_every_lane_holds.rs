//! The walk over every lane, checked against the two counters the module
//! already has.
//!
//! `candidate_count` walks the chain and `overflow_len` reads the buffer, and
//! neither can say which entity a record names. The rule S36.12 has to state
//! is about entities: one `CANDIDATE_BIT` to one record, and no record in two
//! lanes. `collect_lane_tokens` is what can state it, so it is calibrated here
//! against a population whose answer is known and against both counters.

use super::*;

/// The calibration: a chain of two segments and a filled overflow buffer,
/// against the counters and against the entities the fixture registered.
#[test]
fn the_walk_answers_what_both_lanes_hold() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });

    // Filling the head and registering once more puts a full segment behind
    // the write position, which is the case the walk's per-segment bound
    // exists for: the head is read to its fill and the segment behind it to
    // capacity.
    fill_write_segment(first_entity);
    let mut second = candidate(2);
    let second_entity = &raw mut second;
    assert!(unsafe { !release(second_entity) });
    assert_eq!(segment_count(), 2);

    // Straight into the overflow buffer rather than through a pool the test
    // would have to exhaust: the tier below the reserve is a store and an
    // increment, and this is the store.
    let state = owner_state();
    let mut overflowed = candidate(2);
    let overflowed_entity = &raw mut overflowed;
    unsafe { append_to_overflow(state, overflowed_entity) };
    assert_eq!(overflow_len(), 1);

    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);

    assert_eq!(
        tokens.len(),
        candidate_count() + overflow_len(),
        "the walk and the two counters disagree about how many records exist"
    );
    assert_eq!(
        tokens.len(),
        SEGMENT_CAPACITY + 2,
        "one full segment, the entry that grew the chain, and the overflowed one"
    );
    assert_eq!(
        tokens[0], second_entity,
        "the newest segment comes first, and its first entry is the one the growth carried"
    );
    assert_eq!(
        tokens[tokens.len() - 1],
        overflowed_entity,
        "the overflow buffer comes after the chain"
    );
    assert_eq!(
        tokens.iter().filter(|&&t| t == second_entity).count(),
        1,
        "the entity that grew the chain holds one record"
    );
    assert_eq!(
        tokens.iter().filter(|&&t| t == overflowed_entity).count(),
        1,
        "and so does the one in the overflow buffer"
    );
    assert_eq!(
        tokens.iter().filter(|&&t| t == first_entity).count(),
        SEGMENT_CAPACITY,
        "the first entity's own record and the fixture's filler, which is that same pointer"
    );

    reset();
}

/// The empty answer, which is what every later assertion of "nothing is left
/// enrolled" rests on: a walk that answered nothing whatever the queue held
/// would pass such an assertion silently.
#[test]
fn an_empty_queue_answers_nothing() {
    let _g = test_guard();
    reset();

    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert!(tokens.is_empty());
    assert_eq!(candidate_count(), 0);
    assert_eq!(overflow_len(), 0);

    // And the answer is the same before this thread has any state at all,
    // which is the arm a null base block takes.
    let empty = std::thread::spawn(|| {
        let mut tokens = Vec::new();
        collect_lane_tokens(&mut tokens);
        tokens.len()
    })
    .join()
    .expect("the thread reads its own empty queue");
    assert_eq!(empty, 0);

    reset();
}
