//! Several collectors dividing the mutators: a collector with a backlog after
//! two rounds births a sibling through the elder's birth path and names
//! half of its mutators to it; a mutator's poll wakes the collector its record
//! names; a sibling idle for the named rounds is ended by the elder, whose
//! next round names its mutators back to itself; a wake sent to an ended
//! sibling is lost and the count stands; and a cap of one births nothing.
//!
//! The second mutator is a thread of the case's that registers and polls on
//! its own thread through jobs the case sends it.

use super::*;
use crate::cycle::testing::Sent;
use crate::memory::arena::Arena;
use crate::object::Object;
use crate::refcount::{RcHeader, ll_release, ll_retain};

/// A registered thread the case drives by jobs, each run on that thread
/// with its arena; the thread lives until the case drops it.
struct Mutator {
    record: *mut MutatorRecord,
    jobs: std::sync::mpsc::Sender<Box<dyn FnOnce(&mut Arena) + Send>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Mutator {
    fn start() -> Self {
        let (jobs, inbox) = std::sync::mpsc::channel::<Box<dyn FnOnce(&mut Arena) + Send>>();
        let (tell, told) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            assert!(
                crate::memory::heap::ll_thread_init(),
                "the pool served the mutator thread"
            );
            tell.send(Sent(mutator_record::this_thread_record()))
                .expect("the case waits");
            let mut arena = Arena::new();
            while let Ok(job) = inbox.recv() {
                job(&mut arena);
            }

            crate::cycle::queue::release_queue_segments();
        });
        let record = told
            .recv()
            .expect("the mutator thread started")
            .into_inner();
        Self {
            record,
            jobs,
            thread: Some(thread),
        }
    }

    /// Run `job` on the mutator's thread and wait for its answer.
    fn run<T: Send + 'static>(&self, job: impl FnOnce(&mut Arena) -> T + Send + 'static) -> T {
        let (tell, told) = std::sync::mpsc::channel();
        self.jobs
            .send(Box::new(move |arena| {
                tell.send(Sent(job(arena))).expect("the case waits");
            }))
            .expect("the mutator thread runs");
        told.recv().expect("the job ran").into_inner()
    }
}

impl Drop for Mutator {
    fn drop(&mut self) {
        let (jobs, _) = std::sync::mpsc::channel();
        drop(std::mem::replace(&mut self.jobs, jobs));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Roots each mutator registers: the first two batches take one and two
/// starting sizes, K doubling after a completed batch, and leave three at
/// the threshold behind them.
const A_BACKLOG: usize = 6 * INITIAL_BATCH + 8;

/// Register `members` live objects of `class` on this thread, held by the
/// case: each is registered by its one release and read live by any trace.
unsafe fn live_roots(
    arena: &mut Arena,
    class: *const crate::class::Class,
    members: usize,
) -> Vec<Sent<*mut Object>> {
    let ring = unsafe { crate::cycle::testing::long_ring(arena, class, members) };
    for &member in &ring {
        unsafe { ll_retain(member as *mut RcHeader) };
    }

    ring.into_iter().map(Sent).collect()
}

/// Release what [`live_roots`] held, on the thread that holds it, and
/// collect the ring: the roots the collector read live stand in P or wait in
/// the deferred lane for the epoch's turn, so a poll and a re-offer come
/// first.
unsafe fn let_go_and_collect(roots: Vec<Sent<*mut Object>>) {
    let members = roots.len();
    for root in roots {
        assert!(
            !unsafe { ll_release(root.into_inner() as *mut RcHeader) },
            "an edge of the ring holds it"
        );
    }

    // A poll disposes of the verdicts standing in P — read live, they go to
    // the deferred lane — and the re-offer brings the lane back into R.
    let freed_at_the_poll = unsafe { crate::gc::ll_gc_maybe_collect() };
    crate::cycle::queue::reoffer_deferred_candidates();
    assert_eq!(
        freed_at_the_poll + unsafe { crate::gc::ll_gc_collect_cycles() },
        members,
        "the ring is collected whole"
    );
}

fn wait_for_rounds_of(index: usize, rounds: usize) {
    let mut seen = 0;
    assert!(
        wait_until(
            || {
                seen += testing::take_rounds_of(index);
                seen >= rounds
            },
            A_BIRTH
        ),
        "collector {index} made {rounds} rounds"
    );
}

#[test]
fn a_backlog_births_a_sibling_that_takes_half_the_mutators_and_is_ended_when_idle() {
    let _g = test_guard();
    let record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    let other = Mutator::start();
    let class = node_class("SiblingNode");
    let mut arena = Arena::new();
    let mine = unsafe { live_roots(&mut arena, class, A_BACKLOG) };
    let sent_class = Sent(class);
    let theirs =
        other.run(move |arena| unsafe { live_roots(arena, sent_class.into_inner(), A_BACKLOG) });
    testing::confine_rounds_to_records(&[record, other.record]);
    testing::wait_between_rounds_for(Some(PAST_THE_CASE));
    testing::permit_births(true);
    let _ = testing::take_spawns();
    let _ = testing::take_rounds();
    ensure_thread();
    wait_for_rounds_of(ELDER, 1);
    assert_eq!(testing::take_spawns(), 1);

    // The birth: the first round's batches left both mutators at the
    // threshold, the second's too, and the second births the sibling.
    assert!(wake(ELDER));
    wait_for_rounds_of(ELDER, 1);
    assert!(
        wait_until(
            || testing::collector_state(1) == ThreadState::Alive,
            A_BIRTH
        ),
        "two backlog rounds birthed a sibling"
    );
    assert_eq!(testing::take_spawns(), 1);
    assert_eq!(testing::collector_state(2), ThreadState::Unborn);
    // The sibling's birth round, over what the handover gave it, before the
    // rounds are counted for the routing.
    wait_for_rounds_of(1, 1);

    // The handover: of the two backlogged mutators, the second the round read
    // is named to the sibling; a record with no backlog — another thread's,
    // or one on the free list — is not in the handover at all.
    let named = [record, other.record].map(|record| unsafe { &*record }.collector());
    assert_eq!(
        named.iter().filter(|&&slot| slot == 1).count(),
        1,
        "{named:?}"
    );
    assert_eq!(
        named.iter().filter(|&&slot| slot == ELDER).count(),
        1,
        "{named:?}"
    );
    let (to_sibling, to_elder) = if named[0] == 1 {
        (record, other.record)
    } else {
        (other.record, record)
    };

    // The routing: a mutator's poll wakes the collector its record names and
    // no other. The roots are live, so the poll's disposition defers them
    // and fires nothing.
    let poll_on = |mutator: *mut MutatorRecord| {
        let poll = move || unsafe {
            crate::cycle::queue::make_a_signal_due();
            crate::gc::ll_gc_maybe_collect()
        };
        if mutator == record {
            poll()
        } else {
            other.run(move |_| poll())
        }
    };
    let _ = testing::take_rounds();
    assert_eq!(poll_on(to_sibling), 0);
    wait_for_rounds_of(1, 1);
    assert_eq!(testing::take_rounds_of(ELDER), 0, "the elder was not woken");
    assert_eq!(poll_on(to_elder), 0);
    wait_for_rounds_of(ELDER, 1);
    assert_eq!(testing::take_rounds_of(1), 0, "the sibling was not woken");

    // The end: the sibling's mutator lets its roots go and collects them, so
    // the sibling's rounds are empty; after the named number the elder ends
    // it, and the elder's next round names the mutator back to itself.
    let mut mine = Some(mine);
    let mut theirs = Some(theirs);
    let let_go_on = |mutator: *mut MutatorRecord, mine: Option<_>, theirs: Option<_>| {
        if mutator == record {
            unsafe { let_go_and_collect(mine.expect("held once")) };
        } else {
            let theirs = theirs.expect("held once");
            other.run(move |_| unsafe { let_go_and_collect(theirs) });
        }
    };
    if to_sibling == record {
        let_go_on(record, mine.take(), None);
    } else {
        let_go_on(other.record, None, theirs.take());
    }
    for _ in 0..IDLE_ROUNDS_TO_END {
        assert!(wake(1));
        wait_for_rounds_of(1, 1);
    }
    assert!(wake(ELDER));
    wait_for_rounds_of(ELDER, 1);
    assert!(
        wait_until(
            || testing::collector_state(1) == ThreadState::Unborn,
            A_BIRTH
        ),
        "the elder ended the idle sibling"
    );
    assert_eq!(
        unsafe { &*to_sibling }.collector(),
        1,
        "named until the elder's next round"
    );

    // The lost wake: a signal to the ended sibling is lost and the count
    // stands; the elder's round takes the mutator back, and the next signal
    // reaches the elder.
    assert!(!wake(1));
    assert_eq!(poll_on(to_sibling), 0);
    let signal_stands_on = |mutator: *mut MutatorRecord| {
        if mutator == record {
            crate::cycle::queue::signal_is_due()
        } else {
            other.run(|_| crate::cycle::queue::signal_is_due())
        }
    };
    assert!(signal_stands_on(to_sibling), "lost, and the flag stands");
    assert!(wake(ELDER));
    wait_for_rounds_of(ELDER, 1);
    assert_eq!(unsafe { &*to_sibling }.collector(), ELDER);
    let _ = testing::take_rounds();
    assert_eq!(poll_on(to_sibling), 0);
    assert!(!signal_stands_on(to_sibling), "received by the elder");
    wait_for_rounds_of(ELDER, 1);

    testing::retire();
    if mine.is_some() {
        let_go_on(record, mine.take(), None);
    } else {
        let_go_on(other.record, None, theirs.take());
    }
    drop(other);
    crate::cycle::queue::release_queue_segments();
}

#[test]
fn one_backlogged_mutator_births_no_sibling() {
    let _g = test_guard();
    let record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    let class = node_class("LoneBacklogNode");
    let mut arena = Arena::new();
    let mine = unsafe { live_roots(&mut arena, class, 2 * A_BACKLOG) };
    testing::confine_rounds_to(record);
    testing::wait_between_rounds_for(Some(PAST_THE_CASE));
    testing::permit_births(true);
    let _ = testing::take_spawns();
    let _ = testing::take_rounds();
    ensure_thread();
    wait_for_rounds_of(ELDER, 1);
    // One mutator is read by one collector at a time, so its backlog, however
    // many rounds it outlasts, is no reason for a sibling.
    for _ in 0..2 * BACKLOG_ROUNDS_TO_BIRTH {
        assert!(wake(ELDER));
        wait_for_rounds_of(ELDER, 1);
    }
    std::thread::sleep(A_ROUNDS_ABSENCE);
    assert_eq!(testing::take_spawns(), 1, "the elder alone");
    assert_eq!(testing::collector_state(1), ThreadState::Unborn);

    testing::retire();
    unsafe { let_go_and_collect(mine) };
    crate::cycle::queue::release_queue_segments();
}

#[test]
fn a_cap_of_one_births_no_sibling() {
    let _g = test_guard();
    let record = record();
    let _end = RetireOnDrop;
    reset_lanes();
    let other = Mutator::start();
    let class = node_class("CappedNode");
    let mut arena = Arena::new();
    let mine = unsafe { live_roots(&mut arena, class, A_BACKLOG) };
    let sent_class = Sent(class);
    let theirs =
        other.run(move |arena| unsafe { live_roots(arena, sent_class.into_inner(), A_BACKLOG) });
    set_collector_cap(1);
    testing::confine_rounds_to_records(&[record, other.record]);
    testing::wait_between_rounds_for(Some(PAST_THE_CASE));
    testing::permit_births(true);
    let _ = testing::take_spawns();
    let _ = testing::take_rounds();
    ensure_thread();
    wait_for_rounds_of(ELDER, 1);
    for _ in 0..BACKLOG_ROUNDS_TO_BIRTH {
        assert!(wake(ELDER));
        wait_for_rounds_of(ELDER, 1);
    }
    std::thread::sleep(A_ROUNDS_ABSENCE);
    assert_eq!(testing::take_spawns(), 1, "the elder alone");
    assert_eq!(testing::collector_state(1), ThreadState::Unborn);
    assert_eq!(unsafe { &*record }.collector(), ELDER);
    assert_eq!(unsafe { &*other.record }.collector(), ELDER);

    testing::retire();
    unsafe { let_go_and_collect(mine) };
    other.run(move |_| unsafe { let_go_and_collect(theirs) });
    drop(other);
    crate::cycle::queue::release_queue_segments();
}
