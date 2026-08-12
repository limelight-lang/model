//! What the sites record, and what they cost when the mask says nothing.
//!
//! Only where the sites exist, so the group is `debug-journal`'s alone:
//! without the feature there is nothing here to test, which is the whole
//! of §9.6's claim and is checked over the emitted IR instead.

use super::*;
use crate::journal::{Event, Window, between, mark, this_thread_identity};
use crate::memory::block_pool::BlockPool;
use crate::refcount::{EntityKind, MemoryCategory};

/// Every event the answers carry, whichever ring it came from.
fn events(windows: Vec<Window>) -> Vec<Event> {
    windows
        .into_iter()
        .flat_map(|window| match window {
            Window::Records(records) => records,
            _ => Vec::new(),
        })
        .collect()
}

/// The acceptance question of 2026-08-06 in miniature: which strings
/// died inside this window, answered from the journal alone. The hunt
/// itself is covered elsewhere; what this pins is that the two sites carry the
/// address and the kind it needs to ask.
#[test]
fn a_string_born_and_killed_inside_a_window_is_in_it_twice() {
    // The default set, held: a test that quiets the sites would
    // otherwise turn them off underneath this one, and the mask is
    // process-wide.
    let _sites = set_sites_for_test(DEFAULT_KINDS);
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

    let start = mark();
    let s = unsafe { crate::string::ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"hunt") };
    assert!(!s.is_null());
    let subject = s as u64;
    unsafe {
        assert!(crate::refcount::ll_release(
            s as *mut crate::refcount::RcHeader
        ));
        crate::object::ll_entity_die(s as *mut crate::refcount::RcHeader);
    }

    let end = mark();

    // By ring as well as by address: a slot is reused, and another
    // thread's entity born at this address inside the window would
    // answer under the same name.
    let ring = this_thread_identity();
    let mine: Vec<(u32, u64)> = events(between(&start, &end))
        .into_iter()
        .filter(|event| event.thread == ring && event.subject == subject)
        .map(|event| (event.kind, event.a))
        .collect();
    assert_eq!(
        mine,
        vec![
            (KIND_ENTITY_BIRTH, EntityKind::String as u64),
            (KIND_ENTITY_DEATH, EntityKind::String as u64),
        ],
        "a string's life inside the window is its birth and its death"
    );
}

/// A block's two ends are one address in two records, and the
/// decommission carries the kind the block arrived with — which is
/// how §9.5's third block event is asked for: a block leaving the set
/// the entity walk reaches is this record with an entity kind in it.
#[test]
fn a_block_round_trip_is_a_commission_and_a_decommission() {
    // The default set, held: a test that quiets the sites would
    // otherwise turn them off underneath this one, and the mask is
    // process-wide.
    let _sites = set_sites_for_test(DEFAULT_KINDS);
    let _g = crate::memory::block_pool::test_guard();
    let pool = BlockPool::global();

    let start = mark();
    let block = pool.get();
    assert!(!block.is_null());
    pool.put(block);
    let end = mark();

    // By ring as well as by address. A block is process-global: the
    // one this thread is handed may have been returned by another
    // inside the same window, which reads as a decommission before
    // the commission — seen once in forty runs under contention.
    let ring = this_thread_identity();
    let mine: Vec<u32> = events(between(&start, &end))
        .into_iter()
        .filter(|event| event.thread == ring && event.subject == block as u64)
        .map(|event| event.kind)
        .collect();
    // The trip is the tail rather than the whole of what the ring
    // holds for the address: `mark` frees the rings an eviction left
    // for a live thread to free, that free decommissions a block
    // onto this very ring, and the `get` above draws the same block
    // straight back out of the thread cache. A record before the
    // trip's own commission is therefore an earlier life of the
    // address — seen 1 in 300 runs at eight threads.
    let trip = mine
        .iter()
        .rposition(|&kind| kind == KIND_BLOCK_COMMISSIONED)
        .expect("this thread's own commission of the block is in the window");
    assert_eq!(
        &mine[trip..],
        &[KIND_BLOCK_COMMISSIONED, KIND_BLOCK_DECOMMISSIONED]
    );
}

/// A site on a thread with no ring yet allocates one, and that
/// allocation runs back through the very pool the site sits on. The
/// re-entry is refused rather than recursed into (§9.7), and the
/// borrow the pool's thread cache takes is closed before the site —
/// under a held borrow the re-entry finds the cell already borrowed
/// and the thread dies where a release build would abort.
///
/// Only the decommission kind is on, which is what makes `put`'s the
/// **first** record on that thread: with commissioning enabled the
/// ring is allocated by `get` long before, and the re-entry this is
/// about never happens.
#[test]
fn a_pools_own_site_may_be_a_threads_first_record() {
    let _only = set_sites_for_test(bit(KIND_BLOCK_DECOMMISSIONED));
    let _g = crate::memory::block_pool::test_guard();

    let recorded = std::thread::spawn(|| {
        // No `ll_thread_init`: the first record does that too, and
        // this is the shape a site inside the allocator really has.
        let pool = BlockPool::global();
        let mut blocks = Vec::new();
        // Past the thread cache's capacity, so the overflow flush
        // runs — that is the push to the global list the record must
        // not be holding a borrow across.
        for _ in 0..12 {
            blocks.push(pool.get());
        }

        let start = mark();
        for block in blocks {
            pool.put(block);
        }

        let end = mark();
        let ring = this_thread_identity();
        events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == ring && event.kind == KIND_BLOCK_DECOMMISSIONED)
            .count()
    })
    .join()
    .expect("a record on a ringless thread took the thread down");

    assert!(
        recorded > 0,
        "the thread journaled nothing, so the site was never reached"
    );
}

/// A kind the mask has turned off costs the load and the branch and
/// writes nothing — which is what lets an investigator spend a ring
/// on one question instead of on the loudest kind in the runtime.
#[test]
fn a_disabled_kind_writes_no_record() {
    let _quiet = disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    let pool = BlockPool::global();
    // One record through the door that ignores the mask, so that this
    // thread has a ring to be silent in: with no ring the silence
    // below would be the wrong silence, and the test would pass on a
    // thread the journal never reached.
    crate::journal::record(KIND_BLOCK_COMMISSIONED, 0, 0, 0, 0);
    let ring = this_thread_identity();
    assert_ne!(ring, 0, "the thread has no ring, so it is silent anyway");

    let start = mark();
    let block = pool.get();
    pool.put(block);
    let end = mark();

    assert_eq!(
        events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == ring && event.subject == block as u64)
            .count(),
        0,
        "a disabled kind reached the ring"
    );
}
