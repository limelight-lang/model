//! The queue's blocks are owned by the GC explicitly, not merely hidden
//! from the entity walker under the generic arena kind.

use super::*;

use crate::memory::block_pool::{
    BLOCK_KIND_GC_METADATA, BLOCK_PAYLOAD, BlockHeader, load_block_kind,
};
use crate::memory::gc_metadata::{stats, thread_stats};

fn kind_of(block: *mut BlockHeader) -> u32 {
    unsafe { load_block_kind(&raw const (*block).kind) }
}

#[test]
fn the_base_block_is_gc_memory_and_its_control_cost_is_in_the_capacity() {
    let _g = test_guard();
    reset();

    // The figures the commit message, `PLAN.md`, `docs/memory-manager.md`
    // and `dev/BENCHMARKS.md` all name. Written out rather than derived
    // through the expressions that define them: a test that recomputes a
    // constant agrees with whatever the constant becomes.
    assert_eq!(size_of::<MutatorCycleState>(), 64);
    assert_eq!(align_of::<MutatorCycleState>(), 64);
    assert_eq!(BLOCK_ENTRIES, 8_135);
    assert_eq!(OVERFLOW_CAPACITY, 8_152);
    assert_eq!(POLL_STRIDE, 4_076);

    // The overflow buffer ends flush with the block: one control line and the
    // entries account for the payload exactly, with no tail to absorb an
    // off-by-one and nothing of a neighbour within reach.
    assert_eq!(
        size_of::<MutatorCycleState>() + OVERFLOW_CAPACITY * size_of::<*mut RcHeader>(),
        BLOCK_PAYLOAD
    );

    let base = queue_base();
    assert!(!base.is_null());
    assert_eq!(kind_of(base), BLOCK_KIND_GC_METADATA);
    assert!(thread_stats().current_blocks() >= 1);
}

#[test]
fn a_spare_stays_one_accounted_block_when_it_joins_the_ring() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let before = thread_stats().current_blocks();

    let mut header = candidate(2);
    assert!(unsafe { !release(&raw mut header) });

    assert_eq!(
        thread_stats().current_blocks(),
        before,
        "spare to ring block is a state transition, not a second acquisition"
    );
    assert_eq!(kind_of(tail_block()), BLOCK_KIND_GC_METADATA);

    reset();
}

#[test]
fn the_base_block_accepts_its_exact_rederived_overflow_capacity() {
    let _g = test_guard();
    reset();
    let state = mutator_state();
    let mut header = candidate(2);

    for _ in 0..OVERFLOW_CAPACITY {
        unsafe { append_to_overflow(state, &raw mut header) };
    }
    assert_eq!(overflow_len(), OVERFLOW_CAPACITY);

    // What makes the capacity exact rather than merely sufficient: the
    // entry past the last one is the first byte of the next block. A
    // capacity one too large would fill without complaint on stable and
    // would be seen only by Miri.
    let past_the_last = unsafe { overflow_entries(state).add(OVERFLOW_CAPACITY) } as *mut u8;
    assert_eq!(past_the_last, BlockHeader::end(queue_base()));

    reset();
}

#[test]
#[cfg_attr(
    miri,
    ignore = "spawns a child process, which Miri's isolation forbids"
)]
fn one_entry_past_the_overflow_capacity_aborts() {
    const CHILD: &str = "LL_QUEUE_ESCROW_OVERFLOW_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        reset();
        let state = mutator_state();
        let mut header = candidate(2);
        for _ in 0..=OVERFLOW_CAPACITY {
            unsafe { append_to_overflow(state, &raw mut header) };
        }
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cycle::queue::tests::what_gc_owns::one_entry_past_the_overflow_capacity_aborts",
        ])
        .env(CHILD, "1")
        .output()
        .expect("the test binary runs as its own overflow child");
    // The signal, not merely a failure: any panic in the fixture would
    // satisfy an unsuccessful exit, and the overflow buffer's last resort is an
    // abort with no frame to report through.
    use std::os::unix::process::ExitStatusExt;
    // `SIGABRT`, which is 6 on every unix this crate builds for. Spelled
    // out because the crate takes no `libc` dependency.
    assert_eq!(
        output.status.signal(),
        Some(6),
        "capacity plus one did not abort; status {:?}",
        output.status
    );
}

#[test]
fn the_entity_row_dispatch_never_enters_gc_metadata() {
    let _g = test_guard();
    reset();
    let pretend_child = BlockHeader::payload_start(queue_base()) as *mut RcHeader;

    assert_eq!(
        unsafe { crate::cycle::row::resolve_edge_target(pretend_child) },
        crate::cycle::row::EdgeTarget::Untracked
    );
}

/// Bytes in use inside the blocks the queue owns. Three quanta and no others:
/// the base block's control line, an overflow-buffer entry, and a block in
/// the ring or the deferred lane, charged whole at the link. A spare is
/// reservation; a block's fill moves no figure.
fn in_use() -> usize {
    thread_stats().current_bytes_in_use()
}

#[test]
fn a_spare_is_reservation_and_a_block_in_the_ring_is_the_payload_it_holds() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");
    crate::memory::gc_metadata::lower_thread_peak_to_current();
    let before = in_use();

    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });
    assert_eq!(
        in_use(),
        before + BLOCK_PAYLOAD,
        "the block the first registration links in is charged whole"
    );

    // The ordinary write, which is the path the whole design exists to
    // keep clear: three registrations into the block that now exists.
    let mut ordinary = [candidate(2), candidate(2), candidate(2)];
    for header in &mut ordinary {
        assert!(unsafe { !release(&raw mut *header) });
    }
    assert_eq!(
        in_use(),
        before + BLOCK_PAYLOAD,
        "an ordinary registration charges nothing"
    );
    assert_eq!(
        crate::memory::gc_metadata::thread_stats().peak_bytes_in_use(),
        before + BLOCK_PAYLOAD,
        "and reaches the high-water figure no more than the current one, \
         which a balanced charge and discharge on that path would"
    );

    fill_tail_block(first_entity);
    assert_eq!(
        in_use(),
        before + BLOCK_PAYLOAD,
        "the fill alone publishes nothing"
    );

    let mut second = candidate(2);
    assert!(unsafe { !release(&raw mut second) });
    assert_eq!(
        in_use(),
        before + 2 * BLOCK_PAYLOAD,
        "the second block is charged whole as it is linked in"
    );

    reset();
    assert_eq!(
        in_use(),
        before,
        "the release gives every charged byte back"
    );
}

#[test]
fn an_overflow_entry_costs_the_pointer_it_holds_and_nothing_more() {
    let _g = test_guard();
    reset();
    let state = mutator_state();
    let before = in_use();
    let mut header = candidate(2);

    for _ in 0..3 {
        unsafe { append_to_overflow(state, &raw mut header) };
    }
    assert_eq!(in_use(), before + 3 * size_of::<*mut RcHeader>());

    reset();
    assert_eq!(in_use(), before);
}

#[test]
fn a_threads_base_block_is_in_use_from_its_draw_until_its_exit() {
    let _g = test_guard();
    reset();
    // The process figure and not this thread's, the claim being about memory a
    // thread that no longer exists gave back. It is the reading a third thread
    // can move, and the one case here that no per-thread figure can replace
    // (`PLAN.md`, "A gate flake watch, not a step").
    let before = stats().current_bytes_in_use();

    std::thread::spawn(move || {
        // The child's own figure, which starts at zero and is moved by the
        // init alone: what the draw charges is stated without reference to
        // what the rest of the suite is holding.
        assert_eq!(in_use(), 0, "the thread has charged nothing yet");
        assert!(crate::memory::heap::ll_thread_init());
        assert_eq!(
            in_use(),
            size_of::<MutatorCycleState>() + BLOCK_PAYLOAD,
            "the control line and P's block are working memory; the spares \
             behind them are not"
        );
    })
    .join()
    .unwrap();

    assert_eq!(
        stats().current_bytes_in_use(),
        before,
        "the exit returns the control line"
    );
}

#[test]
fn an_entry_leaving_the_overflow_buffer_gives_its_pointer_back() {
    let _g = test_guard();
    reset();
    assert!(
        refill_spares(),
        "the move below re-registers into a spare, so the cells start full"
    );
    let state = mutator_state();
    let before = in_use();
    let mut header = candidate(2);

    for _ in 0..3 {
        unsafe { append_to_overflow(state, &raw mut header) };
    }
    assert_eq!(in_use(), before + 3 * size_of::<*mut RcHeader>());

    drain_overflow();
    assert_eq!(overflow_len(), 0, "a spare cell took all three");
    assert_eq!(
        in_use(),
        before + BLOCK_PAYLOAD,
        "the candidates left the overflow buffer, and the block they went into \
         is charged whole"
    );

    reset();
}

#[test]
fn a_consumed_block_stays_charged_until_it_leaves_the_circle() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the first registration takes a spare");
    crate::memory::gc_metadata::lower_thread_peak_to_current();
    let before = crate::memory::gc_metadata::thread_stats();

    let class = candidate_class("ChargedBlockCandidate");
    let mut arena = Arena::new();
    let entity = unsafe { allocated_candidate(&mut arena, class, 2) };
    assert!(unsafe { !release(entity) });
    assert_eq!(in_use(), before.current_bytes_in_use() + BLOCK_PAYLOAD);

    // The entry is retired and the block is empty, and it is still the
    // ring's: the charge stays with the circle rather than with the fill.
    unsafe { dismantle_candidate(entity) };
    unsafe { retire_candidates() };
    assert_eq!(candidate_count(), 0);
    assert_eq!(segment_count(), 1, "the emptied block stays in the circle");
    assert_eq!(
        in_use(),
        before.current_bytes_in_use() + BLOCK_PAYLOAD,
        "an empty block in the circle is charged as a full one"
    );

    reset();
    assert_eq!(in_use(), before.current_bytes_in_use());
    assert_eq!(
        crate::memory::gc_metadata::thread_stats().peak_bytes_in_use(),
        before.current_bytes_in_use() + BLOCK_PAYLOAD,
        "the high-water figure is the block, and no fill stands beside it"
    );
}

/// A block the deferral links into the lane is charged whole as a ring
/// block is, at the link and not at the fill, and the lane's blocks are
/// discharged with the rest at the release. The ring's own charge does not
/// move when the deferral empties its blocks: they stay in the circle.
#[test]
fn a_block_the_deferral_links_into_the_lane_is_charged_whole() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");
    let before = in_use();

    let mut filler = candidate(2);
    let mut grew = candidate(2);
    unsafe { ring_of_two_blocks(&raw mut filler, &raw mut grew) };
    assert_eq!(
        in_use(),
        before + 2 * BLOCK_PAYLOAD,
        "the ring's two blocks"
    );

    assert!(refill_spares());
    assert_eq!(
        in_use(),
        before + 2 * BLOCK_PAYLOAD,
        "a spare is reservation"
    );
    defer_candidates(read_batch(), 0);
    assert_eq!(deferred_segment_count(), 2);
    assert_eq!(
        in_use(),
        before + 4 * BLOCK_PAYLOAD,
        "the lane's two blocks are charged whole at the link, beside the ring's emptied two"
    );

    reset();
    assert_eq!(
        in_use(),
        before,
        "the release gives every charged byte back"
    );
}
