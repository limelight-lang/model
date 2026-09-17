//! The walk over every lane, checked against the two counters the module
//! already has.
//!
//! `candidate_count` reads the ring's indices and `overflow_len` the buffer's
//! count, and neither can say which entity a record names. The rule the batch's
//! membership rests on is about entities: one `CANDIDATE_BIT` to one record,
//! and no record in two
//! lanes. `collect_lane_tokens` is what can state it, so it is calibrated here
//! against a population whose answer is known and against both counters.

use super::*;

use crate::test_support::allocation_probe;

/// The calibration: a ring of two blocks and a filled overflow buffer,
/// against the counters and against the entities the fixture registered.
#[test]
fn the_walk_answers_what_both_lanes_hold() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });

    // Filling the tail block and registering once more puts a second block
    // in the circle, so the walk crosses a block boundary: each block is
    // read from its own front to its own tail.
    fill_tail_block(first_entity);
    let mut second = candidate(2);
    let second_entity = &raw mut second;
    assert!(unsafe { !release(second_entity) });
    assert_eq!(segment_count(), 2);

    // Straight into the overflow buffer rather than through a pool the test
    // would have to exhaust: the tier below the reserve is a store and an
    // increment, and this is the store.
    let state = mutator_state();
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
        BLOCK_ENTRIES + 2,
        "one full block, the entry that grew the circle, and the overflowed one"
    );
    assert_eq!(tokens[0], first_entity, "the front block comes first");
    assert_eq!(
        tokens[BLOCK_ENTRIES], second_entity,
        "the second block follows it, and its first entry is the one the growth carried"
    );
    assert_eq!(
        tokens[tokens.len() - 1],
        overflowed_entity,
        "the overflow buffer comes after the ring"
    );
    assert_eq!(
        tokens.iter().filter(|&&t| t == second_entity).count(),
        1,
        "the entity that grew the circle holds one record"
    );
    assert_eq!(
        tokens.iter().filter(|&&t| t == overflowed_entity).count(),
        1,
        "and so does the one in the overflow buffer"
    );
    assert_eq!(
        tokens.iter().filter(|&&t| t == first_entity).count(),
        BLOCK_ENTRIES,
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

/// An unwind inside the deferral's own pass leaves every lane whole: the
/// record the pass had moved stands in the deferred lane, the records it had
/// not reached stand in the ring, and the overflow buffer holds what it held.
/// Without the pass finishing itself on the unwind the ring's indices would
/// stand where the pass stopped, and every record behind them would carry a
/// candidate bit no lane names.
#[test]
fn an_unwind_inside_the_deferral_keeps_every_lane_whole() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let mut deferred = candidate(2);
    let deferred_entity = &raw mut deferred;
    assert!(unsafe { !release(deferred_entity) });
    let batch = read_batch();

    // Two records behind the batch against one in it, so that a lane
    // holding the wrong records answers a different count rather than the
    // same.
    let mut active = candidate(2);
    let active_entity = &raw mut active;
    assert!(unsafe { !release(active_entity) });
    let mut second_active = candidate(2);
    let second_active_entity = &raw mut second_active;
    assert!(unsafe { !release(second_active_entity) });
    let state = mutator_state();
    let mut overflowed = candidate(2);
    let overflowed_entity = &raw mut overflowed;
    unsafe { append_to_overflow(state, overflowed_entity) };
    assert_eq!((candidate_count(), overflow_len()), (3, 1));

    // Raised after the batch's one record joined the deferred lane, before
    // the pass reached the two behind it.
    let _injection = compaction::inject(3);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| defer_candidates(batch, 0)))
            .is_err()
    );

    assert_eq!(
        candidate_count(),
        2,
        "the records behind the batch stand in the ring"
    );
    assert_eq!(deferred_count(), 1, "the batch stands in the deferred lane");
    assert_eq!(overflow_len(), 1, "and the overflow buffer keeps its entry");
    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    tokens.sort_unstable();
    let mut expected = vec![
        deferred_entity,
        active_entity,
        second_active_entity,
        overflowed_entity,
    ];
    expected.sort_unstable();
    assert_eq!(tokens, expected, "no record was dropped or duplicated");
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
    let first_batch = read_batch();
    // The count is the caller's: it stands for the commit the reading that
    // found this batch live saw, and zero is as good as any other for a case
    // whose probes are full-width.
    defer_candidates(first_batch, 0);
    assert_eq!(candidate_count(), 0);
    assert_eq!(deferred_count(), 1);

    let mut second = candidate(2);
    let second_entity = &raw mut second;
    assert!(unsafe { !release(second_entity) });
    let second_batch = read_batch();
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
    // The lane's block went into the circle with the re-offer, so the
    // deferral takes a fresh one from a cell.
    assert!(refill_spares());
    defer_candidates(read_batch(), u64::MAX);
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

/// A deferred lane of more than one block, which a lane of one record never
/// reaches: the lane's last block is full, so the deferral takes a second
/// spare for the rest. The lane's length is what `deferred_count` answers
/// and what the queue's release has to discharge, and the re-offer splices
/// every block of it into the circle.
#[test]
fn a_deferred_lane_of_two_blocks_is_spliced_back_whole() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let mut filler = candidate(2);
    let filler_entity = &raw mut filler;
    assert!(unsafe { !release(filler_entity) });
    fill_tail_block(filler_entity);
    let mut grew = candidate(2);
    let grew_entity = &raw mut grew;
    assert!(unsafe { !release(grew_entity) });
    assert_eq!(segment_count(), 2);

    // The deferral fills its blocks from the spare cells, which the growth
    // above spent: refilled first, so that the lane can take every record
    // rather than leaving the rest in the ring.
    assert!(refill_spares());
    defer_candidates(read_batch(), 0);
    assert_eq!(candidate_count(), 0);
    assert_eq!(deferred_count(), BLOCK_ENTRIES + 1);
    assert_eq!(
        deferred_segment_count(),
        2,
        "a full block and one with the rest"
    );
    assert_eq!(
        segment_count(),
        2,
        "the ring's two blocks stay in the circle, empty"
    );

    // A later record goes into the ring's emptied tail block, and the second
    // deferral appends it to the lane's last block, which has room.
    let mut later = candidate(2);
    let later_entity = &raw mut later;
    assert!(unsafe { !release(later_entity) });
    assert_eq!(
        overflow_len(),
        0,
        "the record is in a block, not the buffer"
    );
    defer_candidates(read_batch(), 0);
    assert_eq!(deferred_count(), BLOCK_ENTRIES + 2);
    assert_eq!(deferred_segment_count(), 2);

    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert_eq!(tokens.len(), BLOCK_ENTRIES + 2);
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
        BLOCK_ENTRIES + 2,
        "every record of both blocks came back"
    );
    assert_eq!(segment_count(), 4, "the lane's blocks joined the circle");

    reset();
}

/// The re-offer is a splice and draws nothing: at a poll with both cells
/// empty and the reserve drained, a deferred lane of three blocks is linked
/// into the circle after the tail block and read after what stood there,
/// with no allocation and no pool request.
#[test]
fn a_reoffer_at_a_poll_with_nothing_to_draw_splices_the_lane_in() {
    let _g = test_guard();
    reset();

    // Two deferrals build a lane of three blocks: the first fills a block
    // and starts a second, the second fills that one and starts a third.
    // Two locals rather than an array: indexing an array takes a `&mut` of
    // the whole of it, which retags away the pointer the earlier round
    // registered (`dev/WORKFLOW.md`, Miri).
    // Every registered header outlives the case: the second deferral's
    // sweep reads the first's records through their entries.
    let mut first_filler = candidate(2);
    let mut second_filler = candidate(2);
    let mut first_grew = candidate(2);
    let mut second_grew = candidate(2);
    let fillers = [&raw mut first_filler, &raw mut second_filler];
    let grew = [&raw mut first_grew, &raw mut second_grew];
    for (round, &filler_entity) in fillers.iter().enumerate() {
        assert!(refill_spares());
        assert!(unsafe { !release(filler_entity) });
        fill_tail_block(filler_entity);
        assert!(unsafe { !release(grew[round]) });
        // The lane's blocks come from the cells too.
        assert!(refill_spares());
        defer_candidates(read_batch(), 0);
        assert_eq!(candidate_count(), 0);
        assert_eq!(deferred_count(), (round + 1) * (BLOCK_ENTRIES + 1));
    }
    assert_eq!(deferred_segment_count(), 3);
    assert_eq!(segment_count(), 2, "the ring keeps its two consumed blocks");

    // One record ahead of the splice, to read the order against.
    let mut ahead = candidate(2);
    let ahead_entity = &raw mut ahead;
    assert!(unsafe { !release(ahead_entity) });

    // Nothing to draw from: the cells spent by hand, the reserve drained.
    let state = mutator_state();
    let mutator_state = unsafe { mutator_state_ref(state) };
    loop {
        let spare = take_spare(mutator_state);
        if spare.is_null() {
            break;
        }
        crate::memory::gc_metadata::release(spare);
    }
    crate::memory::critical::drain_for_test();
    assert_eq!(spare_count(), 0);

    let _ = allocation_probe::take_allocations();
    assert!(reoffer_deferred_if_epoch_moved(u64::MAX));
    assert_eq!(
        allocation_probe::take_allocations(),
        (0, 0),
        "the splice neither allocates nor asks the pool"
    );
    assert_eq!(deferred_count(), 0);
    assert_eq!(candidate_count(), 2 * (BLOCK_ENTRIES + 1) + 1);
    assert_eq!(segment_count(), 5, "the three blocks joined the two");

    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert_eq!(
        tokens[0], ahead_entity,
        "what stood in the ring is read first"
    );
    for filler_entity in fillers {
        assert_eq!(
            tokens
                .iter()
                .filter(|&&entry| entry == filler_entity)
                .count(),
            BLOCK_ENTRIES,
            "and the lane's records follow it whole"
        );
    }

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
    defer_candidates(read_batch(), 0);
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
/// the way into the deferred lane rather than being deferred: the retirement
/// outranks the mark. Without that the slot would be withheld until the
/// turnover, the deferred lane being offered to nothing until then
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
            append_entry(mutator_state(), entity);
        }
    }
    unsafe { dismantle_candidate(dead) };
    assert_eq!(candidate_count(), 2);

    defer_candidates(read_batch(), 0);
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
