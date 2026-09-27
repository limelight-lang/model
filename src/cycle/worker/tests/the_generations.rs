//! A root the collector reads live under `deferral-by-generation`: of the
//! first generation — its stamp carries no age, or one of the batch's own
//! epoch — it is listed alone, posted `ReadLive` unmarked and written back
//! into R by the disposition, its core left unstamped so that a later batch
//! of the same epoch can see it die; once it has outlived an epoch it is
//! posted marked, deferred, and its core listed and stamped as the build
//! without the feature does (`dev/plans/S65.md`, S65.31).
//!
//! The collector is a thread of the case's that serves this thread's record,
//! as in `the_batch`.

use super::the_batch::{keeper_class, served_by_a_collector};
use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::queue::{candidate_count, deferred_count};
use crate::cycle::testing::{move_prop, ring, stamp_of};
use crate::gc::{ll_gc_collect_cycles, ll_gc_maybe_collect};
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::test_support::{prop_offset, store_prop};

/// The rig's `SMALL_RING`: six members, one of them registered.
const MEMBERS: usize = 6;

/// A ring of [`MEMBERS`] linked through each member's first property with its
/// creation reference, its first member registered and held by a keeper from
/// outside.
struct KeptRing {
    members: Vec<*mut Object>,
    keeper: *mut Object,
}

impl KeptRing {
    fn root(&self) -> *mut Object {
        self.members[0]
    }

    /// Null the keeper's edge, a decrement the registered root does not
    /// register again, and let the keeper die: the ring is garbage whose one
    /// entry stands wherever the root's does.
    ///
    /// # Safety
    /// The ring came from [`a_kept_ring`] on this thread and its keeper is
    /// still held.
    unsafe fn let_the_keeper_go(&mut self, arena: &mut Arena) {
        unsafe {
            store_prop(arena, self.keeper, prop_offset(0), std::ptr::null_mut());
            assert!(
                ll_release(self.keeper as *mut RcHeader),
                "the keeper's last"
            );
            ll_object_die(self.keeper);
        }
        self.keeper = std::ptr::null_mut();
    }
}

/// # Safety
/// A quiescent heap under `test_guard`.
unsafe fn a_kept_ring(arena: &mut Arena, name: &str) -> KeptRing {
    unsafe { a_kept_ring_of(arena, name, MEMBERS) }
}

/// [`a_kept_ring`] of `members` members.
///
/// # Safety
/// As [`a_kept_ring`].
unsafe fn a_kept_ring_of(arena: &mut Arena, name: &str, members: usize) -> KeptRing {
    let class = member_class(name);
    let arena_ptr: *mut Arena = arena;
    let mut context = LLContext { arena };
    let members: Vec<*mut Object> = (0..members)
        .map(|_| unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) })
        .collect();
    let keeper = unsafe {
        new_constructed(
            &mut context,
            keeper_class("GenerationKeeper"),
            MemoryCategory::GcHeap,
        )
    };
    unsafe {
        for (position, &member) in members.iter().enumerate() {
            move_prop(
                member,
                prop_offset(0),
                members[(position + 1) % members.len()],
            );
        }

        ll_retain(members[0] as *mut RcHeader);
        assert!(
            !ll_release(members[0] as *mut RcHeader),
            "an edge holds the root"
        );
        store_prop(arena_ptr, keeper, prop_offset(0), members[0]);
    }
    KeptRing { members, keeper }
}

/// Let the ring go and free it by the explicit collection, then empty the
/// lanes of whatever entry is left.
///
/// # Safety
/// As [`KeptRing::let_the_keeper_go`], or the keeper already went.
unsafe fn free_the_ring(arena: &mut Arena, mut ring: KeptRing) {
    if !ring.keeper.is_null() {
        unsafe { ring.let_the_keeper_go(arena) };
    }

    unsafe { ll_gc_collect_cycles() };
    reset_lanes();
}

/// A class of two counted Box properties: the first the ring links through,
/// the second free for an edge into another ring.
fn member_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .prop("held", true)
        .build()
}

/// The epoch this thread's cell stands in, moved off zero first so that a
/// stamp of this epoch is told apart from a fresh header's zero byte.
fn a_nonzero_epoch() -> u32 {
    crate::cycle::epoch::turn_to_a_nonzero_epoch();
    crate::cycle::epoch::current()
}

/// One serve, reading the batch the collector traced.
fn a_traced_serve() -> (Served, testing::TracedBatch) {
    testing::read_traced_batches(true);
    let served = served_by_a_collector();
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert_eq!(traced.len(), 1, "one batch");
    (served, traced[0])
}

/// The generation figures of one serve and the poll after it.
fn a_serve_and_its_poll() -> (testing::Generations, usize) {
    let _ = testing::take_generations();
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    let freed = unsafe { ll_gc_maybe_collect() };
    (testing::take_generations(), freed)
}

/// A young ring the batch read live is written back into R with its root
/// alone stamped, so that the next batch of the same epoch, after the keeper
/// went, meets the whole ring unpruned and proposes it, and the poll frees
/// it. Red with the core listed on the first reading: the second batch
/// prunes at the stamped members and reads the ring live again.
#[test]
fn a_young_ring_let_go_inside_its_epoch_is_freed_by_the_next_batch() {
    let _g = test_guard();
    reset_lanes();
    let epoch = a_nonzero_epoch();
    let mut arena = Arena::new();
    let mut ring = unsafe { a_kept_ring(&mut arena, "YoungLetGo") };

    let (generations, freed) = a_serve_and_its_poll();
    assert_eq!(freed, 0);
    assert_eq!(
        generations,
        testing::Generations {
            posted_first: 1,
            written_back_first: 1,
            ..Default::default()
        }
    );
    assert_eq!(
        deferred_count(),
        0,
        "a root of the first generation waits in no lane"
    );
    assert_eq!(candidate_count(), 1, "it stands in R again");
    assert_eq!(
        unsafe { stamp_of(ring.root()) },
        (epoch, 1),
        "the root, listed alone"
    );
    assert!(
        ring.members[1..]
            .iter()
            .all(|&member| unsafe { stamp_of(member) }.1 == 0),
        "the core stays unstamped"
    );

    unsafe { ring.let_the_keeper_go(&mut arena) };
    let (_, second) = a_traced_serve();
    assert_eq!(second.edges_pruned, 0, "the re-read met the whole ring");
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        MEMBERS,
        "and the poll freed it"
    );
    assert_eq!(candidate_count() + deferred_count(), 0);
    reset_lanes();
}

/// A kept ring is written back at every batch of the epoch it was first read
/// in, and deferred at the first batch after the turn, its whole core listed
/// and stamped with the new epoch. Red with the collector's mark ignored by
/// the disposition: the root is written back after the turn too.
#[test]
fn a_kept_ring_is_written_back_in_its_epoch_and_deferred_after_the_turn() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "KeptAcrossTheTurn") };

    for _ in 0..2 {
        let (generations, _) = a_serve_and_its_poll();
        assert_eq!(
            (generations.posted_first, generations.written_back_first),
            (1, 1)
        );
        assert_eq!((candidate_count(), deferred_count()), (1, 0));
    }

    crate::cycle::epoch::turn_this_threads_cell();
    let epoch = crate::cycle::epoch::current();
    let (generations, _) = a_serve_and_its_poll();
    assert_eq!(
        generations,
        testing::Generations {
            posted_second: 1,
            ..Default::default()
        }
    );
    assert_eq!(
        (candidate_count(), deferred_count()),
        (0, 1),
        "deferred after the turn"
    );
    assert!(
        ring.members
            .iter()
            .all(|&member| unsafe { stamp_of(member) } == (epoch, 1)),
        "the core listed with the root, and stamped at the take"
    );

    unsafe { free_the_ring(&mut arena, ring) };
}

/// A root read live in one epoch and next read two turns later has outlived
/// an epoch, not only the one before; four turns later its stamp aliases to
/// the batch's own epoch and it reads as young, which costs a lap of R and no
/// root.
#[test]
fn a_root_read_two_turns_later_is_deferred_and_four_turns_later_is_young() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let ring = unsafe { a_kept_ring(&mut arena, "TwoAndFourTurns") };
    let (generations, _) = a_serve_and_its_poll();
    assert_eq!(generations.written_back_first, 1);

    for _ in 0..4 {
        crate::cycle::epoch::turn_this_threads_cell();
    }
    let (generations, _) = a_serve_and_its_poll();
    assert_eq!(
        (generations.posted_first, generations.posted_second),
        (1, 0),
        "four turns alias to the batch's epoch"
    );

    for _ in 0..2 {
        crate::cycle::epoch::turn_this_threads_cell();
    }
    let (generations, _) = a_serve_and_its_poll();
    assert_eq!(
        (generations.posted_first, generations.posted_second),
        (0, 1),
        "two turns are an epoch outlived"
    );
    assert_eq!(deferred_count(), 1);

    unsafe { free_the_ring(&mut arena, ring) };
}

/// The collection over P that a proposal arms leaves the collector's mark on
/// a `ReadLive` that is no root of it: the old root is deferred, the young
/// one written back, and the garbage freed. Red with the close's rewrite of
/// every entry of the prefix, which strips the mark and writes the old root
/// back.
#[test]
fn the_collection_over_p_keeps_the_collectors_mark_on_a_root_it_did_not_read() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let garbage_class = member_class("GenerationGarbage");
    let _garbage = unsafe { ring(&mut arena, [garbage_class, garbage_class]) };
    let old = unsafe { a_kept_ring(&mut arena, "GenerationOld") };
    unsafe { crate::cycle::testing::as_of_the_second_generation(old.root() as *mut RcHeader) };
    let young = unsafe { a_kept_ring(&mut arena, "GenerationYoung") };

    let (generations, freed) = a_serve_and_its_poll();
    assert_eq!(freed, 2, "the garbage ring, proposed and freed over P");
    assert_eq!(
        (
            generations.posted_first,
            generations.posted_second,
            generations.written_back_first
        ),
        (1, 1, 1)
    );
    assert_eq!(deferred_count(), 1, "the old root, by the collector's mark");
    assert_eq!(candidate_count(), 1, "the young root, written back");

    unsafe {
        free_the_ring(&mut arena, old);
        free_the_ring(&mut arena, young);
    }
}

/// A collection over R whole takes an unmarked `ReadLive` of P for a root, as
/// it takes an unwalked one: a young ring whose keeper went after the batch
/// is freed by the explicit collection. Red with the entry left out of the
/// batch's roots, which writes the root back and frees nothing.
#[test]
fn a_collection_over_r_whole_traces_a_root_of_the_first_generation() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let mut ring = unsafe { a_kept_ring(&mut arena, "YoungUnderAllRoots") };
    assert!(matches!(
        served_by_a_collector(),
        Served::Batch { complete: true, .. }
    ));
    assert_eq!(candidate_count(), 0, "the root stands in P, not in R");

    unsafe { ring.let_the_keeper_go(&mut arena) };
    assert_eq!(unsafe { ll_gc_collect_cycles() }, MEMBERS);
    assert_eq!(candidate_count() + deferred_count(), 0);
    reset_lanes();
}

/// A young root that a part whose root has outlived an epoch meets is posted
/// marked and deferred with the old root: the old part lists its core, the
/// young ring's rows among them, so a write-back would only re-read a ring
/// the prune stops at. Red with the young root posted by its generation
/// alone, which writes it back.
#[test]
fn a_young_root_an_old_part_meets_is_deferred_with_the_old_core() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let old = unsafe { a_kept_ring(&mut arena, "GenerationOldHolder") };
    unsafe { crate::cycle::testing::as_of_the_second_generation(old.root() as *mut RcHeader) };
    let young = unsafe { a_kept_ring(&mut arena, "GenerationHeldYoung") };
    unsafe {
        store_prop(&mut arena, old.members[1], prop_offset(1), young.root());
    }

    let (generations, _) = a_serve_and_its_poll();
    assert_eq!(
        (
            generations.posted_second,
            generations.posted_in_an_old_core,
            generations.posted_first
        ),
        (1, 1, 0)
    );
    assert_eq!(
        (candidate_count(), deferred_count()),
        (0, 2),
        "both deferred"
    );

    unsafe {
        free_the_ring(&mut arena, young);
        free_the_ring(&mut arena, old);
    }
}

/// A young root the list has no room for is posted marked and deferred: a
/// root listed with no stamp would come back young at every lap. The list is
/// bounded to one block and an old part's core fills it first. Red with the
/// refusal read as a listing, which writes the root back.
#[test]
#[cfg_attr(miri, ignore = "8,200 members are past what Miri affords")]
fn a_young_root_the_list_refuses_is_deferred() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let _bound = crate::cycle::live_list::testing::bound_the_chain(1);
    let mut arena = Arena::new();
    let old = unsafe { a_kept_ring_of(&mut arena, "GenerationFillsTheList", 8_200) };
    unsafe { crate::cycle::testing::as_of_the_second_generation(old.root() as *mut RcHeader) };
    let young = unsafe { a_kept_ring(&mut arena, "GenerationRefused") };

    let (generations, _) = a_serve_and_its_poll();
    assert_eq!(
        (
            generations.posted_second,
            generations.posted_unlisted,
            generations.posted_first
        ),
        (1, 1, 0)
    );
    assert_eq!((candidate_count(), deferred_count()), (0, 2));

    unsafe {
        free_the_ring(&mut arena, young);
        free_the_ring(&mut arena, old);
    }
}
