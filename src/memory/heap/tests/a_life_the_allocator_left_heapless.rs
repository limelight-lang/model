//! A thread whose `ThreadHeaps` allocation the OS refuses starts all the
//! same: `ll_thread_init` answers `true`, the TLS slot stays null, and every
//! allocation path on that thread reports null rather than ending the
//! process. The state is reached here through `refuse_thread_heap`, which
//! denies that one allocation on the thread that holds the guard.

use super::*;
use crate::memory::heap::refuse_thread_heap;

/// The refusal leaves the thread started and heapless, which is what
/// `ll_thread_init` promises for a heap the OS would not give.
#[test]
fn a_thread_whose_heap_is_refused_starts_without_one() {
    let _g = crate::memory::block_pool::test_guard();
    let heapless = std::thread::spawn(|| {
        let _refused = refuse_thread_heap();
        assert!(ll_thread_init(), "a heapless thread is a started thread");
        thread_heap().is_null()
    })
    .join()
    .expect("the refused thread panicked");

    assert!(heapless, "the thread was given a heap it was refused");
}

/// The refusal is the holder's alone. A process-wide one would reach every
/// thread a test running beside this one starts, and a heapless thread is a
/// state those tests do not expect; the second thread here starts while the
/// first holds its guard and reads that it is funded.
#[test]
fn a_thread_started_beside_a_refused_one_gets_its_heap() {
    let _g = crate::memory::block_pool::test_guard();
    let both_ready = std::sync::Arc::new(std::sync::Barrier::new(2));
    let beside = std::sync::Arc::clone(&both_ready);
    let (funded_started, neighbour_has_started) = std::sync::mpsc::channel();

    let refused = std::thread::spawn(move || {
        let _refused = refuse_thread_heap();
        assert!(ll_thread_init(), "a heapless thread is a started thread");
        let heapless = thread_heap().is_null();
        both_ready.wait();
        // The guard is dropped where this closure ends, so the wait is what
        // holds the refusal over the neighbour's whole init: a barrier passed
        // before that init only orders its beginning, and the ordinary
        // interleaving would run it with the cell already restored.
        neighbour_has_started
            .recv()
            .expect("the funded thread panicked");
        heapless
    });

    let funded = std::thread::spawn(move || {
        beside.wait();
        assert!(ll_thread_init(), "the runtime started this thread");
        let funded = !thread_heap().is_null();
        funded_started.send(()).expect("the refused thread waits");
        !funded
    });

    assert!(
        refused.join().expect("the refused thread panicked"),
        "the thread holding the guard was given a heap"
    );
    assert!(
        !funded.join().expect("the funded thread panicked"),
        "a neighbour's guard refused this thread's heap"
    );
}

/// Every allocation path on a heapless thread reports null, which is what
/// `heapless_allocation` is for: the thread is started, so the entry point
/// may not end the process, and it holds no heap to serve the request from.
#[test]
fn an_allocation_on_a_heapless_thread_reports_null() {
    let _g = crate::memory::block_pool::test_guard();
    let refused = std::thread::spawn(|| {
        let _refused = refuse_thread_heap();
        assert!(ll_thread_init(), "a heapless thread is a started thread");
        assert!(thread_heap().is_null(), "this case needs a heapless thread");
        let small = unsafe { crate::memory::stdapi::ll_alloc(40, 16) };
        let entity = unsafe { entity_alloc(32) };
        (small.is_null(), entity.is_null())
    })
    .join()
    .expect("the refused thread panicked");

    let (small, entity) = refused;
    assert!(small, "a thread with no heap served a raw allocation");
    assert!(entity, "a thread with no heap served an entity allocation");
}

/// A heapless thread frees what another thread allocated: the free reads the
/// block rather than the freeing thread's heap, so it is posted to the
/// owner's stack and the owner accounts for it. Read off the owner's own
/// slots, `blocks_out` being shared with every test running beside this one.
#[test]
fn a_heapless_thread_frees_what_another_allocated() {
    let _g = crate::memory::block_pool::test_guard();
    const SLOTS: usize = 16;

    let before = unsafe { with_thread_heap(|heap| heap.live_slots_after_collect()) };
    let mut allocated = Vec::with_capacity(SLOTS);
    unsafe {
        with_thread_heap(|heap| {
            for _ in 0..SLOTS {
                let slot = heap.alloc(24);
                assert!(!slot.is_null(), "the owner was refused a slot to free");
                allocated.push(slot as usize);
            }
        });
    }

    // The instrument reads sixteen before the case asks it to see them come
    // back: two readings that are equal say nothing where the count clamps at
    // zero, and a refused slot would hold the equality vacuously.
    let allocated_now = unsafe { with_thread_heap(|heap| heap.live_slots_after_collect()) };
    assert_eq!(
        allocated_now,
        before + SLOTS as u32,
        "the owner never accounted for the slots it allocated"
    );

    std::thread::spawn(move || {
        let _refused = refuse_thread_heap();
        assert!(ll_thread_init(), "a heapless thread is a started thread");
        assert!(thread_heap().is_null(), "this case needs a heapless thread");
        for slot in allocated {
            unsafe { crate::memory::stdapi::ll_free(slot as *mut u8) };
        }
    })
    .join()
    .expect("the freeing thread panicked");

    let after = unsafe { with_thread_heap(|heap| heap.live_slots_after_collect()) };
    assert_eq!(
        after, before,
        "the owner never got back what the heapless thread freed"
    );
}

/// The record a collector reaches this thread through is handed over
/// claimable, as a funded thread's is. Under the hold `ll_thread_init` takes
/// for the length of the initialisation, the claim below fails and the
/// thread's own first collection would wait on it.
///
/// The claim is made by hand rather than through
/// `cycle::token::testing::HeldByACollector`, which stands in for a collector
/// the same way: that helper asserts the claim inside its own thread, and
/// what this case reads is the answer itself, which a refused claim must
/// carry back to an assertion of its own.
#[test]
fn a_heapless_thread_hands_its_record_over_claimable() {
    let _g = crate::memory::block_pool::test_guard();
    use crate::cycle::token::testing::Handed;
    use crate::cycle::token::this_thread_token;

    let (started, initialised) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let refused = std::thread::spawn(move || {
        let _refused = refuse_thread_heap();
        assert!(ll_thread_init(), "a heapless thread is a started thread");
        assert!(thread_heap().is_null(), "this case needs a heapless thread");
        started
            .send(Handed(this_thread_token()))
            .expect("the claiming thread waits for this");
        // Blocked here for the claim, so no store of this thread's races
        // the stand-in collector's swap.
        let _ = released.recv();
    });

    let token = initialised.recv().expect("the refused thread panicked");
    let claimed = unsafe { (*token.token()).claim_for_test(crate::cycle::worker::ELDER) };
    if claimed {
        unsafe { (*token.token()).release_claim(crate::cycle::worker::ELDER, false) };
    }

    drop(release);
    refused.join().expect("the refused thread panicked");
    assert!(
        claimed,
        "the initialisation's hold outlived a heapless thread's start"
    );
}

/// A heapless life gives back everything it drew. The thread holds no heap,
/// so what it takes at its init is the queue's base block, the two spare
/// segments, the two reserves and its record; the exit guard returns them,
/// and the process-wide readings come back to where they stood. Under
/// `debug-journal` the life's ring is retired rather than freed, and the
/// registry keeps a retired ring readable until a later thread evicts it, so
/// the pool is one block short of its reading — measured in both builds —
/// unless the retirement pushed an older ring off the quota. Each reading
/// follows a mark, which frees the rings evicted before it, so that no other
/// thread's free of one lands between the two readings.
#[test]
fn a_heapless_life_gives_back_the_blocks_it_drew() {
    let _g = crate::memory::block_pool::test_guard();
    let evictions_before = crate::journal::mark().evictions();
    let metadata_before = crate::memory::gc_metadata::stats().current_blocks();
    let blocks_before = crate::memory::block_pool::BlockPool::global().blocks_out();

    std::thread::spawn(|| {
        let _refused = refuse_thread_heap();
        assert!(ll_thread_init(), "a heapless thread is a started thread");
        assert!(thread_heap().is_null(), "this case needs a heapless thread");
    })
    .join()
    .expect("the refused thread panicked");

    assert_eq!(
        crate::memory::gc_metadata::stats().current_blocks(),
        metadata_before,
        "a heapless life kept GC metadata past its exit"
    );
    let evicted = crate::journal::mark().evictions() - evictions_before;
    let ring_kept = usize::from(cfg!(feature = "debug-journal")) - evicted as usize;
    assert_eq!(
        crate::memory::block_pool::BlockPool::global().blocks_out(),
        blocks_before + ring_kept,
        "a heapless life kept blocks past its exit"
    );
}
