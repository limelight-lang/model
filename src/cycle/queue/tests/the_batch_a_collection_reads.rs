//! The reading and the close: one collection reads the whole ring as its
//! batch, and the close disposes of those entries where they stand.
//!
//! Y12 clause 2 gives the trace's reader the ring behind the writer, and
//! clause 5 says what happens to a root it did not dispose of: it keeps its
//! registration and its entry stays. Nothing leaves the ring on the way, and
//! the property this module holds is that no record is lost, duplicated,
//! moved out of order or paid for by a reading.

use super::*;

use crate::memory::block_pool::BlockPool;
use crate::memory::gc_metadata;
use crate::test_support::allocation_probe;

/// The candidate bit of the two entities, one set and one down: the half of
/// the clause a token walk cannot state, since a record answers for a bit
/// and neither a reading nor a retirement may touch one.
fn assert_bits(set: *mut RcHeader, down: *mut RcHeader) {
    assert_ne!(
        unsafe { mutator_flags(set) } & CANDIDATE_BIT,
        0,
        "a record crossed the reading and the bit that answers for it went down"
    );
    assert_eq!(
        unsafe { mutator_flags(down) } & CANDIDATE_BIT,
        0,
        "a record crossed the reading and a bit went up behind it"
    );
}

/// Every root the batch holds, in the walk's own order — from the front
/// block's front — read into a vector a test can compare.
fn roots_of(batch: &Batch) -> Vec<*mut RcHeader> {
    let mut roots = Vec::new();
    batch.walk_roots(|root| {
        roots.push(root);
        true
    });
    roots
}

/// A reading over a ring of two blocks counts every entry and moves none:
/// the ring, its blocks and its order are what they were, and the batch
/// reads them from the front.
#[test]
fn a_reading_counts_the_ring_and_takes_nothing_out() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });
    fill_tail_block(first_entity);
    let mut second = candidate(2);
    let second_entity = &raw mut second;
    assert!(unsafe { !release(second_entity) });

    let before = candidate_count();
    assert_eq!(before, BLOCK_ENTRIES + 1);
    assert_eq!(segment_count(), 2);

    let batch = read_batch();
    assert!(!batch.is_empty());
    assert_eq!(
        candidate_count(),
        before,
        "the ring holds every entry while the batch is read"
    );
    assert_eq!(segment_count(), 2, "and every block: nothing went anywhere");

    let roots = roots_of(&batch);
    assert_eq!(
        roots.len(),
        before,
        "the batch has as many records as the ring"
    );
    assert_eq!(roots[0], first_entity, "read from the front block's front");
    assert_eq!(
        roots[BLOCK_ENTRIES], second_entity,
        "across the block boundary, in the order they were written"
    );
    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert_eq!(tokens, roots, "and the batch is the ring's walk");

    reset();
}

/// The clause the reading exists to keep: a record per bit, and one lane per
/// record. Every token the lanes held before the reading is in the batch or
/// in the overflow buffer, never in both, and a retirement over live roots
/// leaves the same set.
#[test]
fn every_token_stays_in_its_lane_across_a_reading() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut chained = candidate(2);
    let chained_entity = &raw mut chained;
    assert!(unsafe { !release(chained_entity) });

    // A second record in the ring whose bit is down. The pair is what
    // makes the bit assertions below able to fail: with one entity the walk
    // could only catch a clear, and a reading that set a bit on every root
    // it passed would keep every token set equal and still be wrong.
    let mut bitless = candidate(2);
    let bitless_entity = &raw mut bitless;
    assert!(unsafe { !release(bitless_entity) });
    unsafe { crate::refcount::clear_candidate_bit(bitless_entity) };

    // A third in the tier below the reserve, because the overflow buffer is
    // the lane the reading deliberately leaves out.
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

    let batch = read_batch();
    assert_eq!(
        roots_of(&batch),
        vec![chained_entity, bitless_entity],
        "both records of the ring are in the batch, and the overflow buffer's is not"
    );
    let mut during = Vec::new();
    collect_lane_tokens(&mut during);
    assert_eq!(during, before, "and nothing moved for the reading");
    assert_bits(chained_entity, bitless_entity);

    drop(batch);
    unsafe { retire_candidates() };
    let mut after = Vec::new();
    collect_lane_tokens(&mut after);
    assert_eq!(
        after, before,
        "a retirement over standing roots keeps the set as it was"
    );
    assert_bits(chained_entity, bitless_entity);

    reset();
}

/// Neither the reading nor a retirement over live roots draws, charges or
/// discharges anything: no global allocation, no pool request, no cell
/// spent, and neither the ledger's current figures nor its high-water ones
/// moved. That is what a reading that takes nothing out can be held to
/// (`dev/DECISIONS.md`, "the detach of a candidate chain draws no segment",
/// whose requirement the ring meets by construction).
#[test]
fn neither_the_reading_nor_the_retirement_asks_for_memory() {
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

    let batch = read_batch();
    assert_eq!(
        allocation_probe::take_allocations(),
        (0, 0),
        "the reading is a count over the indices"
    );
    assert_eq!(BlockPool::global().blocks_out(), blocks_before);
    assert_eq!(gc_metadata::thread_stats(), stats_before);

    drop(batch);
    unsafe { retire_candidates() };
    assert_eq!(
        allocation_probe::take_allocations(),
        (0, 0),
        "and so is the retirement that keeps every entry"
    );
    assert_eq!(BlockPool::global().blocks_out(), blocks_before);
    assert_eq!(gc_metadata::thread_stats(), stats_before);
    assert_eq!(spare_count(), spares_before, "and no cell was spent either");

    reset();
}

/// A registration made after the reading stands behind the batch: the
/// close's marks reach the batch and nothing behind it, and the compaction
/// keeps the later record in the ring, in order.
#[test]
fn a_registration_behind_the_batch_is_kept_by_the_close() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");

    let mut read = candidate(2);
    let read_entity = &raw mut read;
    assert!(unsafe { !release(read_entity) });
    let mut batch = read_batch();

    let mut later = candidate(2);
    let later_entity = &raw mut later;
    assert!(unsafe { !release(later_entity) });
    assert_eq!(candidate_count(), 2);

    assert_eq!(
        roots_of(&batch),
        vec![read_entity],
        "the batch is what the ring had when it was read"
    );
    assert_eq!(
        batch.mark_for_deferral(|_| true),
        1,
        "the marks reach the batch's records and no later one"
    );
    dispose_candidates(batch, 0);

    assert_eq!(candidate_count(), 1, "the later record stays in the ring");
    assert_eq!(
        deferred_count(),
        1,
        "and the batch's went to the deferred lane"
    );
    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert_eq!(tokens, vec![later_entity, read_entity]);

    reset();
}

/// A batch dropped without a disposition leaves every record where it was:
/// nothing was taken out, so nothing can be lost by not putting it back.
#[test]
fn a_batch_dropped_without_a_disposition_leaves_the_ring_whole() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let mut header = candidate(2);
    let entity = &raw mut header;
    assert!(unsafe { !release(entity) });
    drop(read_batch());

    assert_eq!(candidate_count(), 1);
    let mut tokens = Vec::new();
    collect_lane_tokens(&mut tokens);
    assert_eq!(tokens, vec![entity]);
    assert_ne!(unsafe { mutator_flags(entity) } & CANDIDATE_BIT, 0);

    reset();
}

/// The empty answer: a thread that registered nothing reads a batch that
/// holds nothing, and its disposition is a no-op over an empty ring.
#[test]
fn an_empty_lane_reads_an_empty_batch() {
    let _g = test_guard();
    reset();

    let batch = read_batch();
    assert!(batch.is_empty());
    assert!(roots_of(&batch).is_empty());
    dispose_candidates(batch, 0);
    assert_eq!(candidate_count(), 0);
    assert_eq!(segment_count(), 0);

    // And the same before this thread has any state at all, which is the
    // arm a null base block and a null record take.
    let empty = std::thread::spawn(|| {
        let batch = read_batch();
        (batch.is_empty(), roots_of(&batch).len())
    })
    .join()
    .expect("the thread reads its own empty queue");
    assert_eq!(empty, (true, 0));

    reset();
}

/// The walk stops where the visitor stops and says so, and a walk that runs
/// through answers true: what the two trace phases read the bound off.
#[test]
fn a_walk_of_the_batch_stops_where_the_visitor_stops() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let mut headers = [candidate(2), candidate(2), candidate(2)];
    for header in &mut headers {
        assert!(unsafe { !release(&raw mut *header) });
    }
    let batch = read_batch();

    let mut seen = 0;
    assert!(
        !batch.walk_roots(|_| {
            seen += 1;
            seen < 2
        }),
        "a stop is reported"
    );
    assert_eq!(seen, 2, "and nothing past it was visited");

    seen = 0;
    assert!(batch.walk_roots(|_| {
        seen += 1;
        true
    }));
    assert_eq!(seen, 3);

    reset();
}
