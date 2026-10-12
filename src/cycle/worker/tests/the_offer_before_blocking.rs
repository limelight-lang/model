//! The poll before a call that may block, under `gc-window`
//! (`crate::gc::ll_gc_before_blocking`; `dev/design/recycler-over-counts.md`,
//! §5g): R offered whatever its count and the young cut, for the collector to
//! test while the thread sleeps; a batch posted before it disposed of first;
//! nothing offered over a batch in flight, nor inside the pacing interval.

use super::*;
use crate::cycle::offer::{offer_at, testing::note_an_offer_before_blocking_at};
use crate::cycle::token::{FREE, OFFERED, state};
use crate::gc::{ll_gc_before_blocking, ll_gc_maybe_collect};
use crate::memory::arena::Arena;
use std::time::{Duration, Instant};

fn token() -> &'static crate::cycle::token::TraceToken {
    unsafe { &(*record()).token }
}

/// Register `rings` garbage rings of two members each on this thread, their
/// class with no destructor, so that the collector frees them itself.
fn garbage_rings(arena: &mut Arena, rings: usize, name: &str) {
    let class = node_class(name);
    for _ in 0..rings {
        let _ = unsafe { crate::cycle::testing::ring(arena, [class, class]) };
    }
}

/// The young cut at `cut` while the guard stands.
struct YoungCut;

impl YoungCut {
    fn of(cut: Duration) -> Self {
        crate::cycle::young_cut::testing::reset_the_clock();
        crate::cycle::young_cut::testing::cut_at(Some(cut));
        Self
    }
}

impl Drop for YoungCut {
    fn drop(&mut self) {
        crate::cycle::young_cut::testing::cut_at(None);
        crate::cycle::young_cut::testing::reset_the_clock();
    }
}

/// The members the collector freed so far, process-wide.
fn members_freed() -> usize {
    crate::cycle::collector_frees::frees_counts().members
}

/// Whether the collector freed `members` more than `before` within
/// [`A_BIRTH`], the thread sleeping meanwhile with no poll and no reading of
/// its byte, as a thread blocked in a call does.
fn freed_while_asleep(before: usize, members: usize) -> bool {
    let deadline = Instant::now() + A_BIRTH;
    while members_freed() - before < members {
        if Instant::now() > deadline {
            return false;
        }

        std::thread::sleep(Duration::from_millis(1));
    }

    true
}

/// The frame of a case with the elder to be born: this thread's record the
/// only one its rounds visit, R offered by nothing but the case's calls.
fn ready() -> RetireOnDrop {
    reset_lanes();
    let end = ready_for_a_birth();
    note_an_offer_before_blocking_at(None);
    end
}

/// Three rings, six roots and six members, younger than the cut and under
/// every threshold: the poll offers nothing; the call offers them, and the
/// collector frees them while the thread sleeps with no poll; the next poll
/// applies the frees.
#[test]
fn a_young_r_under_every_threshold_is_freed_while_the_thread_sleeps() {
    let _g = test_guard();
    let _end = ready();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let _cut = YoungCut::of(Duration::from_millis(100));
    let mut arena = Arena::new();
    garbage_rings(&mut arena, 3, "BeforeBlockingYoungNode");

    let _ = unsafe { ll_gc_maybe_collect() };
    assert_eq!(
        state(token().read()),
        FREE,
        "the poll offers a young R nothing"
    );

    let before = members_freed();
    let _ = unsafe { ll_gc_before_blocking() };
    assert_ne!(state(token().read()), FREE, "the call offered R");
    assert!(
        freed_while_asleep(before, 6),
        "the collector freed the three rings"
    );
    let mut applied = 0;
    assert!(
        wait_until(
            || {
                applied += unsafe { ll_gc_maybe_collect() };
                applied == 6
            },
            A_BIRTH
        ),
        "the polls after the sleep applied the six members"
    );
    reset_lanes();
}

/// A batch the collector posted before the call is disposed of by it, its
/// frees applied and its byte released, and R behind it offered at once.
#[test]
fn a_batch_posted_before_the_call_is_disposed_of_and_r_offered() {
    let _g = test_guard();
    let _end = ready();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    garbage_rings(&mut arena, 1, "BeforeBlockingPostedNode");

    let before = members_freed();
    let _ = unsafe { ll_gc_before_blocking() };
    assert!(freed_while_asleep(before, 2), "the first ring freed");
    assert!(
        wait_until_without_a_poll(|| unsafe { &*record() }.collectors_frees_stand(), A_BIRTH),
        "its frees stand for the owner"
    );

    garbage_rings(&mut arena, 1, "BeforeBlockingPostedNode");
    note_an_offer_before_blocking_at(None);
    let applied = unsafe { ll_gc_before_blocking() };
    assert_eq!(applied, 2, "the call applied the first ring's frees");
    assert_ne!(state(token().read()), FREE, "and offered the second ring");
    assert!(freed_while_asleep(before + 2, 2), "the second ring freed");
    let mut applied = 0;
    assert!(
        wait_until(
            || {
                applied += unsafe { ll_gc_maybe_collect() };
                applied == 2
            },
            A_BIRTH
        ),
        "applied after the sleep"
    );
    reset_lanes();
}

/// Inside the pacing interval of the last offer before blocking the call
/// offers nothing; past it, it offers.
#[test]
fn a_second_call_inside_the_interval_offers_nothing() {
    let _g = test_guard();
    let _end = ready();
    let _interval = StandingInterval::of(Duration::from_secs(60));
    let mut arena = Arena::new();
    garbage_rings(&mut arena, 1, "BeforeBlockingPacedNode");

    note_an_offer_before_blocking_at(Some(Instant::now()));
    let _ = unsafe { ll_gc_before_blocking() };
    assert_eq!(state(token().read()), FREE, "paced: no offer");

    note_an_offer_before_blocking_at(Some(Instant::now() - Duration::from_millis(60)));
    let before = members_freed();
    let _ = unsafe { ll_gc_before_blocking() };
    assert_ne!(state(token().read()), FREE, "past the interval: offered");
    assert!(freed_while_asleep(before, 2));
    let mut applied = 0;
    assert!(wait_until(
        || {
            applied += unsafe { ll_gc_maybe_collect() };
            applied == 2
        },
        A_BIRTH
    ));
    reset_lanes();
}

/// A batch in flight at the call — the byte `OFFERED`, no collector to take
/// it — is left as it stands: the call offers nothing more and returns at
/// once.
#[test]
fn a_batch_in_flight_is_left_and_the_call_waits_for_nothing() {
    let _g = test_guard();
    reset_lanes();
    note_an_offer_before_blocking_at(None);
    let mut arena = Arena::new();
    garbage_rings(&mut arena, 2, "BeforeBlockingInFlightNode");
    assert!(unsafe { offer_at(4) }, "a batch of four roots offered");
    let window = crate::refcount::this_threads_window();

    let from = Instant::now();
    let _ = unsafe { ll_gc_before_blocking() };
    assert!(from.elapsed() < Duration::from_secs(1), "no wait");
    assert_eq!(state(token().read()), OFFERED, "the batch still offered");
    assert_eq!(token().ceiling(), 4, "as it was");
    assert_eq!(
        crate::refcount::this_threads_window(),
        window,
        "no frame spent"
    );

    assert_eq!(token().withdraw_the_offer(FREE), Ok(()));
    let _ = unsafe { crate::gc::ll_gc_collect_cycles() };
    reset_lanes();
}

/// A thread with no record frees and offers nothing.
#[test]
fn a_thread_with_no_record_does_nothing() {
    let _g = test_guard();
    let freed = std::thread::spawn(|| unsafe { ll_gc_before_blocking() })
        .join()
        .expect("the call returned");
    assert_eq!(freed, 0);
}
