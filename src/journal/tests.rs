use super::*;

/// The kind a test writes when the kind plays no part: what these
/// tests are about is the ring, the window and the registry, and no
/// assertion here reads a kind. Past every kind that has a site
/// ([`kinds`]), and at the mask's last bit, so that a record written
/// here cannot be taken for a record of some site.
const ANY_KIND: Kind = 63;

/// How many rings are live and how many retired. Tests only, and the
/// live count is what a resurrected ring shows up in.
fn registry_counts() -> (usize, usize) {
    let registry = locked();
    (registry.live.len(), registry.retired.len())
}

/// Rings evicted and waiting for a live thread to free them.
fn pending_count() -> usize {
    locked().pending_free.len()
}

/// How many live rings and how many retired ones carry this
/// identity. Tests only, and the answer a test wants where the
/// registry's totals are somebody else's: `RETIRED_KEPT` bounds the
/// retired list, so a suite whose threads journal keeps it full and
/// a count of the whole list moves for reasons the test is not
/// about.
fn rings_named(thread: u64) -> (usize, usize) {
    let registry = locked();
    let carrying = |rings: &Vec<*mut Ring>| {
        rings
            .iter()
            .filter(|&&ring| unsafe { (*ring).thread } == thread)
            .count()
    };

    (carrying(&registry.live), carrying(&registry.retired))
}

/// Free one retired ring by identity, the way the quota's eviction
/// frees the oldest. Tests only: firing the quota takes
/// `RETIRED_KEPT + 1` threads and 2 MiB of rings to observe one line
/// of arithmetic, while what the tests below are about is what a
/// window says once a ring is gone.
fn evict_retired_ring(thread: u64) -> bool {
    let ring = {
        let mut registry = locked();
        match registry
            .retired
            .iter()
            .position(|&ring| unsafe { (*ring).thread } == thread)
        {
            Some(at) => {
                registry.evicted += 1;
                registry.retired.remove(at)
            }
            None => return false,
        }
    };

    unsafe { crate::memory::stdapi::ll_free(ring as *mut u8) };
    true
}

/// Every event the answers carry, in the order the windows came in.
fn events(windows: Vec<Window>) -> Vec<Event> {
    windows
        .into_iter()
        .flat_map(|window| match window {
            Window::Records(events) => events,
            _ => Vec::new(),
        })
        .collect()
}

/// A thread that journals one record and then exits through the whole
/// exit sequence, as a dying thread does. Returns its ring identity.
fn a_journaling_thread(subject: u64) -> u64 {
    std::thread::spawn(move || {
        crate::memory::heap::ll_thread_init();
        record(ANY_KIND, 0, subject, 0, 0);
        let identity = this_thread_identity();
        crate::memory::heap::ll_thread_exit();
        identity
    })
    .join()
    .expect("the journaling thread panicked")
}

/// A ring keeps the last `CAPACITY` records while its cursor counts
/// past the wrap, which is what makes a window an arithmetic
/// subtraction rather than a comparison of wrapped positions. Two
/// marks handed over the wrong way round bound no interval, and an
/// empty list of answers would read as "nothing happened anywhere",
/// so a reversed pair says so in an answer of its own.
mod the_ring_and_the_window_over_it {
    use super::*;

    /// A ring wraps and keeps the last `CAPACITY` records, and the cursor
    /// keeps counting past the wrap — which is what makes a window's
    /// arithmetic subtraction rather than a comparison of wrapped
    /// positions.
    #[test]
    fn a_ring_wraps_and_its_cursor_does_not() {
        let _quiet = kinds::disable_sites_for_test();
        let _g = crate::memory::block_pool::test_guard();
        let ring = allocate_ring();
        assert!(!ring.is_null());
        let ring = unsafe { &*ring };

        for i in 0..(CAPACITY as u64 + 3) {
            ring.write(ANY_KIND, 0, i, 0, 0);
        }

        assert_eq!(ring.cursor.load(Ordering::Relaxed), CAPACITY as u64 + 3);
        // The three newest are readable and name the last three subjects.
        for (offset, subject) in [
            (3, CAPACITY as u64),
            (2, CAPACITY as u64 + 1),
            (1, CAPACITY as u64 + 2),
        ] {
            let at = ring.cursor.load(Ordering::Relaxed) - offset;
            assert_eq!(
                ring.read_at(at).expect("still inside the ring").subject,
                subject
            );
        }

        // The record that was lapped is gone rather than stale.
        assert!(ring.read_at(0).is_none(), "a lapped position read as live");

        unsafe { crate::memory::stdapi::ll_free(ring as *const Ring as *mut u8) };
    }

    /// The window is the cursor pair, so a record written before the
    /// first mark is outside it and one written after is inside — which
    /// is the whole of "what happened between these two moments".
    #[test]
    fn a_cursor_pair_names_exactly_what_was_written_inside_it() {
        let _quiet = kinds::disable_sites_for_test();
        let _g = crate::memory::block_pool::test_guard();
        const BEFORE: u64 = 0x0B4;
        const FIRST_INSIDE: u64 = 0x1_11;
        const SECOND_INSIDE: u64 = 0x2_22;
        const AFTER: u64 = 0x0AF;

        record(ANY_KIND, 0, BEFORE, 0, 0);
        let start = mark();
        record(ANY_KIND, 0, FIRST_INSIDE, 1, 0);
        record(ANY_KIND, 0, SECOND_INSIDE, 2, 0);
        let end = mark();
        record(ANY_KIND, 0, AFTER, 0, 0);

        let mine = this_thread_identity();
        let inside: Vec<u64> = events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == mine)
            .map(|event| event.subject)
            .collect();
        assert_eq!(inside, vec![FIRST_INSIDE, SECOND_INSIDE]);
        retire_thread_ring();
    }

    /// A window's two ends handed over the wrong way round bound no
    /// interval, and an empty list of answers reads as "nothing happened
    /// anywhere" — the one answer this module may not invent. It says so
    /// in one answer of its own, in the release build too, where an
    /// assertion would have been compiled out, and whether or not any
    /// ring is named: a pair taken before anything journaled names none,
    /// and a per-ring answer over no rings is the empty list again.
    #[test]
    fn two_marks_in_the_wrong_order_answer_that_they_bound_nothing() {
        let _quiet = kinds::disable_sites_for_test();
        let _g = crate::memory::block_pool::test_guard();
        record(ANY_KIND, 0, 0x0D, 0, 0);
        let start = mark();
        let end = mark();

        assert_eq!(between(&end, &start), vec![Window::Reversed]);
        retire_thread_ring();
    }

    /// The same, with no ring in either mark — where a per-ring answer
    /// has nothing to answer over and the empty list comes back.
    #[test]
    fn a_reversed_pair_naming_no_ring_still_answers() {
        let _quiet = kinds::disable_sites_for_test();
        let _g = crate::memory::block_pool::test_guard();
        let start = Mark {
            positions: Vec::new(),
            evictions: 0,
            refusals: 0,
            lost: 0,
            taken: 1,
        };

        let end = Mark {
            positions: Vec::new(),
            evictions: 0,
            refusals: 0,
            lost: 0,
            taken: 2,
        };

        assert_eq!(between(&end, &start), vec![Window::Reversed]);
    }
}

/// Turning unknown into none is what this module exists to prevent.
/// An overflowed window answers `unknown`; a ring the registry freed
/// inside the window is reported rather than dropped from the
/// answer; and a mark names rings by identity, so a freed one is
/// never read through an address the allocator has handed to
/// somebody else. An answer already given does not change under a
/// later close.
mod the_answer_a_window_may_not_invent {
    use super::*;

    /// An overflowed window answers `unknown`, and that is the point of
    /// the whole mechanism: the hunt it exists for turned on "no string
    /// died inside the window", and a silent eviction would have made
    /// that finding false.
    #[test]
    fn an_overflowed_window_answers_unknown_rather_than_none() {
        let _quiet = kinds::disable_sites_for_test();
        let _g = crate::memory::block_pool::test_guard();
        let start = mark();
        for i in 0..(CAPACITY as u64 * 2) {
            record(ANY_KIND, 0, i, 0, 0);
        }

        let end = mark();

        let mine = this_thread_identity();
        let mut seen = false;
        for window in between(&start, &end) {
            if let Window::Unknown { thread, written } = window
                && thread == mine
            {
                assert_eq!(written, CAPACITY as u64 * 2);
                seen = true;
            }
        }

        assert!(
            seen,
            "an overflowed ring reported records instead of unknown"
        );
        retire_thread_ring();
    }

    /// A ring the registry frees inside a window takes a whole thread's
    /// history with it, and the window has to say so: reporting the rings
    /// that are left is the conversion of *unknown* into *none* this
    /// module exists to prevent.
    #[test]
    fn a_ring_freed_inside_the_window_is_reported_rather_than_missing() {
        let _quiet = kinds::disable_sites_for_test();
        const SUBJECT: u64 = 0x105E;
        let _g = crate::memory::block_pool::test_guard();

        let identity = a_journaling_thread(SUBJECT);
        let start = mark();
        assert!(
            evict_retired_ring(identity),
            "the exited thread's ring was not on the retired list"
        );
        let end = mark();

        assert!(
            between(&start, &end)
                .into_iter()
                .any(|window| window == Window::Evicted { rings: 1 }),
            "a ring freed inside the window left no trace in it"
        );
    }

    /// A mark names rings by identity, so a ring freed after it is taken
    /// is reported as unknown rather than read through an address the
    /// allocator has handed to somebody else.
    ///
    /// What it pins reliably is the *repair*. On the defect it failed
    /// because the freed block still held its old contents and the read
    /// reported records; that is a use-after-free, so the shape of the
    /// failure was the allocator's to decide and Miri is what names it as
    /// one.
    #[test]
    fn a_ring_freed_after_the_mark_is_not_read_through_its_address() {
        let _quiet = kinds::disable_sites_for_test();
        const SUBJECT: u64 = 0x5EE;
        let _g = crate::memory::block_pool::test_guard();

        let start = mark();
        let identity = a_journaling_thread(SUBJECT);
        let end = mark();
        assert!(
            evict_retired_ring(identity),
            "the exited thread's ring was not on the retired list"
        );

        let answer = between(&start, &end)
            .into_iter()
            .find(|window| matches!(window, Window::Unknown { thread, .. } if *thread == identity));
        assert_eq!(
            answer,
            Some(Window::Unknown {
                thread: identity,
                written: 1
            }),
            "a freed ring was read rather than reported"
        );
    }

    /// A window that ended before a ring closed keeps its answer when the
    /// thread later exits. [`Window::Lost`] says "records were raised and
    /// dropped inside this window", and its count is the difference
    /// between the two marks' readings of `LOST`, so a close after the
    /// second mark adds nothing to it — an answer that changes under a
    /// reader's feet is one that stops meaning anything.
    #[test]
    fn a_window_that_ended_before_the_close_is_not_reclassified_by_it() {
        let _quiet = kinds::disable_sites_for_test();
        const SUBJECT: u64 = 0xC105;
        let _g = crate::memory::block_pool::test_guard();

        let start = mark();
        let (sender, receiver) = std::sync::mpsc::channel();
        let (go, wait) = std::sync::mpsc::channel();
        let journaling = std::thread::spawn(move || {
            crate::memory::heap::ll_thread_init();
            record(ANY_KIND, 0, SUBJECT, 0, 0);
            sender
                .send(this_thread_identity())
                .expect("the test hung up");
            wait.recv().expect("the test hung up");
            crate::memory::heap::ll_thread_exit();
        });

        let identity = receiver.recv().expect("the journaling thread hung up");
        let end = mark();

        // Both marks are taken while the thread is alive, so the window
        // is complete. Only then does the thread exit.
        go.send(()).expect("the journaling thread hung up");
        journaling.join().expect("the journaling thread panicked");

        let answer = between(&start, &end)
            .into_iter()
            .find(|window| match window {
                Window::Records(events) => events.iter().any(|event| event.thread == identity),
                _ => false,
            })
            .expect("the window lost the thread it was taken around");
        assert!(
            matches!(answer, Window::Records(_)),
            "a window that was complete stopped being one: {answer:?}"
        );
    }
}

/// A thread's records matter most once it is gone, so a retired ring
/// stays readable and stays in the window, and the retired list is
/// bounded at `RETIRED_KEPT`, dropping the oldest. A record arriving
/// after the exit finds the cell closed rather than opening a second
/// ring under the same identity, while `ll_thread_init` reopens it
/// for a pool thread's next life. The eviction itself frees no ring:
/// it waits for a live thread to take one, and an investigator
/// taking a mark is such a thread.
mod a_ring_across_a_threads_life {
    use super::*;

    /// A thread's records matter most once it is gone — the census flake
    /// this journal was designed for is a hypothesis about a *finishing*
    /// thread — so a retired ring stays readable and stays in the window.
    #[test]
    fn a_retired_threads_ring_is_still_read_by_a_window() {
        let _quiet = kinds::disable_sites_for_test();
        const SUBJECT: u64 = 0xDEAD;
        let _g = crate::memory::block_pool::test_guard();
        let start = mark();

        let joined = a_journaling_thread(SUBJECT);

        let end = mark();
        let found = events(between(&start, &end))
            .into_iter()
            .any(|event| event.thread == joined && event.subject == SUBJECT);
        assert!(found, "the exited thread's ring left the window with it");
    }

    /// Thread exit is not the last thing a dying thread does: the heap
    /// teardown that follows the journal's own step decommissions blocks,
    /// which is a default event kind. A record from there must find the
    /// thread closed rather than open a second ring under the same
    /// identity — one nothing would ever retire, and one that would make
    /// `RETIRED_KEPT` bound a list the leak is not on.
    #[test]
    fn a_thread_that_journals_after_its_exit_starts_no_second_ring() {
        let _quiet = kinds::disable_sites_for_test();
        const BEFORE_EXIT: u64 = 0xE1;
        const AFTER_EXIT: u64 = 0xE2;
        let _g = crate::memory::block_pool::test_guard();
        let (live_before, _) = registry_counts();
        let start = mark();

        let identity = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            record(ANY_KIND, 0, BEFORE_EXIT, 0, 0);
            let identity = this_thread_identity();
            crate::memory::heap::ll_thread_exit();
            record(ANY_KIND, 0, AFTER_EXIT, 0, 0);
            identity
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        let (live_after, _) = registry_counts();
        assert_eq!(
            live_after, live_before,
            "the exited thread left a live ring behind"
        );
        assert_eq!(
            rings_named(identity),
            (0, 1),
            "the thread ended with a number of rings other than its one, retired"
        );

        // The post-exit record is not in the ring, and it is not silent
        // either: it is counted, because a window that carried neither
        // the record nor a word about it would say the thread stopped
        // after `BEFORE_EXIT`, which is not what happened.
        let answers = between(&start, &end);
        let subjects: Vec<u64> = events(answers.clone())
            .into_iter()
            .filter(|event| event.thread == identity)
            .map(|event| event.subject)
            .collect();
        assert_eq!(subjects, vec![BEFORE_EXIT]);
        assert!(
            answers
                .iter()
                .any(|window| matches!(window, Window::Lost { records } if *records >= 1)),
            "the record raised after the exit was dropped without a word: {answers:?}"
        );
    }

    /// A pool thread runs `ll_thread_init` and `ll_thread_exit` once per
    /// task, so one OS thread is a sequence of thread lives. The second
    /// life journals into a ring of its own: without that it journals
    /// nothing at all and looks exactly like a thread doing nothing.
    #[test]
    fn a_second_life_on_one_thread_journals_into_a_ring_of_its_own() {
        let _quiet = kinds::disable_sites_for_test();
        const FIRST: u64 = 0x11FE;
        const SECOND: u64 = 0x21FE;
        let _g = crate::memory::block_pool::test_guard();
        let start = mark();

        let (first, second) = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            record(ANY_KIND, 0, FIRST, 0, 0);
            let first = this_thread_identity();
            crate::memory::heap::ll_thread_exit();

            crate::memory::heap::ll_thread_init();
            record(ANY_KIND, 0, SECOND, 0, 0);
            let second = this_thread_identity();
            crate::memory::heap::ll_thread_exit();
            (first, second)
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        assert_ne!(first, second, "the second life reused the first's ring");
        assert_ne!(second, 0, "the second life journaled nothing");
        let subjects: Vec<u64> = events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == first || event.thread == second)
            .map(|event| event.subject)
            .collect();
        assert_eq!(subjects, vec![FIRST, SECOND]);
    }

    /// The retired list stops growing at [`RETIRED_KEPT`], and what it
    /// drops is the oldest. That bound is the only thing between a
    /// program that spawns a thread per request and a ring per request
    /// held for the life of the process.
    ///
    /// Read per identity through [`rings_named`], because the list is
    /// process-global and a thread exiting elsewhere in the suite retires
    /// a ring into it while this test counts.
    #[test]
    fn the_retired_list_keeps_the_newest_and_drops_the_oldest() {
        let _quiet = kinds::disable_sites_for_test();
        let _g = crate::memory::block_pool::test_guard();

        let mut mine = Vec::new();
        let mut freed = Vec::new();
        for _ in 0..=RETIRED_KEPT {
            let ring = allocate_ring();
            assert!(!ring.is_null());
            free_rings(register_ring(ring));
            mine.push(unsafe { (*ring).thread });
            retire_ring(ring);
            let evicted = std::mem::take(&mut locked().pending_free);
            for old in evicted {
                freed.push(unsafe { (*old).thread });
                unsafe { crate::memory::stdapi::ll_free(old as *mut u8) };
            }
        }

        let (_, retired_after) = registry_counts();
        assert_eq!(
            retired_after, RETIRED_KEPT,
            "the retired list outgrew its bound"
        );
        // Per identity, never by the list's totals. Another thread
        // retiring a ring inside the loop above changes how many rings
        // were dropped and which ones, and `test_guard()` does not
        // serialise a retirement; what it cannot change is that the
        // oldest of this test's own went and the rest stayed.
        assert!(
            freed.contains(&mine[0]),
            "the oldest of this test's rings was not dropped"
        );
        assert_eq!(
            rings_named(mine[0]).1,
            0,
            "the ring reported dropped is still on the list"
        );
        for thread in &mine[1..] {
            assert_eq!(
                rings_named(*thread).1,
                1,
                "the list dropped a ring newer than its oldest"
            );
        }

        // Leave the list as it was found, so that a later test's window
        // does not carry this one's evictions.
        for thread in mine.into_iter().skip(1) {
            assert!(
                evict_retired_ring(thread),
                "a ring of this test went missing"
            );
        }
    }

    /// A retiring thread frees no ring, because by then it cannot: its
    /// parked backlog is gone, and a ring's free parks while a collector
    /// epoch is in flight. The rings wait for a thread that is not on its
    /// way out — an investigator taking a mark is one.
    #[test]
    fn an_evicted_ring_is_freed_by_a_live_thread_rather_than_a_dying_one() {
        let _quiet = kinds::disable_sites_for_test();
        const SUBJECT: u64 = 0xEB0;
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
            "the eviction freed on the spot instead of leaving the ring"
        );

        let _ = mark();
        assert_eq!(pending_count(), 0, "a mark left the eviction unfreed");
        // The oldest retired ring is whichever test ran before this one,
        // so the eviction above may have taken that rather than this
        // test's. Leave nothing of its own behind either way.
        let _ = evict_retired_ring(identity);
    }
}

/// The ring is retired by the last act of `ll_thread_exit`, so a
/// `__destruct` body's records and every block handover are inside a
/// window over the thread's death — the reserve and the pool's
/// thread cache are drained there by hand for that reason. Past that
/// act completeness ends and honesty does not: a record from a TLS
/// destructor running later is in the ring or counted as lost, never
/// neither.
mod where_the_retirement_sits_inside_the_exit {
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
            let obj =
                unsafe { crate::object::new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
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
    /// against it (`dev/DECISIONS.md`, 2026-08-03) exists so that runtime
    /// structures do not depend on TLS order, and this test depends on
    /// nothing but its own cell.
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

    /// A thread that has begun its own exit frees no evicted ring. Its
    /// deferral backlog is disposed inside that sequence and nothing
    /// rebuilds it, so a parked free there is dropped unreleased — and
    /// the exit a caller invokes by hand is the same sequence, which is
    /// what the exit guard's own state cannot tell.
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
}

/// A refused allocation closes the thread instead of queueing a
/// retry, which would take two process-global mutexes and ask the OS
/// for a block on every later record — under exactly the memory
/// pressure the journal is turned on to investigate. A thread that
/// cannot arm its exit guard is refused the same way, a ring nothing
/// retires staying on the live list for the life of the process.
/// Such a thread is in no window, so it is counted: the count is
/// what keeps its silence from reading as inactivity, and its later
/// records are not counted as losses on top of it.
mod a_thread_the_journal_could_not_serve {
    use super::*;

    /// A refused allocation closes the thread instead of queueing a
    /// retry. Retrying would take two process-global mutexes and ask the
    /// OS for a block on every later record — under the memory pressure
    /// the journal was turned on to investigate, which is where §9.7's
    /// "no allocation, no lock" is worth the most.
    #[test]
    fn a_refused_ring_is_not_asked_for_a_second_time() {
        let _quiet = kinds::disable_sites_for_test();
        use crate::memory::block_pool::FORCE_OOM;
        let _g = crate::memory::block_pool::test_guard();
        let counts_before = registry_counts();

        let identity = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            FORCE_OOM.store(true, Ordering::Relaxed);
            record(ANY_KIND, 0, 1, 0, 0);
            FORCE_OOM.store(false, Ordering::Relaxed);
            // The pressure is gone and this thread still journals
            // nothing: the refusal was final, not a bad moment.
            record(ANY_KIND, 0, 2, 0, 0);
            let identity = this_thread_identity();
            crate::memory::heap::ll_thread_exit();
            identity
        })
        .join()
        .expect("the journaling thread panicked");

        assert_eq!(identity, 0, "a refused thread ended up with a ring");
        // The retired count, not the live one: the thread ran its exit,
        // so a ring it did get would have moved off the live list before
        // this line and left that count telling nothing.
        assert_eq!(
            registry_counts(),
            counts_before,
            "a refused ring was asked for again, and granted"
        );
    }

    /// A thread that cannot arm its exit guard gets no ring: the guard is
    /// what retires one, and a ring nothing retires stays on the live
    /// list for the life of the process, where every later window reads
    /// it as a live thread doing nothing. The state is real — a
    /// destructor that allocates reaches a record site with the guard's
    /// slot already destroyed — and it is counted like a refusal, being
    /// the same silence from the reader's side.
    #[test]
    fn a_thread_that_cannot_arm_its_exit_guard_is_given_no_ring() {
        let _quiet = kinds::disable_sites_for_test();
        use crate::memory::heap::FORCE_GUARD_UNARMED;
        let _g = crate::memory::block_pool::test_guard();
        let counts_before = registry_counts();
        let start = mark();

        let identity = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            FORCE_GUARD_UNARMED.store(true, Ordering::Relaxed);
            record(ANY_KIND, 0, 5, 0, 0);
            let identity = this_thread_identity();
            FORCE_GUARD_UNARMED.store(false, Ordering::Relaxed);
            crate::memory::heap::ll_thread_exit();
            identity
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        assert_eq!(identity, 0, "a thread with no exit guard opened a ring");
        assert_eq!(
            registry_counts(),
            counts_before,
            "a ring nothing will retire was registered anyway"
        );
        let reported = between(&start, &end)
            .into_iter()
            .find_map(|window| match window {
                Window::Refused { threads } => Some(threads),
                _ => None,
            });

        assert_eq!(
            reported,
            Some(start.refusals + 1),
            "the thread left no trace in the window that covered it"
        );
    }

    /// A refused thread's later records are not counted as losses. Its
    /// silence is already reported for the whole of its life by
    /// [`Window::Refused`], and counting every record it goes on to raise
    /// would mark every window it runs through as having lost something —
    /// the degradation the per-window difference exists to avoid, through
    /// a second door.
    #[test]
    fn a_refused_threads_later_records_are_not_counted_as_losses() {
        let _quiet = kinds::disable_sites_for_test();
        use crate::memory::block_pool::FORCE_OOM;
        let _g = crate::memory::block_pool::test_guard();

        let (announce, announced) = std::sync::mpsc::channel();
        let (go, wait) = std::sync::mpsc::channel();
        let refused = std::thread::spawn(move || {
            crate::memory::heap::ll_thread_init();
            FORCE_OOM.store(true, Ordering::Relaxed);
            record(ANY_KIND, 0, 1, 0, 0);
            FORCE_OOM.store(false, Ordering::Relaxed);
            announce.send(()).expect("the test hung up");

            wait.recv().expect("the test hung up");
            // Raised while refused, and inside the window below.
            record(ANY_KIND, 0, 2, 0, 0);
            announce.send(()).expect("the test hung up");

            wait.recv().expect("the test hung up");
            crate::memory::heap::ll_thread_exit();
        });

        announced.recv().expect("the thread hung up");
        let start = mark();
        go.send(()).expect("the thread hung up");
        announced.recv().expect("the thread hung up");
        let end = mark();
        go.send(()).expect("the thread hung up");
        refused.join().expect("the refused thread panicked");
        assert!(
            !between(&start, &end)
                .iter()
                .any(|window| matches!(window, Window::Lost { .. })),
            "a refused thread's records were counted as losses"
        );
    }

    /// A thread whose ring the allocator refused is in no window at all,
    /// so the count of such threads is the only thing standing between a
    /// reader and the conclusion that they did nothing.
    #[test]
    fn a_thread_refused_a_ring_is_counted_since_it_is_in_no_window() {
        let _quiet = kinds::disable_sites_for_test();
        use crate::memory::block_pool::FORCE_OOM;
        let _g = crate::memory::block_pool::test_guard();
        let start = mark();

        std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            FORCE_OOM.store(true, Ordering::Relaxed);
            record(ANY_KIND, 0, 3, 0, 0);
            FORCE_OOM.store(false, Ordering::Relaxed);
            crate::memory::heap::ll_thread_exit();
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        let reported = between(&start, &end)
            .into_iter()
            .find_map(|window| match window {
                Window::Refused { threads } => Some(threads),
                _ => None,
            });

        assert_eq!(
            reported,
            Some(start.refusals + 1),
            "a refused thread left no trace in the window that covered it"
        );
    }
}

/// The acceptance question of 2026-08-06, answered from journal
/// reads and nothing else: which strings died inside this window.
/// What it replaces is a ring of `(thread, address)` written by hand
/// for that one question. It needs the record sites, so it is the
/// `debug-journal` build's alone.
#[cfg(feature = "debug-journal")]
mod the_hunt_the_journal_was_built_for {
    use super::*;

    /// The journal's acceptance criterion, `dev/design/debug-modes.md` §9.
    ///
    /// The four strings are created before any of them dies, so that the
    /// four addresses are distinct while the window is being marked: a
    /// death frees the slot, and the next string born there would answer
    /// under the same address. Deaths are then read back per ring, which
    /// is the other half of the same care — an address is only a name
    /// while its thread is the one that wrote it.
    ///
    /// The trustworthy *none* is the point, so the two rings that matter
    /// are checked to have answered with records rather than with
    /// `Unknown`: a hunt that concludes "no string died" from a lapped
    /// ring has concluded nothing.
    #[test]
    fn which_strings_died_inside_the_window_is_answered_from_the_journal() {
        use crate::refcount::{EntityKind, MemoryCategory, RcHeader};
        let _sites = kinds::set_sites_for_test(kinds::DEFAULT_KINDS);
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

        let make = |ctx: &mut crate::memory::context::LLContext, bytes: &[u8]| unsafe {
            let s = crate::string::ll_string_new(ctx, MemoryCategory::GcHeap, bytes);
            assert!(!s.is_null());
            s
        };

        let kill = |s: *mut crate::string::LLString| unsafe {
            assert!(crate::refcount::ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        };

        let early = make(&mut ctx, b"before");
        let inside = make(&mut ctx, b"inside");
        let survivor = make(&mut ctx, b"survives");
        let late = make(&mut ctx, b"after");
        kill(early);

        let start = mark();
        kill(inside);
        let here = this_thread_identity();
        let (there, elsewhere) = std::thread::spawn(move || {
            crate::memory::heap::ll_thread_init();
            let mut arena = crate::memory::arena::Arena::new();
            let mut ctx = crate::memory::context::LLContext { arena: &mut arena };
            let s = unsafe {
                crate::string::ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"elsewhere")
            };

            assert!(!s.is_null());
            let identity = this_thread_identity();
            unsafe {
                assert!(crate::refcount::ll_release(s as *mut RcHeader));
                crate::object::ll_entity_die(s as *mut RcHeader);
            }

            (identity, s as u64)
        })
        .join()
        .expect("the second thread panicked");
        let end = mark();
        kill(late);

        let answers = between(&start, &end);
        for identity in [here, there] {
            assert_ne!(identity, 0, "a thread of this test journaled nothing");
            let lapped = answers.iter().any(
                |window| matches!(window, Window::Unknown { thread, .. } if *thread == identity),
            );
            assert!(!lapped, "ring {identity} could not answer for the window");
        }

        let died: Vec<u64> = events(answers)
            .into_iter()
            .filter(|event| event.thread == here || event.thread == there)
            .filter(|event| {
                event.kind == kinds::KIND_ENTITY_DEATH && event.a == EntityKind::String as u64
            })
            .map(|event| event.subject)
            .collect();

        assert!(
            died.contains(&(inside as u64)) && died.contains(&elsewhere),
            "a string that died inside the window is missing from it: {died:x?}"
        );
        assert!(
            !died.contains(&(early as u64)),
            "a string that died before the window is inside it"
        );
        assert!(
            !died.contains(&(survivor as u64)) && !died.contains(&(late as u64)),
            "a string that outlived the window is in it"
        );

        kill(survivor);
    }
}
