//! The detach and the restore: one collection takes the whole active chain,
//! and a trace that disposed of nothing puts it back.
//!
//! Y12 clause 2 gives the trace's reader a detached buffer, and clause 5 says
//! what happens to a root it did not dispose of: it keeps its registration
//! and its entry goes back. Between the two the roots are in a lane of their own, and
//! the property this module holds is that no record is lost, duplicated or
//! paid for on the way through.

use super::*;

use crate::memory::block_pool::BlockPool;
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

/// The pair, over a chain of two segments: the lane is empty between them and
/// identical afterwards.
#[test]
fn a_detach_empties_the_lane_and_a_restore_puts_it_back() {
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

    restore_candidates(batch);
    assert_eq!(candidate_count(), before);
    assert_eq!(segment_count(), 2);
    assert_eq!(
        write_segment_entry(0),
        second_entity,
        "the head goes back to the write position with its own fill"
    );

    reset();
}

/// The clause the pair exists to keep: a record per bit, and one lane per
/// record. Every token the two lanes held before the detach is in the batch or
/// in the overflow buffer, never in both, and the restore leaves the same set.
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
    // could only catch a clear, and a detach or a restore that set a bit on
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

    restore_candidates(batch);
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
fn neither_the_detach_nor_the_restore_asks_for_memory() {
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

    restore_candidates(batch);
    assert_eq!(
        allocation_probe::take_allocations(),
        (0, 0),
        "and so is the restore"
    );
    assert_eq!(BlockPool::global().blocks_out(), blocks_before);
    assert_eq!(gc_metadata::thread_stats(), stats_before);
    assert_eq!(spare_count(), spares_before, "and no cell was spent either");

    reset();
}

/// A registration while a batch is out takes the growth path, because the
/// write position is empty. That is the state the restore refuses: putting the
/// batch's head back would drop the fresh segment out of the chain with its own
/// roots' bits standing.
///
/// In a child process because the refusal is an assertion: the failing path
/// leaves a chain nothing owns, and a test that unwound through it would hand
/// the next test a pool short two blocks.
#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_restore_over_a_lane_that_grew_again_fails() {
    const CHILD: &str = "LL_QUEUE_RESTORE_OVER_A_GROWN_LANE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        reset();
        let _ = refill_spares();

        let mut header = candidate(2);
        assert!(unsafe { !release(&raw mut header) });
        let batch = detach_candidates();

        let mut later = candidate(2);
        assert!(unsafe { !release(&raw mut later) });
        assert_eq!(
            segment_count(),
            1,
            "the registration grew a lane of its own"
        );

        restore_candidates(batch);
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("cycle::queue::tests::the_batch_a_collection_detaches::a_restore_over_a_lane_that_grew_again_fails")
        .arg("--nocapture")
        .env(CHILD, "1")
        .output()
        .expect("the child runs this test again");
    assert!(
        !output.status.success(),
        "the restore refuses the grown lane"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("a candidate was registered while a batch was detached"),
        "and it says which rule it refused on"
    );
}

/// A batch is restored or the process stops. Dropping one silently would leave
/// every root in it carrying `CANDIDATE_BIT` with no record behind it, which
/// the gate then refuses to register again for the life of the process.
#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn a_batch_dropped_instead_of_restored_fails() {
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
        .arg("cycle::queue::tests::the_batch_a_collection_detaches::a_batch_dropped_instead_of_restored_fails")
        .arg("--nocapture")
        .env(CHILD, "1")
        .output()
        .expect("the child runs this test again");
    assert!(
        !output.status.success(),
        "an unrestored batch stops the process"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("dropped instead of restored"),
        "and it says what was dropped"
    );
}

/// The empty answer: a thread that registered nothing detaches a batch that
/// holds nothing, and restoring it is a no-op rather than a null write into the
/// write position.
#[test]
fn an_empty_lane_detaches_an_empty_batch() {
    let _g = test_guard();
    reset();

    let batch = detach_candidates();
    assert!(batch.is_empty());
    assert!(roots_of(&batch).is_empty());
    restore_candidates(batch);
    assert_eq!(candidate_count(), 0);
    assert_eq!(segment_count(), 0);

    reset();
}
