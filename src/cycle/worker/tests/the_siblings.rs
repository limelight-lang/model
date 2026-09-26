//! Several collectors dividing the mutators: a collector with a backlog after
//! two rounds births a sibling through the elder's birth path and names
//! half of its mutators to it; a mutator's poll wakes the collector its record
//! names; a sibling idle for the named rounds is ended by the elder, whose
//! next round names its mutators back to itself; a wake sent to an ended
//! sibling is lost and the count stands; and a cap of one births nothing.
//!
//! The second mutator is a thread of the case's that registers and polls on
//! its own thread through jobs the case sends it ([`Mutator`]).

use super::*;
use crate::cycle::testing::Sent;
use crate::memory::arena::Arena;
use crate::object::Object;
use crate::refcount::{RcHeader, ll_release, ll_retain};

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

    // A collection disposes of the verdicts standing in P at its close —
    // read live, they go to the deferred lane — and the re-offer brings the
    // lane back into R for the next.
    let freed_first = unsafe { crate::gc::ll_gc_collect_cycles() };
    crate::cycle::queue::reoffer_deferred_candidates();
    // Under the collector's chain the roots it read live wait there, and the
    // exit's splice brings it back whole.
    #[cfg(feature = "collector-chain")]
    unsafe {
        let _claim = crate::cycle::token::HeldToken::take();
        crate::cycle::chain::splice_this_threads_chain_into_r(true)
    };
    assert_eq!(
        freed_first + unsafe { crate::gc::ll_gc_collect_cycles() },
        members,
        "the ring is collected whole"
    );
}

fn wait_for_rounds_of(index: usize, rounds: usize) {
    let mut seen = 0;
    assert!(
        wait_until(
            || {
                // As the other mutator's loop does: the `POSTED` a batch
                // left is cleared; the clear a wake relies on is
                // `dispose_by_hand`'s, after this wait returns.
                unsafe { &*record() }.clear_posted_for_test();
                seen += testing::take_rounds_of(index);
                seen >= rounds
            },
            A_BIRTH
        ),
        "collector {index} made {rounds} rounds"
    );
}

/// Stand in for every mutator's disposition of the last batch before the
/// next round is woken. A round reads a record still at `POSTED` as neither
/// a batch nor work, so a backlog read across such a record is one short.
/// `wait_for_rounds_of` clears the case's byte, but its clear can precede
/// the batch's post and its take follow the round's note; the other
/// mutator's loop clears its byte a tick after the batch, and a wake inside
/// that tick met the second backlog round with a backlog of one
/// (`dev/POSTMORTEM.md`, "a wake inside the other mutator's tick meets a
/// backlog of one").
fn dispose_by_hand(others: &[&Mutator]) {
    unsafe { &*record() }.clear_posted_for_test();
    for other in others {
        other.run(|_| unsafe { &*mutator_record::this_thread_record() }.clear_posted_for_test());
    }
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
    // threshold, the second's too, and the second births the sibling. The
    // outcomes are zeroed here so that a red's message carries the second
    // round's alone (`dev/POSTMORTEM.md`, "a wake inside the other mutator's
    // tick meets a backlog of one").
    let _ = testing::take_outcomes();
    dispose_by_hand(&[&other]);
    assert!(wake(ELDER));
    wait_for_rounds_of(ELDER, 1);
    assert!(
        wait_until(
            || testing::collector_state(1) == ThreadState::Alive,
            A_BIRTH
        ),
        "two backlog rounds birthed a sibling; the second round read {:?}; \
         now mine at {:#x}, theirs at {:#x}, the last refused birth {:?} ago",
        testing::take_outcomes(),
        unsafe { &*record }.token.read(),
        unsafe { &*other.record }.token.read(),
        refused_birth_age()
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
        dispose_by_hand(&[]);
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
    let _ = testing::take_backlog_rounds_without_a_birth();
    ensure_thread();
    wait_for_rounds_of(ELDER, 1);
    for _ in 0..BACKLOG_ROUNDS_TO_BIRTH {
        dispose_by_hand(&[&other]);
        assert!(wake(ELDER));
        wait_for_rounds_of(ELDER, 1);
    }
    std::thread::sleep(A_ROUNDS_ABSENCE);
    assert_eq!(testing::take_spawns(), 1, "the elder alone");
    assert_eq!(testing::collector_state(1), ThreadState::Unborn);
    // The cap was what refused, and not a round that read no backlog: the
    // second backlog round reached the birth once and found no slot.
    assert_eq!(testing::take_backlog_rounds_without_a_birth(), 1);
    assert_eq!(unsafe { &*record }.collector(), ELDER);
    assert_eq!(unsafe { &*other.record }.collector(), ELDER);

    testing::retire();
    unsafe { let_go_and_collect(mine) };
    other.run(move |_| unsafe { let_go_and_collect(theirs) });
    drop(other);
    crate::cycle::queue::release_queue_segments();
}
