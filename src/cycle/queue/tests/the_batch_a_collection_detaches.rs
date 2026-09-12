//! The detach and the merge: one collection takes the whole active chain, and
//! the close joins it back into whatever the lane holds by then.
//!
//! Y12 clause 2 gives the trace's reader a detached buffer, and clause 5 says
//! what happens to a root it did not dispose of: it keeps its registration
//! and its entry goes back. Between the two the roots are in a lane of their own, and
//! the property this module holds is that no record is lost, duplicated or
//! paid for on the way through.

use super::*;

use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockPool};
use crate::memory::gc_metadata;
use crate::test_support::allocation_probe;

/// The candidate bit of the two chained entities, one set and one down: the
/// half of the clause a token walk cannot state, since a record answers for a
/// bit and neither end of the pair may touch one.
fn assert_bits(set: *mut RcHeader, down: *mut RcHeader) {
    assert_ne!(
        unsafe { mutator_flags(set) } & CANDIDATE_BIT,
        0,
        "a record crossed the pair and the bit that answers for it went down"
    );
    assert_eq!(
        unsafe { mutator_flags(down) } & CANDIDATE_BIT,
        0,
        "a record crossed the pair and a bit went up behind it"
    );
}

/// Every root the batch holds, in the walk's own order — newest segment first,
/// oldest entry within each — read into a vector a test can compare.
fn roots_of(batch: &InFlightBatch) -> Vec<*mut RcHeader> {
    let mut roots = Vec::new();
    batch.walk_roots(|root| {
        roots.push(root);
        true
    });
    roots
}

/// The pair over an untouched lane, and a chain of two segments: the lane is
/// empty between them and identical afterwards.
#[test]
fn a_detach_empties_the_lane_and_a_merge_puts_it_back() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });
    fill_write_segment(first_entity);
    let mut second = candidate(2);
    let second_entity = &raw mut second;
    assert!(unsafe { !release(second_entity) });

    let before = candidate_count();
    assert_eq!(before, SEGMENT_CAPACITY + 1);
    assert_eq!(segment_count(), 2);

    let batch = detach_candidates();
    assert!(!batch.is_empty());
    assert_eq!(
        candidate_count(),
        0,
        "the lane holds nothing while a batch is out"
    );
    assert_eq!(
        segment_count(),
        0,
        "and no segment either: the chain went whole"
    );
    assert_eq!(
        write_segment(),
        std::ptr::null_mut(),
        "the write position is the state a thread holds before its first registration"
    );
    assert_eq!(
        roots_of(&batch).len(),
        before,
        "the batch has as many records as the lane did"
    );

    merge_candidates(batch);
    assert_eq!(candidate_count(), before);
    assert_eq!(segment_count(), 2);
    assert_eq!(
        write_segment_entry(0),
        second_entity,
        "an empty active lane restores the original head"
    );
    let mut restored = Vec::new();
    collect_lane_tokens(&mut restored);
    assert_eq!(
        restored
            .iter()
            .filter(|&&entry| entry == second_entity)
            .count(),
        1
    );
    assert_eq!(
        restored
            .iter()
            .filter(|&&entry| entry == first_entity)
            .count(),
        SEGMENT_CAPACITY
    );

    reset();
}

/// The clause the pair exists to keep: a record per bit, and one lane per
/// record. Every token the two lanes held before the detach is in the batch or
/// in the overflow buffer, never in both, and the merge leaves the same set.
#[test]
fn every_token_crosses_the_detach_exactly_once() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut chained = candidate(2);
    let chained_entity = &raw mut chained;
    assert!(unsafe { !release(chained_entity) });

    // A second record in the same lane whose bit is down. The pair is what
    // makes the bit assertions below able to fail: with one entity the walk
    // could only catch a clear, and a detach or a merge that set a bit on
    // every root it passed would keep every token set equal and still be
    // wrong.
    let mut bitless = candidate(2);
    let bitless_entity = &raw mut bitless;
    assert!(unsafe { !release(bitless_entity) });
    unsafe { crate::refcount::clear_candidate_bit(bitless_entity) };

    // A third in the tier below the reserve, because the overflow buffer is
    // the lane the detach deliberately leaves alone.
    let state = owner_state();
    let mut overflowed = candidate(2);
    let overflowed_entity = &raw mut overflowed;
    unsafe { append_to_overflow(state, overflowed_entity) };

    let mut before = Vec::new();
    collect_lane_tokens(&mut before);
    assert_eq!(
        before,
        vec![chained_entity, bitless_entity, overflowed_entity]
    );
    assert_bits(chained_entity, bitless_entity);

    let batch = detach_candidates();
    let mut during = Vec::new();
    collect_lane_tokens(&mut during);
    assert_eq!(
        during,
        vec![overflowed_entity],
        "the overflow buffer's entry stays where it is; only the chain moves"
    );
    assert_eq!(
        roots_of(&batch),
        vec![chained_entity, bitless_entity],
        "and both records of the chain are in the batch, in one lane and not two"
    );
    assert_bits(chained_entity, bitless_entity);

    merge_candidates(batch);
    let mut after = Vec::new();
    collect_lane_tokens(&mut after);
    assert_eq!(after, before, "the set of records is what it was");
    assert_bits(chained_entity, bitless_entity);

    reset();
}

/// Neither end draws, charges or discharges anything: no global allocation, no
/// pool request, no cell spent, and neither the ledger's current figures nor
/// its high-water ones moved. That is what
/// a detach of two words can be held to (`dev/DECISIONS.md`, "the detach of a
/// candidate chain draws no segment").
#[test]
fn neither_the_detach_nor_the_merge_asks_for_memory() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells are stocked ahead of the path");

    let mut header = candidate(2);
    let entity = &raw mut header;
    assert!(unsafe { !release(entity) });

    let blocks_before = BlockPool::global().blocks_out();
    // The registration above already spent a cell, this thread's first
    // registration being a growth by construction, so the stock is read here
    // rather than assumed full.
    let spares_before = spare_count();
    // The peak is lowered first, because it never falls on its own and a figure
    // this pair cannot move is one no assertion can see moved
    // (`gc_metadata::lower_thread_peak_to_current`).
    gc_metadata::lower_thread_peak_to_current();
    let stats_before = gc_metadata::thread_stats();
    let _ = allocation_probe::take_allocations();

    let batch = detach_candidates();
    assert_eq!(
        allocation_probe::take_allocations(),
        (0, 0),
        "the detach is two cell swaps"
    );
    assert_eq!(BlockPool::global().blocks_out(), blocks_before);
    assert_eq!(gc_metadata::thread_stats(), stats_before);

    merge_candidates(batch);
    assert_eq!(
        allocation_probe::take_allocations(),
        (0, 0),
        "and so is the merge into an untouched lane"
    );
    assert_eq!(BlockPool::global().blocks_out(), blocks_before);
    assert_eq!(gc_metadata::thread_stats(), stats_before);
    assert_eq!(spare_count(), spares_before, "and no cell was spent either");

    reset();
}

/// A registration made while the batch is out owns a different partial head.
/// Combining the two retains both records and returns the surplus segment.
#[test]
fn a_merge_over_a_lane_that_grew_again_keeps_both_records() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut detached = candidate(2);
    let detached_entity = &raw mut detached;
    assert!(unsafe { !release(detached_entity) });
    let batch = detach_candidates();

    let mut severed = candidate(2);
    let severed_entity = &raw mut severed;
    assert!(unsafe { !release(severed_entity) });
    assert_eq!(
        segment_count(),
        1,
        "the registration grew a lane of its own"
    );

    let blocks_before = BlockPool::global().blocks_out();
    let spares_before = spare_count();
    // The peak never falls on its own, so it is lowered before the reading a
    // merge must leave alone (`gc_metadata::lower_thread_peak_to_current`).
    gc_metadata::lower_thread_peak_to_current();
    let stats_before = gc_metadata::thread_stats();

    merge_candidates(batch);

    let mut after = Vec::new();
    collect_lane_tokens(&mut after);
    assert_eq!(
        after,
        vec![severed_entity, detached_entity],
        "the newer record first, and the batch's behind it"
    );
    assert_eq!(segment_count(), 1, "in one chain rather than two");
    assert_eq!(
        spare_count(),
        spares_before + 1,
        "and the emptied head went to a cell"
    );
    assert_eq!(
        BlockPool::global().blocks_out(),
        blocks_before,
        "rather than to the pool"
    );
    assert_eq!(
        gc_metadata::thread_stats().current_bytes_in_use(),
        stats_before.current_bytes_in_use(),
        "neither partial input head was charged"
    );

    reset();
}

/// A batch with an interior full segment combines with a partial active head.
/// Every record survives, and the published chain has one partial head only.
#[test]
fn a_merged_batch_publishes_only_full_segments_behind_the_head() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut filler = candidate(2);
    let filler_entity = &raw mut filler;
    let mut oldest = candidate(2);
    let oldest_entity = &raw mut oldest;
    assert!(unsafe { !release(oldest_entity) });
    fill_write_segment(filler_entity);

    let mut newest = candidate(2);
    let newest_entity = &raw mut newest;
    assert!(unsafe { !release(newest_entity) });
    assert_eq!(segment_count(), 2, "a full segment behind the write one");

    let batch = detach_candidates();

    // Both cells went to the two growths above, and the reserve is empty under
    // `reset`. Without this fill the registration below would find every
    // allocation path refused and land in the overflow buffer, which is a lane
    // the merge never touches — the case would then pass over an untouched
    // write position and prove nothing.
    assert!(refill_spares(), "the registration below grows from a cell");

    let mut severed = candidate(2);
    let severed_entity = &raw mut severed;
    assert!(unsafe { !release(severed_entity) });
    assert_eq!(
        segment_count(),
        1,
        "the registration grew a lane of its own"
    );
    assert_eq!(overflow_len(), 0, "and no entry went to the tier below");

    merge_candidates(batch);

    assert_eq!(segment_count(), 2, "the head, and the batch's full segment");
    assert_eq!(
        candidate_count(),
        2 + SEGMENT_CAPACITY,
        "counted by the fill rule, which holds only while the segment behind \
         the head is the full one"
    );

    let mut after = Vec::new();
    collect_lane_tokens(&mut after);
    assert_eq!(after.len(), 2 + SEGMENT_CAPACITY, "and the walk agrees");
    for entry in [severed_entity, newest_entity, oldest_entity] {
        assert_eq!(after.iter().filter(|&&found| found == entry).count(), 1);
    }
    assert_eq!(
        after
            .iter()
            .filter(|&&found| found == filler_entity)
            .count(),
        SEGMENT_CAPACITY - 1
    );

    reset();
}

/// Two uncharged input heads yield one charged interior and one uncharged
/// output head. Neither a spare cell nor a new block is consumed.
#[test]
fn combining_a_full_and_partial_head_charges_one_output_interior() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut filler = candidate(2);
    let filler_entity = &raw mut filler;
    let mut oldest = candidate(2);
    let oldest_entity = &raw mut oldest;
    assert!(unsafe { !release(oldest_entity) });
    fill_write_segment(filler_entity);

    let batch = detach_candidates();
    assert!(refill_spares(), "the registration below grows from a cell");

    let mut severed = candidate(2);
    let severed_entity = &raw mut severed;
    assert!(unsafe { !release(severed_entity) });
    assert_eq!(segment_count(), 1);

    let blocks_before = BlockPool::global().blocks_out();
    let spares_before = spare_count();
    gc_metadata::lower_thread_peak_to_current();
    let charged_before = gc_metadata::thread_stats().current_bytes_in_use();

    merge_candidates(batch);

    assert_eq!(
        segment_count(),
        2,
        "both input segments remain in the packed output"
    );
    assert_eq!(
        candidate_count(),
        1 + SEGMENT_CAPACITY,
        "counted by the fill rule, which holds of the spliced head"
    );
    assert_eq!(
        spare_count(),
        spares_before,
        "no cell was spent and none was taken back"
    );
    assert_eq!(BlockPool::global().blocks_out(), blocks_before);
    assert_eq!(
        gc_metadata::thread_stats().current_bytes_in_use(),
        charged_before + BLOCK_PAYLOAD,
        "the output interior carries one payload charge"
    );

    let mut after = Vec::new();
    collect_lane_tokens(&mut after);
    assert_eq!(after.len(), 1 + SEGMENT_CAPACITY);
    for entry in [severed_entity, oldest_entity] {
        assert_eq!(after.iter().filter(|&&found| found == entry).count(), 1);
    }
    assert_eq!(
        after
            .iter()
            .filter(|&&found| found == filler_entity)
            .count(),
        SEGMENT_CAPACITY - 1
    );

    reset();
}

/// A full active head and a partial detached head already hold enough storage
/// for their combined records. Both segments remain, with no overflow append.
#[test]
fn a_merge_uses_input_storage_when_the_active_head_is_full() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    // The batch's head holds one record and the live head is full, so the
    // copy of that one record cannot fit and has to grow.
    let mut detached = candidate(2);
    let detached_entity = &raw mut detached;
    assert!(unsafe { !release(detached_entity) });
    let batch = detach_candidates();

    assert!(refill_spares(), "the growths below draw from cells");
    let mut filler = candidate(2);
    let filler_entity = &raw mut filler;
    let mut severed = candidate(2);
    let severed_entity = &raw mut severed;
    assert!(unsafe { !release(severed_entity) });
    fill_write_segment(filler_entity);
    assert_eq!(candidate_count(), SEGMENT_CAPACITY);

    merge_candidates(batch);

    assert_eq!(
        candidate_count(),
        SEGMENT_CAPACITY + 1,
        "the record crossed into a lane that had no room for it"
    );
    assert_eq!(
        segment_count(),
        2,
        "the two input segments provide all output storage"
    );
    assert_eq!(overflow_len(), 0, "no record needed the overflow buffer");

    let mut after = Vec::new();
    collect_lane_tokens(&mut after);
    assert_eq!(after.len(), SEGMENT_CAPACITY + 1);
    assert_eq!(
        after[0], detached_entity,
        "the copied record is the newest of the lane"
    );
    assert_eq!(after[1], severed_entity, "and the lane's own is behind it");

    reset();
}

/// A batch is merged back or the process stops. Dropping one silently would leave
/// every root in it carrying `CANDIDATE_BIT` with no record behind it, which
/// the gate then refuses to register again for the life of the process.
#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_batch_dropped_instead_of_merged_fails() {
    const CHILD: &str = "LL_QUEUE_BATCH_DROPPED_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        reset();
        let _ = refill_spares();

        let mut header = candidate(2);
        assert!(unsafe { !release(&raw mut header) });
        drop(detach_candidates());
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("cycle::queue::tests::the_batch_a_collection_detaches::a_batch_dropped_instead_of_merged_fails")
        .arg("--nocapture")
        .env(CHILD, "1")
        .output()
        .expect("the child runs this test again");
    assert!(
        !output.status.success(),
        "a batch nothing merged back stops the process"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("dropped instead of merged back"),
        "and it says what was dropped"
    );
}

/// The empty answer: a thread that registered nothing detaches a batch that
/// holds nothing, and merging it back is a no-op rather than a null write into
/// the write position.
#[test]
fn an_empty_lane_detaches_an_empty_batch() {
    let _g = test_guard();
    reset();

    let batch = detach_candidates();
    assert!(batch.is_empty());
    assert!(roots_of(&batch).is_empty());
    merge_candidates(batch);
    assert_eq!(candidate_count(), 0);
    assert_eq!(segment_count(), 0);

    reset();
}

/// Full segments do not enter the merge's record loop. Only the detached
/// partial head moves into the active head, and the counter records that bound
/// — the baseline the early-retirement measurement took (`dev/BENCHMARKS.md`, "early pressure retirement returns matching slots at one extra queue pass").
#[test]
fn a_merge_moves_only_what_fits_between_the_two_partial_heads() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let mut detached = candidate(2);
    let detached_entity = &raw mut detached;
    unsafe { append_entry(owner_state(), detached_entity) };
    fill_write_segment(detached_entity);
    unsafe { append_entry(owner_state(), detached_entity) };
    fill_write_segment(detached_entity);
    assert!(
        refill_spares(),
        "the final partial head has its own segment"
    );
    for _ in 0..3 {
        unsafe { append_entry(owner_state(), detached_entity) };
    }
    let batch = detach_candidates();

    assert!(refill_spares());
    let mut active = candidate(2);
    let active_entity = &raw mut active;
    for _ in 0..5 {
        unsafe { append_entry(owner_state(), active_entity) };
    }

    let _ = take_queue_work();
    merge_candidates(batch);
    assert_eq!(
        take_queue_work(),
        QueueWork {
            record_passes: 0,
            records_read: 3,
            records_moved: 3,
        }
    );
    assert_eq!(candidate_count(), 2 * SEGMENT_CAPACITY + 8);
    assert_eq!(segment_count(), 3);

    reset();
}

/// When the detached head does not fit, the merge takes records from its end.
/// Its untouched prefix can therefore become the output head without a second
/// shift across the records that remain there.
#[test]
fn a_merge_leaves_the_detached_heads_prefix_in_place() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let mut detached = candidate(2);
    let detached_entity = &raw mut detached;
    for _ in 0..5 {
        unsafe { append_entry(owner_state(), detached_entity) };
    }
    let batch = detach_candidates();

    assert!(refill_spares());
    let mut active = candidate(2);
    let active_entity = &raw mut active;
    for _ in 0..SEGMENT_CAPACITY - 2 {
        unsafe { append_entry(owner_state(), active_entity) };
    }

    let _ = take_queue_work();
    merge_candidates(batch);
    assert_eq!(
        take_queue_work(),
        QueueWork {
            record_passes: 0,
            records_read: 2,
            records_moved: 2,
        }
    );
    assert_eq!(candidate_count(), SEGMENT_CAPACITY + 3);
    assert_eq!(segment_count(), 2);
    assert_eq!(write_segment_entry(0), detached_entity);

    reset();
}
