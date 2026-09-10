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

/// Deferral preserves the original registrations as one deferred lane while a
/// later decrement writes the active lane. Re-offer is the inverse ownership
/// transition: no record is copied, dropped, or left in both lanes.
#[test]
fn a_deferred_batch_keeps_one_token_until_the_turnover_reoffers_it() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });
    let first_batch = detach_candidates();
    // The count is the caller's: it stands for the commit the reading that
    // found this batch live saw, and zero is as good as any other for a case
    // whose probes are full-width.
    defer_candidates(first_batch, 0);
    assert_eq!(candidate_count(), 0);
    assert_eq!(deferred_count(), 1);

    let mut second = candidate(2);
    let second_entity = &raw mut second;
    assert!(unsafe { !release(second_entity) });
    let second_batch = detach_candidates();
    defer_candidates(second_batch, 0);
    assert_eq!(candidate_count(), 0);
    assert_eq!(deferred_count(), 2);

    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        tokens
            .iter()
            .filter(|&&entry| entry == first_entity)
            .count(),
        1
    );
    assert_eq!(
        tokens
            .iter()
            .filter(|&&entry| entry == second_entity)
            .count(),
        1
    );

    assert!(reoffer_deferred_if_epoch_moved(u64::MAX));
    assert_eq!(candidate_count(), 2);
    assert_eq!(deferred_count(), 0);
    tokens.clear();
    collect_lane_tokens(&mut tokens);
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        tokens
            .iter()
            .filter(|&&entry| entry == first_entity)
            .count(),
        1
    );
    assert_eq!(
        tokens
            .iter()
            .filter(|&&entry| entry == second_entity)
            .count(),
        1
    );
    // A third deferral recorded at the same reading, so that the refusal below
    // is the mirror's answer rather than the empty lane's: with nothing
    // deferred the call returns on its first disjunct and an implementation
    // that never wrote the mirror would pass it.
    let mut third = candidate(2);
    let third_entity = &raw mut third;
    assert!(unsafe { !release(third_entity) });
    defer_candidates(detach_candidates(), u64::MAX);
    assert_eq!(
        deferred_count(),
        3,
        "the deferral takes back the two re-offered records with the new one"
    );
    assert!(
        !reoffer_deferred_if_epoch_moved(u64::MAX),
        "the same full-width reading may not re-offer a second time"
    );
    assert_eq!(deferred_count(), 3, "the refused reading moved nothing");

    reset();
}

/// A deferred lane of more than one segment, which is the merge arm the
/// one-record cases never reach: the lane's head is full, so the batch's own
/// head becomes the chain head instead of being copied into the room ahead of
/// it. The chain's length is what `deferred_count` walks and what the queue's
/// release has to discharge, and neither is exercised by a lane of one entry.
#[test]
fn a_deferred_lane_of_two_segments_comes_back_whole() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let mut filler = candidate(2);
    let filler_entity = &raw mut filler;
    assert!(unsafe { !release(filler_entity) });
    fill_write_segment(filler_entity);
    let mut grew = candidate(2);
    let grew_entity = &raw mut grew;
    assert!(unsafe { !release(grew_entity) });
    assert_eq!(segment_count(), 2);

    defer_candidates(detach_candidates(), 0);
    assert_eq!(candidate_count(), 0);
    assert_eq!(deferred_count(), SEGMENT_CAPACITY + 1);

    // The second deferral meets a full deferred head, so its own head is the
    // one that survives as the chain's. The cells are refilled first: the
    // growth above spent them, and a registration with no segment to write
    // into lands in the overflow buffer, which is not the lane this case is
    // about.
    assert!(refill_spares());
    let mut later = candidate(2);
    let later_entity = &raw mut later;
    assert!(unsafe { !release(later_entity) });
    assert_eq!(
        overflow_len(),
        0,
        "the record is in a segment, not the buffer"
    );
    defer_candidates(detach_candidates(), 0);
    assert_eq!(deferred_count(), SEGMENT_CAPACITY + 2);

    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert_eq!(tokens.len(), SEGMENT_CAPACITY + 2);
    assert_eq!(
        tokens
            .iter()
            .filter(|&&entry| entry == later_entity)
            .count(),
        1,
        "the record of the second deferral is in the lane once"
    );
    assert_eq!(
        tokens.iter().filter(|&&entry| entry == grew_entity).count(),
        1
    );

    assert!(reoffer_deferred_if_epoch_moved(u64::MAX));
    assert_eq!(deferred_count(), 0);
    assert_eq!(
        candidate_count(),
        SEGMENT_CAPACITY + 2,
        "every record of both segments came back"
    );

    reset();
}

/// A decrement while the candidate is deferred sees the standing bit and may
/// not register it a second time. Re-offer joins its original token to records
/// registered after the deferral.
#[test]
fn a_deferred_decrement_neither_duplicates_its_token_nor_loses_an_active_one() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let mut deferred = candidate(3);
    let deferred_entity = &raw mut deferred;
    assert!(unsafe { !release(deferred_entity) });
    defer_candidates(detach_candidates(), 0);
    assert_eq!(deferred_count(), 1);

    assert!(unsafe { !release(deferred_entity) });
    assert_eq!(candidate_count(), 0);
    assert_eq!(deferred_count(), 1);

    let mut active = candidate(2);
    let active_entity = &raw mut active;
    assert!(unsafe { !release(active_entity) });
    assert_eq!(candidate_count(), 1);

    assert!(reoffer_deferred_if_epoch_moved(u64::MAX));
    assert_eq!(candidate_count(), 2);
    assert_eq!(deferred_count(), 0);
    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert_eq!(
        tokens
            .iter()
            .filter(|&&entry| entry == deferred_entity)
            .count(),
        1
    );
    assert_eq!(
        tokens
            .iter()
            .filter(|&&entry| entry == active_entity)
            .count(),
        1
    );

    reset();
}

/// A record naming an entity whose death is complete gives its slot back on
/// the way into the deferred lane. Without the sweep the slot would be
/// withheld until the turnover, because retirement reads the active lane and
/// the deferred one is offered to nothing until then
/// (`rfc/model/gc/cycle/questions.md`, Y12 clause 8).
#[test]
fn a_deferral_retires_the_record_of_a_completed_death() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());
    let mut arena = Arena::new();
    let class = candidate_class("DeferredCompletedDeath");

    let survivor = unsafe { allocated_candidate(&mut arena, class, 1) };
    let dead = unsafe { allocated_candidate(&mut arena, class, 1) };
    for entity in [survivor, dead] {
        unsafe {
            crate::refcount::update_header_flags(entity, |flags| flags | CANDIDATE_BIT);
            append_entry(owner_state(), entity);
        }
    }
    unsafe { dismantle_candidate(dead) };
    assert_eq!(candidate_count(), 2);

    defer_candidates(detach_candidates(), 0);
    assert_eq!(
        deferred_count(),
        1,
        "the completed death was retired on the way into the lane"
    );
    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert_eq!(tokens, vec![survivor]);

    unsafe { dismantle_candidate(survivor) };
    assert!(reoffer_deferred_if_epoch_moved(u64::MAX));
    unsafe { retire_candidates() };
    assert_eq!(candidate_count(), 0);
    reset();
}
