//! What a thread's exit does with the candidates it never polled for: it waits
//! for any trace over its blocks, collects, and reports what it could not
//! take (`dev/DECISIONS.md`, "a thread waits for the trace, collects, and then
//! exits").
//!
//! A case about the exit sequence runs on a thread of its own and calls
//! `ll_thread_exit` by hand, because the answer it reads — `take_exit_residue`
//! — is the exiting thread's and the guard's own call runs after the closure
//! has returned; the guard's call then finds an exited thread and disposes
//! nothing twice. A case about what the collection offers and reports calls
//! `collect_before_exit` on its own thread, so that the ring it leaves can be
//! taken apart afterwards rather than abandoned with the thread's blocks.
//!
//! The holder that stands in for a collector is the shared stand-in, which
//! lets go only once the token's count of waits has moved
//! (`crate::cycle::token::testing`).

use super::*;
use crate::cycle::collect::{
    EXIT_ROUNDS, Ending, ExitEnding, collect_before_exit, take_exit_residue,
};
use crate::cycle::queue::{
    candidate_count, defer_candidates, deferred_count, detach_candidates, overflow_len,
    release_queue_segments,
};
use crate::cycle::token::testing::HeldByACollector;
use crate::cycle::token::this_thread_token;
use crate::memory::block_pool::BlockPool;
use crate::memory::heap::{ll_thread_exit, ll_thread_init};

/// A class pointer handed to the case's thread: the descriptor is immortal,
/// so the pointee outlives both.
struct Handed(*const Class);

unsafe impl Send for Handed {}

impl Handed {
    /// The pointer, through a method so that a closure captures the wrapper
    /// rather than its field.
    fn class(&self) -> *const Class {
        self.0
    }
}

/// Blocks a thread's life may leave out: under `debug-journal` the exit
/// retires a ring the registry keeps (`journal::retire_ring`).
fn blocks_a_life_may_keep() -> usize {
    usize::from(cfg!(feature = "debug-journal"))
}

/// A ring registered and never collected is freed by the exit, so the block
/// it stood in goes back with the thread's others rather than to the
/// abandoned list with three occupants nobody will ever decrement again.
#[test]
fn a_thread_that_exits_between_registration_and_collection_frees_its_ring() {
    let _g = test_guard();
    // Built here rather than on the thread: class metadata is immortal, and
    // the block a class draws would read as one the thread left out.
    let class = Handed(node_class("ExitRingNode", counting_destructor as *const ()));
    let pool = BlockPool::global();
    let before = pool.blocks_out();

    let residue = std::thread::spawn(move || {
        assert!(ll_thread_init(), "the pool served this thread");
        let class = class.class();
        let mut arena = Arena::new();
        let _members = unsafe { ring(&mut arena, [class, class, class]) };
        drop(arena);

        // No poll between the registration and the exit.
        ll_thread_exit();
        take_exit_residue().expect("the exit ran its collection")
    })
    .join()
    .unwrap();

    assert_eq!(residue.freed, 3, "the exit collected the ring");
    assert_eq!(residue.registered, 0, "and left nothing registered");
    assert_eq!(
        residue.ending,
        ExitEnding::Round(Ending::EmptyLane),
        "the round after the teardown found the lane empty"
    );
    let after = pool.blocks_out();
    assert!(
        after <= before + blocks_a_life_may_keep(),
        "the ring's block went back: {after} out against {before} before"
    );
}

/// An exit whose token a collector holds waits for the release, and the
/// collection it then runs takes the ring. Whether it waited is read off the
/// token's count, and the holder lets go only once that count has moved.
#[test]
fn an_exit_under_a_held_token_waits_for_the_release_and_then_collects() {
    let _g = test_guard();

    let (waited, residue) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        let class = node_class("ExitHeldTokenNode", counting_destructor as *const ());
        let mut arena = Arena::new();
        let _members = unsafe { ring(&mut arena, [class, class, class]) };
        drop(arena);

        let waits_before = unsafe { (*this_thread_token()).waits() };
        // Released by the holder itself, once the count says this thread is
        // waiting; the guard joins it on the way out either way.
        let _held = HeldByACollector::take(this_thread_token(), true);
        ll_thread_exit();
        (
            unsafe { (*this_thread_token()).waits() } - waits_before,
            take_exit_residue().expect("the exit ran its collection"),
        )
    })
    .join()
    .unwrap();

    assert_ne!(waited, 0, "the exit went to wait on the held token");
    assert_eq!(residue.freed, 3, "and collected the ring once released");
    assert_eq!(residue.registered, 0);
}

/// A ring a keeper holds is what the exit cannot take: the scan reads every
/// root as reachable, the registrations stand, and the residue names them with
/// the reading that left them.
///
/// The exit's collection is called on the case's own thread rather than
/// through an exit, so that the keeper can let go afterwards and the ring be
/// collected: a block abandoned with three live occupants would stay out for
/// the rest of the run.
#[test]
fn what_the_exit_cannot_take_is_reported_as_the_residue() {
    let _g = test_guard();
    let class = node_class("ExitKeptRingNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [class, class, class]) };
    // One external hold on a member keeps the whole ring live.
    unsafe { ll_retain(members[0] as *mut RcHeader) };

    let residue = unsafe { collect_before_exit() };

    assert_eq!(residue.freed, 0, "a held ring is not torn down");
    assert_eq!(residue.registered, 3, "every member keeps its registration");
    assert_eq!(
        residue.ending,
        ExitEnding::Round(Ending::NothingProposed),
        "and the cause is a scan that found the keeper's reference"
    );

    unsafe { ll_release(members[0] as *mut RcHeader) };
    assert_eq!(
        unsafe { collect_before_exit() }.freed,
        3,
        "without the keeper the same collection takes the ring"
    );
}

/// A ring whose registrations no allocation path could fund stands in the
/// overflow buffer, and the exit's collection drains that buffer into the
/// lane before it traces: the ring is freed like any other.
#[test]
fn a_ring_in_the_overflow_buffer_is_collected_by_the_exit() {
    let _g = test_guard();
    let class = node_class("ExitOverflowRingNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    // Empty the cells and the reserve, so the registrations have nowhere
    // but the buffer (`cycle::queue::tests::where_a_full_segment_comes_from`).
    release_queue_segments();
    crate::memory::critical::drain_for_test();
    let _members = unsafe { ring(&mut arena, [class, class, class]) };
    assert_eq!(overflow_len(), 3, "the ring registered into the buffer");
    assert_eq!(candidate_count(), 0);

    let residue = unsafe { collect_before_exit() };

    assert_eq!(
        residue.freed, 3,
        "the exit drained the buffer and collected"
    );
    assert_eq!(residue.registered, 0);
    assert_eq!(overflow_len(), 0);
}

/// A ring whose registrations a live reading deferred is re-offered by the
/// exit whatever the epoch says, because the exit is this thread's last
/// turnover.
#[test]
fn a_ring_in_the_deferred_lane_is_collected_by_the_exit() {
    let _g = test_guard();
    let class = node_class("ExitDeferredRingNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let _members = unsafe { ring(&mut arena, [class, class, class]) };
    defer_candidates(detach_candidates(), crate::cycle::epoch::commits());
    assert_eq!(deferred_count(), 3, "the ring's registrations are deferred");
    assert_eq!(candidate_count(), 0);

    let residue = unsafe { collect_before_exit() };

    assert_eq!(
        residue.freed, 3,
        "the exit re-offered the lane and collected"
    );
    assert_eq!(residue.registered, 0);
    assert_eq!(deferred_count(), 0);
}

/// Rings a spawning destructor may still build, the deaths it has counted,
/// and the class it builds from: set by the case, read by every
/// `spawning_destructor` of it.
static SPAWN_BUDGET: AtomicUsize = AtomicUsize::new(0);
static SPAWNER_DEATHS: AtomicUsize = AtomicUsize::new(0);
static SPAWNER_CLASS: AtomicUsize = AtomicUsize::new(0);

/// A destructor that builds a fresh ring of its own class at the first death
/// of every ring, while the budget lasts: one ring's worth of garbage
/// manufactured inside each of the exit's rounds.
unsafe extern "C" fn spawning_destructor(_object: *mut Object) {
    if SPAWNER_DEATHS.fetch_add(1, Ordering::Relaxed) % 3 != 0
        || SPAWN_BUDGET
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .is_err()
    {
        return;
    }

    let class = SPAWNER_CLASS.load(Ordering::Relaxed) as *const Class;
    let mut arena = Arena::new();
    let _ring = unsafe { ring(&mut arena, [class, class, class]) };
}

/// A destructor that manufactures a ring in every round is bounded by the
/// round cap rather than by its own appetite, and the residue says so.
#[test]
fn a_destructor_that_manufactures_garbage_meets_the_round_cap() {
    let _g = test_guard();
    let class = node_class("ExitSpawnerNode", spawning_destructor as *const ());
    SPAWNER_CLASS.store(class as usize, Ordering::Relaxed);
    SPAWNER_DEATHS.store(0, Ordering::Relaxed);
    // One ring per round, for more rounds than the cap allows.
    SPAWN_BUDGET.store(EXIT_ROUNDS * 2, Ordering::Relaxed);
    let mut arena = Arena::new();
    let _members = unsafe { ring(&mut arena, [class, class, class]) };

    let residue = unsafe { collect_before_exit() };

    assert_eq!(
        residue.ending,
        ExitEnding::RoundCap,
        "the cap ended the rounds"
    );
    assert_eq!(
        residue.freed,
        3 * EXIT_ROUNDS,
        "every round tore one ring down"
    );
    assert_eq!(
        residue.registered, 3,
        "and the last round's spawn stands registered as the residue"
    );

    // What the cap left is taken apart with the budget spent.
    SPAWN_BUDGET.store(0, Ordering::Relaxed);
    assert_eq!(
        unsafe { collect_before_exit() }.registered,
        0,
        "without a budget the spawned rings are ordinary garbage"
    );
}

/// The ring member a resurrecting destructor holds from outside, and the
/// object it resurrected: it does so once, whichever member's destructor
/// runs first, and lets the held ring go.
static HELD_BY_THE_RESURRECTOR: AtomicUsize = AtomicUsize::new(0);
static RESURRECTED: AtomicUsize = AtomicUsize::new(0);

/// A destructor that resurrects its object the first time it runs and drops
/// the hold it has on another ring's member: a round that frees nothing and
/// moves no registration, after which garbage stands in the lane.
unsafe extern "C" fn resurrecting_releasing_destructor(object: *mut Object) {
    if RESURRECTED
        .compare_exchange(0, object as usize, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }

    unsafe { ll_retain(object as *mut RcHeader) };
    let held = HELD_BY_THE_RESURRECTOR.load(Ordering::Relaxed) as *mut RcHeader;
    unsafe { ll_release(held) };
}

/// A round whose destructor resurrected a member freed nothing and moved no
/// registration, and still made garbage: the hold it dropped was the other
/// ring's last. The exit runs another round because the round ran
/// destructors, and takes the ring that round orphaned.
#[test]
fn a_round_that_ran_destructors_is_followed_by_another_whatever_the_counts_say() {
    let _g = test_guard();
    let resurrector = node_class(
        "ExitResurrectorNode",
        resurrecting_releasing_destructor as *const (),
    );
    let held = node_class("ExitHeldRingNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    let held_ring = unsafe { ring(&mut arena, [held, held]) };
    unsafe { ll_retain(held_ring[0] as *mut RcHeader) };
    HELD_BY_THE_RESURRECTOR.store(held_ring[0] as usize, Ordering::Relaxed);
    RESURRECTED.store(0, Ordering::Relaxed);
    let _resurrecting_ring = unsafe { ring(&mut arena, [resurrector, resurrector]) };

    let residue = unsafe { collect_before_exit() };

    assert_eq!(
        residue.freed, 2,
        "the held ring died in the round after its holder let go"
    );
    assert_eq!(residue.registered, 2, "the resurrected ring stands, live");

    // The resurrection's own reference goes, and the ring with it: the
    // member that took it is whichever destructor ran first.
    let resurrected = RESURRECTED.load(Ordering::Relaxed) as *mut RcHeader;
    assert!(!resurrected.is_null(), "one member resurrected itself");
    assert!(
        !unsafe { ll_release(resurrected) },
        "the ring's edge still holds it"
    );
    assert_eq!(unsafe { collect_before_exit() }.freed, 2);
}

/// Entries the exit cannot draw a segment for stay in the overflow buffer,
/// unread by every round, and the residue names the buffer rather than the
/// empty lane the rounds read.
#[test]
fn what_stands_in_the_overflow_buffer_at_exit_is_reported_as_unread() {
    let _g = test_guard();
    let class = node_class("ExitStrandedOverflowNode", counting_destructor as *const ());
    let mut arena = Arena::new();
    release_queue_segments();
    crate::memory::critical::drain_for_test();
    let _members = unsafe { ring(&mut arena, [class, class, class]) };
    assert_eq!(overflow_len(), 3, "the ring registered into the buffer");
    // Closed after the ring is built: what the exit needs the pool for is the
    // segment the buffer would drain into.
    let budgeted = crate::memory::block_pool::budget_blocks(0);

    let residue = unsafe { collect_before_exit() };

    assert_eq!(residue.freed, 0, "no round could read the buffer");
    assert_eq!(residue.registered, 3);
    assert_eq!(
        residue.ending,
        ExitEnding::OverflowUnread,
        "and the residue says where it stands"
    );

    drop(budgeted);
    assert_eq!(
        unsafe { collect_before_exit() }.freed,
        3,
        "with the pool open the same exit drains and collects it"
    );
}
