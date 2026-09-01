//! The queue's blocks are owned by the GC explicitly, not merely hidden
//! from the entity walker under the generic arena kind.

use super::*;

use crate::memory::block_pool::{
    BLOCK_KIND_GC_METADATA, BLOCK_PAYLOAD, BlockHeader, load_block_kind,
};
use crate::memory::gc_metadata::stats;

fn kind_of(block: *mut BlockHeader) -> u32 {
    unsafe { load_block_kind(&raw const (*block).kind) }
}

#[test]
fn the_floor_is_gc_memory_and_its_control_cost_is_in_the_capacity() {
    let _g = test_guard();
    reset();

    // The figures the commit message, `PLAN.md`, `docs/memory-manager.md`
    // and `dev/BENCHMARKS.md` all name. Written out rather than derived
    // through the expressions that define them: a test that recomputes a
    // constant agrees with whatever the constant becomes.
    assert_eq!(size_of::<OwnerCycleState>(), 64);
    assert_eq!(align_of::<OwnerCycleState>(), 64);
    assert_eq!(SEGMENT_CAPACITY, 8_160);
    assert_eq!(OVERFLOW_CAPACITY, 8_152);
    assert_eq!(POLL_STRIDE, 4_076);

    // The escrow ends flush with the block: one control line and the
    // entries account for the payload exactly, with no tail to absorb an
    // off-by-one and nothing of a neighbour within reach.
    assert_eq!(
        size_of::<OwnerCycleState>() + OVERFLOW_CAPACITY * size_of::<*mut RcHeader>(),
        BLOCK_PAYLOAD
    );

    let base = queue_base();
    assert!(!base.is_null());
    assert_eq!(kind_of(base), BLOCK_KIND_GC_METADATA);
    assert!(stats().current_blocks() >= 1);
}

#[test]
fn a_spare_stays_one_accounted_segment_when_it_becomes_live() {
    let _g = test_guard();
    reset();
    assert!(refill_spares());

    let before = stats().current_blocks();

    let mut header = candidate(2);
    assert!(unsafe { !release(&raw mut header) });

    assert_eq!(
        stats().current_blocks(),
        before,
        "spare to write segment is a state transition, not a second acquisition"
    );
    assert_eq!(kind_of(write_segment()), BLOCK_KIND_GC_METADATA);

    reset();
}

#[test]
fn the_floor_accepts_its_exact_rederived_escrow_capacity() {
    let _g = test_guard();
    reset();
    let state = owner_state();
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
fn one_entry_past_the_escrow_capacity_aborts() {
    const CHILD: &str = "LL_QUEUE_ESCROW_OVERFLOW_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _g = test_guard();
        reset();
        let state = owner_state();
        let mut header = candidate(2);
        for _ in 0..=OVERFLOW_CAPACITY {
            unsafe { append_to_overflow(state, &raw mut header) };
        }
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cycle::queue::tests::what_gc_owns::one_entry_past_the_escrow_capacity_aborts",
        ])
        .env(CHILD, "1")
        .output()
        .expect("the test binary runs as its own overflow child");
    // The signal, not merely a failure: any panic in the fixture would
    // satisfy an unsuccessful exit, and the escrow's last resort is an
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

/// Bytes in use inside the blocks the queue owns. Three quanta and no
/// others: the floor's control line, an escrowed entry, and a segment
/// that has left the live position full. A spare and the live segment's
/// own fill are reservation.
fn in_use() -> usize {
    stats().current_bytes_in_use()
}

#[test]
fn a_spare_is_reservation_and_a_full_segment_is_the_payload_it_holds() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the cells start full");
    crate::memory::gc_metadata::lower_peak_to_current();
    let before = in_use();

    let mut first = candidate(2);
    let first_entity = &raw mut first;
    assert!(unsafe { !release(first_entity) });
    assert_eq!(
        in_use(),
        before,
        "a segment in the write position is reservation, however full"
    );

    // The ordinary write, which is the path the whole design exists to
    // keep clear: three enrolments into the segment that now exists.
    let mut ordinary = [candidate(2), candidate(2), candidate(2)];
    for header in &mut ordinary {
        assert!(unsafe { !release(&raw mut *header) });
    }
    assert_eq!(in_use(), before, "an ordinary enrolment charges nothing");
    assert_eq!(
        crate::memory::gc_metadata::stats().peak_bytes_in_use(),
        before,
        "and reaches the high-water figure no more than the current one, \
         which a balanced charge and discharge on that path would"
    );

    fill_write_segment(first_entity);
    assert_eq!(in_use(), before, "the fill alone publishes nothing");

    let mut second = candidate(2);
    assert!(unsafe { !release(&raw mut second) });
    assert_eq!(
        in_use(),
        before + BLOCK_PAYLOAD,
        "the segment that left the write position is published whole"
    );

    reset();
    assert_eq!(
        in_use(),
        before,
        "the release gives every published byte back"
    );
}

#[test]
fn an_escrowed_entry_costs_the_pointer_it_holds_and_nothing_more() {
    let _g = test_guard();
    reset();
    let state = owner_state();
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
fn a_threads_floor_is_in_use_from_its_draw_until_its_exit() {
    let _g = test_guard();
    reset();
    let before = in_use();

    // The child holds no `test_guard`; what keeps the figure still under
    // it is that this thread is parked in `join` while holding the lock,
    // so no third thread can charge against the reading.
    std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        assert_eq!(
            in_use(),
            before + size_of::<OwnerCycleState>(),
            "the control line is working memory; the spares behind it are not"
        );
    })
    .join()
    .unwrap();

    assert_eq!(in_use(), before, "the exit returns the control line");
}

#[test]
fn an_entry_leaving_the_escrow_gives_its_pointer_back() {
    let _g = test_guard();
    reset();
    assert!(
        refill_spares(),
        "the move below re-registers into a spare, so the cells start full"
    );
    let state = owner_state();
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
        before,
        "the candidates left the overflow buffer, and the segment they went into is \
         reservation until it is full"
    );

    reset();
}

#[test]
fn the_live_segments_fill_reaches_the_high_water_figure_at_the_drain() {
    let _g = test_guard();
    reset();
    assert!(refill_spares(), "the first registration takes a spare");
    crate::memory::gc_metadata::lower_peak_to_current();
    let before = crate::memory::gc_metadata::stats();

    // Three entries and not a full segment: what the drain has to enter is
    // the fill, and a capacity would be satisfied by a constant.
    let mut candidates = [candidate(2), candidate(2), candidate(2)];
    for header in &mut candidates {
        assert!(unsafe { !release(&raw mut *header) });
    }
    assert_eq!(
        in_use(),
        before.current_bytes_in_use(),
        "nothing charges while the segment stands in the write position"
    );

    // The thread never grows the queue, so no transition has charged the
    // fill. The segment release is the one that ends it.
    reset();
    assert_eq!(
        crate::memory::gc_metadata::stats().peak_bytes_in_use(),
        before.current_bytes_in_use() + 3 * size_of::<*mut RcHeader>(),
        "the fill of a thread that never grew the queue is in the high-water figure"
    );
    assert_eq!(in_use(), before.current_bytes_in_use());
}
