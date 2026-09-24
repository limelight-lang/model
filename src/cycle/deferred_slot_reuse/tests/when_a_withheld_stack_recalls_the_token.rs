//! The marks by stack length: a mutator that withholds its returns under a
//! foreign holder recalls the grant once a stack holds its mark — M deaths,
//! M_b blocks, M_c chunks — and goes on freeing, waiting for nothing
//! (`dev/CYCLE-SPLIT-PACKAGE-3.md`, section 6).
//!
//! Every case reads the recall off the token and off the holder's slot, and
//! the token's count of waits for the mutator's side; the collector's stop at
//! the recall is `the_recall`'s in `cycle::arena` and the release behind
//! another mutator's batch is the worker's.

use super::*;
use crate::cycle::token::testing::HeldByACollector;
use crate::cycle::token::{REQUESTED, TraceToken, this_thread_token, word};
use crate::cycle::worker::{ELDER, take_the_recall_of};
use crate::memory::block_pool::BLOCK_SIZE;
use crate::memory::buffer_arena::{buffer_alloc_longlived_payload, buffer_free_longlived_payload};

/// `count` dead entities of this thread's, allocated and stamped.
unsafe fn dead_slots(count: usize) -> Vec<*mut u8> {
    (0..count)
        .map(|_| {
            let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
            assert!(!slot.is_null(), "the heap served");
            unsafe { dead_entity(slot) };
            slot
        })
        .collect()
}

fn recalled(token: *const TraceToken) -> bool {
    unsafe { (*token).is_recalled() }
}

/// A grant this thread consented to with no collector behind it, released
/// on the unwind as well: a failed assertion under it would leave the
/// thread's exit waiting on `COLLECTOR` for good.
struct Consented(*const TraceToken);

impl Drop for Consented {
    fn drop(&mut self) {
        unsafe { (*self.0).release_claim(ELDER, false) };
    }
}

/// Give back what the holder left withheld, as the poll does once the token
/// reads free.
fn drain() {
    unsafe { crate::gc::ll_gc_maybe_collect() };
    assert_eq!(foreign_withheld_count(), 0, "the poll gave the deaths back");
    assert_eq!(
        foreign_withheld_blocks(),
        0,
        "the poll gave the blocks back"
    );
    assert_eq!(
        foreign_withheld_chunks(),
        0,
        "the poll gave the chunks back"
    );
    assert_eq!(
        foreign_withheld_counts(),
        (0, 0, 0),
        "the drain took every weight off"
    );
}

/// Red without the deaths' count: the grant stands until the holder lets go
/// however many deaths the mutator withholds. A death past the mark tells
/// the slot nothing more, the grant being recalled once.
#[test]
fn the_mth_death_under_a_grant_recalls_it_and_the_mutator_never_waits() {
    let _guard = test_guard();
    let token = this_thread_token();
    let slots = unsafe { dead_slots(DEATHS_MARK + 1) };
    let _ = take_the_recall_of(ELDER);
    let waits = unsafe { (*token).waits() };

    let mut holder = HeldByACollector::take(token, false);
    for &slot in &slots[..DEATHS_MARK - 1] {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert!(!recalled(token), "no recall below the mark");
    assert!(!take_the_recall_of(ELDER), "the slot was not told");

    unsafe { crate::memory::stdapi::ll_free(slots[DEATHS_MARK - 1]) };
    assert!(recalled(token), "the mark's death recalls the grant");
    assert!(take_the_recall_of(ELDER), "the holder's slot is told");
    unsafe { crate::memory::stdapi::ll_free(slots[DEATHS_MARK]) };
    assert!(!take_the_recall_of(ELDER), "the grant is recalled once");
    assert_eq!(
        foreign_withheld_count(),
        DEATHS_MARK + 1,
        "every death still waits"
    );
    assert_eq!(
        unsafe { (*token).waits() },
        waits,
        "the mutator never waited"
    );

    holder.release();
    drain();
    assert!(!recalled(token), "the drain under FREE clears the recall");
}

/// Red if the drain leaves the count: the next grant would be recalled
/// before its own mark, or never, the count standing past the mark already.
#[test]
fn a_recall_at_the_mark_stops_no_later_grant() {
    let _guard = test_guard();
    let token = this_thread_token();
    let slots = unsafe { dead_slots(DEATHS_MARK) };
    let mut holder = HeldByACollector::take(token, false);
    for &slot in &slots {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert!(recalled(token), "the first grant is recalled at the mark");
    holder.release();
    drain();
    let _ = take_the_recall_of(ELDER);

    let slots = unsafe { dead_slots(DEATHS_MARK) };
    let mut holder = HeldByACollector::take(token, false);
    for &slot in &slots[..DEATHS_MARK - 1] {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    assert!(!recalled(token), "the second grant counts from zero");
    assert!(!take_the_recall_of(ELDER), "the slot was not told again");
    unsafe { crate::memory::stdapi::ll_free(slots[DEATHS_MARK - 1]) };
    assert!(
        recalled(token),
        "the second grant is recalled at its own mark"
    );
    holder.release();
    drain();
}

/// Red without the consent's clear: a recall standing stale when the
/// mutator consents — its holder gone, no stack at its mark — would stop the
/// new grant at its first reading.
#[test]
fn the_consent_clears_a_stale_recall() {
    let _guard = test_guard();
    let token = this_thread_token();
    unsafe { (*token).recall_for_test(true) };

    unsafe { (*token).request_for_test(word(REQUESTED, ELDER)) };
    let reading = crate::cycle::token::read_and_act_on_this_thread();
    let grant = Consented(token);
    assert_eq!(
        reading,
        crate::cycle::token::Reading::Collector,
        "the mutator consented"
    );
    assert!(!recalled(token), "the consent cleared the recall");
    drop(grant);
}

/// Red without the consent's reading of the counts: a grant opened while a
/// stack still holds its mark — the holder let go and no drain ran before
/// the next consent — would withhold on without a recall.
#[test]
fn a_grant_opened_on_a_stack_at_its_mark_is_recalled_at_its_consent() {
    let _guard = test_guard();
    let token = this_thread_token();
    let slots = unsafe { dead_slots(DEATHS_MARK) };
    let mut holder = HeldByACollector::take(token, false);
    for &slot in &slots {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    holder.release();
    let _ = take_the_recall_of(ELDER);

    unsafe { (*token).request_for_test(word(REQUESTED, ELDER)) };
    let reading = crate::cycle::token::read_and_act_on_this_thread();
    let grant = Consented(token);
    assert_eq!(
        reading,
        crate::cycle::token::Reading::Collector,
        "the mutator consented"
    );
    assert!(recalled(token), "the new grant is recalled at its consent");
    assert!(take_the_recall_of(ELDER), "the holder's slot is told");

    drop(grant);
    drain();
    assert!(!recalled(token), "the drain cleared the recall");
}

/// Red without the chunks' count.
#[test]
fn the_mth_chunk_under_a_grant_recalls_it() {
    let _guard = test_guard();
    let token = this_thread_token();
    let chunks: Vec<_> = (0..CHUNKS_MARK)
        .map(|_| {
            let (chunk, granted) = buffer_alloc_longlived_payload(256);
            assert!(!chunk.is_null(), "the buffer arena served");
            (chunk, granted)
        })
        .collect();
    let _ = take_the_recall_of(ELDER);

    let mut holder = HeldByACollector::take(token, false);
    for &(chunk, granted) in &chunks[..CHUNKS_MARK - 1] {
        unsafe { buffer_free_longlived_payload(chunk, granted) };
    }

    assert!(!recalled(token), "no recall below the mark");
    let (chunk, granted) = chunks[CHUNKS_MARK - 1];
    unsafe { buffer_free_longlived_payload(chunk, granted) };
    assert!(recalled(token), "the mark's chunk recalls the grant");
    assert!(take_the_recall_of(ELDER), "the holder's slot is told");

    holder.release();
    drain();
}

/// A run counts the blocks it spans: red if it counts as one block.
#[test]
fn a_run_counts_the_blocks_it_spans() {
    let _guard = test_guard();
    let token = this_thread_token();
    // A line of header ahead of the payload: `BLOCKS_MARK - 1` blocks of
    // payload span `BLOCKS_MARK` blocks, and one block less spans one less.
    let short = buffer_alloc_longlived_payload((BLOCKS_MARK - 2) * BLOCK_SIZE);
    let long = buffer_alloc_longlived_payload((BLOCKS_MARK - 1) * BLOCK_SIZE);
    assert!(!short.0.is_null() && !long.0.is_null(), "the system served");
    let _ = take_the_recall_of(ELDER);

    let mut holder = HeldByACollector::take(token, false);
    unsafe { buffer_free_longlived_payload(short.0, short.1) };
    assert!(!recalled(token), "a run one block short of the mark");
    holder.release();
    drain();

    let mut holder = HeldByACollector::take(token, false);
    unsafe { buffer_free_longlived_payload(long.0, long.1) };
    assert!(recalled(token), "a run spanning the mark recalls the grant");
    assert!(take_the_recall_of(ELDER), "the holder's slot is told");
    holder.release();
    drain();
}

/// A large entity's death counts the blocks its run spans rather than one
/// death: red if it counts on the deaths.
#[test]
fn a_large_entitys_death_counts_its_blocks() {
    let _guard = test_guard();
    let token = this_thread_token();
    let entity = crate::memory::large_entity::alloc((BLOCKS_MARK - 1) * BLOCK_SIZE);
    assert!(!entity.is_null(), "the system served");
    let entity = unsafe { dead_entity(entity) };
    let _ = take_the_recall_of(ELDER);

    let mut holder = HeldByACollector::take(token, false);
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    assert_eq!(foreign_withheld_count(), 1, "the death waits");
    assert!(recalled(token), "its run spans the blocks' mark");
    assert!(take_the_recall_of(ELDER), "the holder's slot is told");
    holder.release();
    drain();
}

/// A drain the collector's consent stops, after a first return's own drain
/// nested in it gave the chunks back: red if the nested drain zeroed the
/// counts, the consent then reading nothing withheld with the deaths still
/// standing in the outer drain's hands.
#[test]
fn a_consent_inside_a_drain_reads_what_the_stacks_hold() {
    let _guard = test_guard();
    let token = this_thread_token();
    let slots = unsafe { dead_slots(DEATHS_MARK + 2) };
    let (chunk, granted) = buffer_alloc_longlived_payload(256);
    assert!(!chunk.is_null(), "the buffer arena served");
    let mut holder = HeldByACollector::take(token, false);
    for &slot in &slots {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe { buffer_free_longlived_payload(chunk, granted) };
    holder.release();
    let _ = take_the_recall_of(ELDER);

    // The outer drain's hook arms the next drain's, which is the one the
    // first return makes: the collector asks there, and the second return
    // consents.
    before_the_next_returns(Box::new(|| {
        before_the_next_returns(Box::new(|| unsafe {
            (*this_thread_token()).request_for_test(word(REQUESTED, ELDER))
        }));
    }));
    unsafe { make_returns_withheld_under_a_foreign_trace() };
    let grant = Consented(token);

    assert_eq!(
        foreign_withheld_chunks(),
        0,
        "the nested drain gave the chunk back"
    );
    assert_eq!(
        foreign_withheld_counts(),
        (foreign_withheld_count(), 0, 0),
        "each count is what its stack holds"
    );
    assert!(recalled(token), "the consent read the deaths at their mark");
    assert!(take_the_recall_of(ELDER), "the holder's slot is told");

    drop(grant);
    drain();
}

/// A consent at a drain's last return, whose re-withheld death crosses the
/// blocks' mark: the grant is recalled, and the drain, whole, does not clear
/// that recall. Red if the drain's end clears a recall under an open grant.
#[test]
fn a_drain_ending_under_a_grant_keeps_its_recall() {
    let _guard = test_guard();
    let token = this_thread_token();
    let entity = crate::memory::large_entity::alloc((BLOCKS_MARK - 1) * BLOCK_SIZE);
    assert!(!entity.is_null(), "the system served");
    let entity = unsafe { dead_entity(entity) };
    let mut holder = HeldByACollector::take(token, false);
    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    holder.release();
    let _ = take_the_recall_of(ELDER);

    unsafe { (*token).request_for_test(word(REQUESTED, ELDER)) };
    unsafe { make_returns_withheld_under_a_foreign_trace() };
    let grant = Consented(token);

    assert_eq!(foreign_withheld_count(), 1, "the death was withheld again");
    assert!(
        recalled(token),
        "its blocks recall the grant it consented to"
    );

    drop(grant);
    drain();
}
