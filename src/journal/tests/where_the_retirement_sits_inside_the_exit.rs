//! The ring is retired by the last act of `ll_thread_exit`, so a
//! `__destruct` body's records and every block handover are inside a
//! window over the thread's death — the reserve and the pool's
//! thread cache are drained there by hand for that reason. Past that
//! act completeness ends and honesty does not: a record from a TLS
//! destructor running later is in the ring or counted as lost, never
//! neither.

use super::*;

/// A `__destruct` body runs in step 1 of `heap::ll_thread_exit` and
/// journals like any other code; the ring is retired by the last act
/// of that same function, so the record lands in it. That ordering is
/// the whole of what this journal was built for — the census
/// hypothesis is about a *finishing* thread — and a retirement placed
/// earlier loses exactly those records.
#[test]
fn a_destructor_at_thread_exit_is_recorded_before_the_ring_retires() {
    let _quiet = kinds::disable_sites_for_test();
    const SUBJECT: u64 = 0xD1E;
    let _g = crate::memory::block_pool::test_guard();
    let start = mark();

    let identity = std::thread::spawn(|| {
        use crate::class::ClassBuilder;
        use crate::memory::arena::Arena;
        use crate::memory::context::LLContext;
        use crate::refcount::MemoryCategory;
        use crate::value::{Tag, Value};

        /// A `__destruct` that journals, which is what a record site
        /// on the death path will do once §9.5's set is built.
        unsafe extern "C" fn journaling_destructor(_obj: *mut crate::object::Object) {
            record(ANY_KIND, 0, SUBJECT, 0, 0);
        }

        crate::memory::heap::ll_thread_init();
        let identity = {
            record(ANY_KIND, 0, 0, 0, 0);
            this_thread_identity()
        };

        // A static holding the object is what makes thread exit the
        // point its destructor runs (`static_block.rs`).
        let cls = ClassBuilder::new("JournalingAtExit")
            .destructor(journaling_destructor as *const ())
            .build();
        let holder = ClassBuilder::new("StaticsOfJournalingAtExit")
            .prop("kept", true)
            .build();
        let size = unsafe { (*holder).object_size } as usize;
        let block = unsafe {
            std::alloc::alloc_zeroed(std::alloc::Layout::from_size_align(size, 16).unwrap())
        };

        assert!(!block.is_null());

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { crate::object::new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            let slot = block.add(16) as *mut Value;
            assert!(crate::memory::barrier::store_box(
                &mut arena,
                MemoryCategory::LongLived,
                slot,
                Value::entity(Tag::Object, obj as *mut crate::refcount::RcHeader),
            ));
            crate::static_block::ll_static_block_register(block, holder);
            // The static's store took the second reference.
            assert!(!crate::refcount::ll_release(
                obj as *mut crate::refcount::RcHeader
            ));
        }

        crate::memory::heap::ll_thread_exit();
        identity
    })
    .join()
    .expect("the journaling thread panicked");

    let end = mark();
    let subjects: Vec<u64> = events(between(&start, &end))
        .into_iter()
        .filter(|event| event.thread == identity)
        .map(|event| event.subject)
        .collect();
    assert_eq!(
        subjects,
        vec![0, SUBJECT],
        "the destructor's record was raised into a ring already retired"
    );
}

/// The retirement is the exit's **last** act, and this is what says
/// so: a dying thread's block handovers are records in its own ring
/// rather than losses. Everything the teardown gives back — the
/// barrier reserve, the pool's thread cache, and the heap's own
/// blocks — goes through `BlockPool::put`, which is a default event
/// kind, so a ring retired one step earlier would answer this window
/// with the exit record and nothing after it.
///
/// The order inside the ring is the claim, not the count:
/// [`KIND_THREAD_EXIT`](crate::journal::kinds::KIND_THREAD_EXIT) is
/// written at the head of the sequence and every handover below it
/// has to be in the same ring. Seen failing with the retirement moved
/// to its old position above the heap teardown, where the window
/// answers commissions, a start and an exit and nothing after.
///
/// What it does **not** reach is the last step of all: the two
/// hand-drains sit between the heap teardown and the retirement, and
/// moving them below it would keep this green, the heap's own blocks
/// being recorded either way. That half stays where
/// [`the_exit_hands_back_the_reserve_and_the_block_cache_itself`]
/// leaves it.
#[cfg(feature = "debug-journal")]
#[test]
fn a_dying_threads_block_handovers_are_inside_its_own_ring() {
    // The default set, held: a test that quiets the sites would
    // otherwise turn them off underneath this one.
    let _sites = kinds::set_sites_for_test(kinds::DEFAULT_KINDS);
    let _g = crate::memory::block_pool::test_guard();
    let start = mark();

    let identity = std::thread::spawn(|| {
        crate::memory::heap::ll_thread_init();
        // Something to hand back: the reserve is filled at init, and
        // a small allocation and its free leave a block cached.
        let p = unsafe { crate::memory::stdapi::ll_malloc(64) };
        assert!(!p.is_null());
        unsafe { crate::memory::stdapi::ll_free(p) };
        let identity = this_thread_identity();
        crate::memory::heap::ll_thread_exit();
        identity
    })
    .join()
    .expect("the exiting thread panicked");

    let end = mark();
    assert_ne!(identity, 0, "the thread journaled nothing at all");
    let kinds_in_order: Vec<Kind> = events(between(&start, &end))
        .into_iter()
        .filter(|event| event.thread == identity)
        .map(|event| event.kind)
        .collect();

    let exited_at = kinds_in_order
        .iter()
        .position(|&kind| kind == kinds::KIND_THREAD_EXIT)
        .expect("the thread's own exit is not in its ring");
    let handovers = kinds_in_order[exited_at..]
        .iter()
        .filter(|&&kind| kind == kinds::KIND_BLOCK_DECOMMISSIONED)
        .count();
    assert!(
        handovers > 0,
        "the ring retired before the teardown handed its blocks back: {kinds_in_order:?}"
    );
}

/// The runtime's own block handovers are drained inside the exit, so
/// that they happen while there is still a ring to record them in.
/// What is left for the two destructors afterwards is nothing.
///
/// It pins the drain, not its **position**: the assertions are read
/// after the exit has returned, so moving the two calls below the
/// retirement keeps this green while reopening exactly the defect the
/// drain closed. What the record sites did make testable is the
/// **retirement's** own position —
/// [`a_dying_threads_block_handovers_are_inside_its_own_ring`], under
/// the `debug-journal` feature — and that is a different claim from
/// this one.
#[test]
fn the_exit_hands_back_the_reserve_and_the_block_cache_itself() {
    let _quiet = kinds::disable_sites_for_test();
    let _g = crate::memory::block_pool::test_guard();
    let (reserve, cache) = std::thread::spawn(|| {
        crate::memory::heap::ll_thread_init();
        // Something to hand back: the reserve is filled at init, and
        // a small allocation and its free leave a block cached.
        let p = unsafe { crate::memory::stdapi::ll_malloc(64) };
        assert!(!p.is_null());
        unsafe { crate::memory::stdapi::ll_free(p) };
        crate::memory::heap::ll_thread_exit();
        (
            crate::memory::reserve::blocks_held(),
            crate::memory::block_pool::thread_cache_len(),
        )
    })
    .join()
    .expect("the exiting thread panicked");

    assert_eq!(reserve, 0, "the exit left blocks in the reserve");
    assert_eq!(cache, 0, "the exit left blocks in the thread cache");
}

/// A record raised by a thread's *own* destructor after the runtime's
/// exit is either in the ring or counted as lost, and never neither.
///
/// A `thread_local!` registered before `ll_thread_init` is destroyed
/// after the runtime's guard wherever TLS goes in reverse
/// registration order, which is where this crate's own comment puts
/// glibc — so the record arrives with the ring already retired. Which
/// of the two answers comes back is the platform's to decide; that
/// one of them does is this module's.
///
/// The drop glue here is deliberate and is a test's own: the rule
/// against it (`dev/DECISIONS.md`, "thread exit owns the order its
/// per-thread state dies in") exists so that runtime structures do
/// not depend on TLS order, and this test depends on nothing but its
/// own cell.
#[test]
fn a_destructor_running_after_the_exit_is_recorded_or_counted() {
    let _quiet = kinds::disable_sites_for_test();
    const LATE: u64 = 0x1A7E;
    let _g = crate::memory::block_pool::test_guard();

    struct RecordOnDrop;
    impl Drop for RecordOnDrop {
        fn drop(&mut self) {
            record(ANY_KIND, 0, LATE, 0, 0);
        }
    }

    thread_local! {
        static LATE_CELL: RecordOnDrop = const { RecordOnDrop };
    }

    let start = mark();
    std::thread::spawn(|| {
        // Registered first, so destroyed last.
        LATE_CELL.with(|_| {});
        crate::memory::heap::ll_thread_init();
        record(ANY_KIND, 0, 0, 0, 0);
    })
    .join()
    .expect("the journaling thread panicked");
    let end = mark();

    let answers = between(&start, &end);
    let recorded = events(answers.clone())
        .into_iter()
        .any(|event| event.subject == LATE);
    // `Lost` names no thread — the ring that would have named it is
    // retired — so this is a count over the window, and what keeps it
    // this test's own is the guard serialising the journal's tests.
    let counted = answers
        .iter()
        .any(|window| matches!(window, Window::Lost { records } if *records >= 1));
    assert!(
        recorded || counted,
        "a record after the exit was neither kept nor counted: {answers:?}"
    );
}

/// A thread that has begun its own exit frees no evicted ring: the
/// exit disposes the structures a free reaches, so a ring given back
/// inside it has nowhere to land. The exit a caller invokes by hand is
/// the same sequence, which is what the exit guard's own state cannot
/// tell.
#[test]
fn a_thread_inside_its_own_exit_takes_no_ring_to_free() {
    let _quiet = kinds::disable_sites_for_test();
    const SUBJECT: u64 = 0xF2F2;
    let _g = crate::memory::block_pool::test_guard();
    let identity = a_journaling_thread(SUBJECT);
    // A delta rather than a total: the quota evicts other tests'
    // rings into this same list whenever the suite journals.
    let pending_before = pending_count();
    {
        let mut registry = locked();
        evict_retired(&mut registry, 1);
    }

    assert_eq!(
        pending_count(),
        pending_before + 1,
        "the eviction left nothing to free"
    );

    let taken = std::thread::spawn(|| {
        crate::memory::heap::ll_thread_init();
        crate::memory::heap::ll_thread_exit();
        // Both doors into the pending list go through this.
        let mut registry = locked();
        take_pending(&mut registry).len()
    })
    .join()
    .expect("the exiting thread panicked");

    assert_eq!(taken, 0, "a thread past its own exit took a ring to free");
    assert_eq!(
        pending_count(),
        pending_before + 1,
        "the ring was taken by somebody"
    );
    // This thread is live, so it is one that may.
    let pending = std::mem::take(&mut locked().pending_free);
    free_rings(pending);
    let _ = evict_retired_ring(identity);
}
