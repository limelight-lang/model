//! The live list: a batch that reads a part's root live lists the live rows
//! of that part, and the mutator stamps every listed member `{e, 1}` at its
//! take from `POSTED` — or at the first return of a block or a run under
//! `POSTED`, before the memory leaves the thread — so that the next take in
//! the same epoch prunes at the core; under pressure, and where the epoch
//! moved, the list goes back unstamped (`crate::cycle::live_list`;
//! `dev/CYCLE-SPLIT-PACKAGE-3.md`, section 5).
//!
//! The collector is a thread of the case's that serves this thread's record,
//! as in `the_batch`.

use super::the_batch::{keeper_class, served_by_a_collector};
use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::live_list::testing::{
    after_listed_rows, bound_the_chain, take_blocks_across_a_list, take_stale_lists_given_back,
    take_stamps, take_stamps_at_a_return,
};
use crate::cycle::queue::reoffer_deferred_candidates;
use crate::cycle::testing::{move_prop, stamp_of};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BlockHeader, BlockPool, LINE_SIZE};
use crate::memory::context::LLContext;
use crate::memory::gc_metadata;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS, prop_offset, store_prop, wide_class};

/// The shape of the take probe's `overlapping-live`: one component of 381
/// members, the corpus's median closure, 63 of them registered and a keeper
/// holding the first (`what_a_take_costs`).
const MEMBERS: usize = 381;
const ROOTS: usize = 63;

/// A ring of one member per class of `classes`, linked through each member's
/// first property with its creation reference, so that no member is
/// registered by its construction; the first `roots` members registered by a
/// retain and a non-final release, and a keeper holding the first member
/// from outside, which is what reads the component live.
struct Core {
    members: Vec<*mut Object>,
    keeper: *mut Object,
}

/// # Safety
/// A quiescent heap under `test_guard`, and every class of `classes` has a
/// counted Box as its first property.
unsafe fn live_core(arena: &mut Arena, classes: &[*const Class], roots: usize) -> Core {
    let arena_ptr: *mut Arena = arena;
    let mut context = LLContext { arena };
    let members: Vec<*mut Object> = classes
        .iter()
        .map(|&class| unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) })
        .collect();
    let keeper = unsafe {
        new_constructed(
            &mut context,
            keeper_class("LiveListKeeper"),
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

        for &root in &members[..roots] {
            ll_retain(root as *mut RcHeader);
            assert!(!ll_release(root as *mut RcHeader), "an edge holds the root");
        }

        store_prop(arena_ptr, keeper, prop_offset(0), members[0]);
    }
    Core { members, keeper }
}

/// Take the core apart by hand, as the take probe does: every member whose
/// slot still reads live retained, its edge nulled, released and died; then
/// the keeper; then the lanes emptied of the roots' entries.
///
/// # Safety
/// `core` came from [`live_core`] on this thread, and no collection runs.
unsafe fn let_the_core_go(arena: &mut Arena, core: Core) {
    let arena_ptr: *mut Arena = arena;
    unsafe {
        store_prop(arena_ptr, core.keeper, prop_offset(0), std::ptr::null_mut());
        let alive: Vec<*mut Object> = core
            .members
            .iter()
            .copied()
            .filter(|&member| {
                crate::refcount::slot_state(member as *mut RcHeader)
                    == crate::refcount::SlotState::Live
            })
            .collect();
        for &member in &alive {
            ll_retain(member as *mut RcHeader);
        }

        for &member in &alive {
            store_prop(arena_ptr, member, prop_offset(0), std::ptr::null_mut());
        }

        for &member in &alive {
            assert!(ll_release(member as *mut RcHeader), "the member's last");
            ll_object_die(member);
        }

        assert!(ll_release(core.keeper as *mut RcHeader));
        ll_object_die(core.keeper);
    }
    reset_lanes();
}

/// A class of one counted Box property, which the ring links through.
fn member_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("next", true).build()
}

/// The epoch this thread's cell stands in, moved off zero first so that a
/// stamp of this epoch is told apart from a fresh header's zero byte.
fn a_nonzero_epoch() -> u32 {
    crate::cycle::epoch::turn_to_a_nonzero_epoch();
    crate::cycle::epoch::epoch_of(unsafe { &*record() }.turnovers())
}

/// The stamps `members` carry, in their order.
fn stamps(members: &[*mut Object]) -> Vec<(u32, u32)> {
    members
        .iter()
        .map(|&member| unsafe { stamp_of(member) })
        .collect()
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

/// One serve on a thread of the case's, answering the serve and that
/// thread's GC figures before and after it.
fn served_reading_the_collectors_figures() -> (
    Served,
    gc_metadata::GcMemoryStats,
    gc_metadata::GcMemoryStats,
) {
    unsafe { &*record() }.clear_posted_for_test();
    let sent = crate::cycle::testing::Sent(record());
    testing::consent_while(std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        let before = gc_metadata::thread_stats();
        let served = unsafe { testing::serve_alone(sent.into_inner()) };
        (served, before, gc_metadata::thread_stats())
    }))
}

/// A take from `POSTED` stamps every member of the live core the batch read,
/// and nothing else: the collector wrote no header, the keeper it never met
/// stays unstamped, and the list's blocks, drawn on the collector's thread,
/// end on neither thread's figures.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn a_take_from_posted_stamps_the_live_core_the_batch_read() {
    let _g = test_guard();
    reset_lanes();
    let epoch = a_nonzero_epoch();
    let mut arena = Arena::new();
    let core = unsafe {
        live_core(
            &mut arena,
            &vec![member_class("LiveListMember"); MEMBERS],
            ROOTS,
        )
    };
    unsafe { &*record() }.set_batch_size(ROOTS);
    let _ = (take_stamps(), take_blocks_across_a_list());

    let (served, collector_before, collector_after) = served_reading_the_collectors_figures();
    assert_eq!(
        served,
        Served::Batch {
            roots: ROOTS,
            complete: true,
            backlog: false,
        }
    );
    assert_eq!(
        collector_after.current_blocks(),
        collector_before.current_blocks() + 1,
        "the collector keeps its workspace and nothing of the list"
    );
    assert!(
        stamps(&core.members).iter().all(|&(_, age)| age == 0),
        "the collector writes no stamp"
    );
    assert!(
        !unsafe { &*record() }.live_list().is_null(),
        "the grant left its list"
    );

    unsafe { ll_gc_maybe_collect() };
    assert!(
        unsafe { &*record() }.live_list().is_null(),
        "the take consumed it"
    );
    assert_eq!(take_stamps(), MEMBERS, "one stamp per member listed");
    assert_eq!(stamps(&core.members), vec![(epoch, 1); MEMBERS]);
    assert_eq!(
        unsafe { stamp_of(core.keeper) }.1,
        0,
        "the keeper, which no part met"
    );
    let (before, after) = take_blocks_across_a_list().expect("the take gave a list back");
    assert_eq!(
        before, after,
        "the list's blocks, taken over from the collector, went back on this thread"
    );

    unsafe { let_the_core_go(&mut arena, core) };
}

/// The take after one that stamped the core, in the same epoch, prunes at the
/// first member no queue entry names: the roots are met and the rest of the
/// core is not (`dev/CYCLE-SPLIT-PACKAGE-3.md`, section 11, commit 6).
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn the_next_take_in_the_same_epoch_prunes_at_the_stamped_core() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let core = unsafe {
        live_core(
            &mut arena,
            &vec![member_class("LiveListPrunedMember"); MEMBERS],
            ROOTS,
        )
    };
    unsafe { &*record() }.set_batch_size(ROOTS);
    let (_, first) = a_traced_serve();
    assert_eq!(
        first.rows_met, MEMBERS,
        "the first take meets the whole core"
    );
    unsafe { ll_gc_maybe_collect() };

    // The roots, read live, wait in the deferred lane for the turnover; the
    // splice puts them back in R with no turnover, as pressure does.
    reoffer_deferred_candidates();
    unsafe { &*record() }.set_batch_size(ROOTS);
    let (served, second) = a_traced_serve();
    assert_eq!(
        served,
        Served::Batch {
            roots: ROOTS,
            complete: true,
            backlog: false,
        }
    );
    assert!(
        second.edges_pruned > 0,
        "the mark stopped at the stamped core"
    );
    assert!(
        second.rows_met <= ROOTS,
        "the take met the roots and no more: {} rows",
        second.rows_met
    );
    assert_eq!(
        second.blocks, 0,
        "the take drew no block past the workspace"
    );

    unsafe { ll_gc_maybe_collect() };
    unsafe { let_the_core_go(&mut arena, core) };
}

/// An advance of the epoch between the release and the take leaves the list
/// unread: a stamp of the batch's epoch would read as no stamp in the next,
/// so the list goes back as it stands.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn an_advance_before_the_take_gives_the_list_back_unstamped() {
    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let core = unsafe {
        live_core(
            &mut arena,
            &vec![member_class("LiveListAdvancedMember"); MEMBERS],
            ROOTS,
        )
    };
    unsafe { &*record() }.set_batch_size(ROOTS);
    let _ = take_stamps();
    let _ = served_by_a_collector();
    assert!(!unsafe { &*record() }.live_list().is_null());

    crate::cycle::epoch::turn_this_threads_cell();
    unsafe { ll_gc_maybe_collect() };
    assert!(unsafe { &*record() }.live_list().is_null(), "given back");
    assert_eq!(take_stamps(), 0);
    assert!(stamps(&core.members).iter().all(|&(_, age)| age == 0));

    unsafe { let_the_core_go(&mut arena, core) };
}

/// A listed member whose death under `POSTED` empties its block — a large
/// entity, the sole occupant of a pooled block — has the list stamped from
/// before the block goes back, and the list is gone after it: a stamp made at
/// the later take would land in the block's next life. The block is drawn
/// back from the pool and filled, and the take leaves the fill whole.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn a_block_emptied_under_posted_is_stamped_from_before_it_goes_back() {
    let _g = test_guard();
    reset_lanes();
    let epoch = a_nonzero_epoch();
    let node = member_class("LiveListBlockNode");
    let wide = wide_class("LiveListWideMember", POOLED_FILLERS, None);
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut core = unsafe { live_core(&mut arena, &[node, node, node, wide, node, node], 1) };
    unsafe { &*record() }.set_batch_size(1);
    let _ = (take_stamps(), take_stamps_at_a_return());
    let _ = served_by_a_collector();
    assert!(!unsafe { &*record() }.live_list().is_null());

    // The wide member's only reference is its predecessor's edge: moving the
    // edge past it kills it, and its block goes back with the free.
    let dead = core.members[3];
    let its_block = BlockHeader::of_ptr(dead as *const u8);
    unsafe { store_prop(arena_ptr, core.members[2], prop_offset(0), core.members[4]) };
    let standing = unsafe { &*record() }.live_list();
    if !standing.is_null() {
        crate::cycle::live_list::drop_this_threads();
    }

    assert!(standing.is_null(), "the block's return consumed the list");
    assert_eq!(take_stamps_at_a_return(), 1);
    assert_eq!(
        take_stamps(),
        6,
        "every member listed, the dead one included"
    );

    // The pool's thread cache hands the block back first.
    let drawn = BlockPool::global().get();
    assert_eq!(drawn, its_block, "the pool handed the same block back");
    let fill = unsafe { (drawn as *mut u8).add(LINE_SIZE) };
    unsafe { std::ptr::write_bytes(fill, 0xA5, 64) };
    unsafe { ll_gc_maybe_collect() };
    let whole = unsafe { std::slice::from_raw_parts(fill, 64) }
        .iter()
        .all(|&byte| byte == 0xA5);
    BlockPool::global().put(drawn);
    assert!(whole, "the take wrote nothing into the block's next life");

    // The dead member's memory is the block's next life's, and the teardown
    // reads no slot of it.
    core.members.remove(3);
    assert_eq!(stamps(&core.members), vec![(epoch, 1); 5]);
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A listed member held in an OS-direct run whose death under `POSTED`
/// unmaps the run has the list stamped from before the unmapping.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn a_run_freed_under_posted_is_stamped_from_before_it_is_unmapped() {
    let _g = test_guard();
    reset_lanes();
    let epoch = a_nonzero_epoch();
    let node = member_class("LiveListRunNode");
    let wide = wide_class("LiveListRunMember", RUN_FILLERS, None);
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut core = unsafe { live_core(&mut arena, &[node, node, wide, node], 1) };
    unsafe { &*record() }.set_batch_size(1);
    let _ = (take_stamps(), take_stamps_at_a_return());
    let _ = served_by_a_collector();
    assert!(!unsafe { &*record() }.live_list().is_null());

    unsafe { store_prop(arena_ptr, core.members[1], prop_offset(0), core.members[3]) };
    let standing = unsafe { &*record() }.live_list();
    if !standing.is_null() {
        crate::cycle::live_list::drop_this_threads();
    }

    assert!(standing.is_null(), "the run's unmapping consumed the list");
    assert_eq!(take_stamps_at_a_return(), 1);
    assert_eq!(take_stamps(), 4);
    // The dead member's run is unmapped, and the teardown reads no slot of it.
    core.members.remove(2);
    assert_eq!(stamps(&core.members), vec![(epoch, 1); 3]);

    unsafe { ll_gc_maybe_collect() };
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A pressure collection that takes the token from `POSTED` gives the list
/// back unstamped at its take, before it tears anything down: a destructor
/// the collection runs finds the word null and the list's blocks already
/// back. The stamps are a later trace's saving, and a thread short of memory
/// wants the blocks.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn a_pressure_collection_under_posted_gives_the_list_back_before_its_teardown() {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    static GIVEN_BACK_BEFORE: AtomicBool = AtomicBool::new(false);
    static DESTROYED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn reads_the_list(_object: *mut Object) {
        let record = unsafe { &*crate::cycle::mutator_record::this_thread_record() };
        let given_back = record.live_list().is_null()
            && take_blocks_across_a_list().is_some_and(|(before, after)| before == after);
        if DESTROYED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            GIVEN_BACK_BEFORE.store(given_back, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let core = unsafe {
        live_core(
            &mut arena,
            &vec![member_class("LiveListPressureMember"); MEMBERS],
            ROOTS,
        )
    };
    unsafe { &*record() }.set_batch_size(ROOTS);
    let _ = (take_stamps(), take_blocks_across_a_list());
    let _ = served_by_a_collector();
    assert!(!unsafe { &*record() }.live_list().is_null());

    // Garbage registered in R after the batch, which the pressure path's
    // trace over R finds and tears down.
    let dying = ClassBuilder::new("LiveListPressureGarbage")
        .prop("next", true)
        .destructor(reads_the_list as *const ())
        .build();
    let _garbage = unsafe { crate::cycle::testing::ring(&mut arena, [dying, dying]) };
    DESTROYED.store(0, std::sync::atomic::Ordering::Relaxed);

    unsafe { crate::cycle::collect::collect_under_pressure() };
    assert_eq!(
        DESTROYED.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "the collection tore the ring down"
    );
    assert!(
        GIVEN_BACK_BEFORE.load(std::sync::atomic::Ordering::Relaxed),
        "the list was given back, blocks and all, before the first destructor"
    );
    assert_eq!(take_stamps(), 0);
    assert!(stamps(&core.members).iter().all(|&(_, age)| age == 0));

    unsafe { let_the_core_go(&mut arena, core) };
}

/// The allocation a destructor makes under pressure is refused a collection,
/// and the retirement pass its refusal runs holds the byte at `POSTED`: the
/// list goes back unstamped there too.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn a_teardowns_refusal_under_posted_gives_the_list_back_unstamped() {
    unsafe extern "C" fn asks_under_pressure(_object: *mut Object) {
        unsafe { crate::cycle::collect::collect_under_pressure() };
    }

    let _g = test_guard();
    reset_lanes();
    let _ = a_nonzero_epoch();
    let mut arena = Arena::new();
    let core = unsafe {
        live_core(
            &mut arena,
            &vec![member_class("LiveListRefusalMember"); MEMBERS],
            ROOTS,
        )
    };
    let dying = unsafe {
        let mut context = LLContext { arena: &mut arena };
        new_constructed(
            &mut context,
            ClassBuilder::new("LiveListRefusalDying")
                .destructor(asks_under_pressure as *const ())
                .build(),
            MemoryCategory::GcHeap,
        )
    };
    unsafe { &*record() }.set_batch_size(ROOTS);
    let _ = take_stamps();
    let _ = served_by_a_collector();
    assert!(!unsafe { &*record() }.live_list().is_null());

    unsafe {
        assert!(ll_release(dying as *mut RcHeader));
        ll_object_die(dying);
    }
    assert!(unsafe { &*record() }.live_list().is_null(), "given back");
    assert_eq!(take_stamps(), 0);

    unsafe { ll_gc_maybe_collect() };
    assert!(stamps(&core.members).iter().all(|&(_, age)| age == 0));
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A recall raised inside the list's walk of a part, past the walk's first
/// reading, is read within a stride of rows and ends the batch before the
/// release, and the rows listed before it stay listed: they are live rows of
/// a part that completed (`dev/S65-PLAN-CRITIC.md`, F1). The core holds three
/// strides of live rows, so a walk that read the recall once, before it began,
/// would pass the raise by two strides.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn a_recall_raised_inside_the_list_walk_is_read_within_a_stride() {
    use crate::cycle::arena::RECALL_STRIDE;
    let _g = test_guard();
    reset_lanes();
    let members = 3 * RECALL_STRIDE;
    let raised_after = RECALL_STRIDE + 10;
    let mut arena = Arena::new();
    let core = unsafe {
        live_core(
            &mut arena,
            &vec![member_class("LiveListRecalledMember"); members],
            1,
        )
    };
    unsafe { &*record() }.set_batch_size(1);
    let _ = take_stamps();

    let token = unsafe { &raw const (*record()).token } as usize;
    after_listed_rows(
        raised_after,
        Box::new(move || {
            unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
        }),
    );
    let (served, traced) = a_traced_serve();
    unsafe { &*record() }.token.recall_for_test(false);
    assert_eq!(
        served,
        Served::Batch {
            roots: 1,
            complete: false,
            backlog: false,
        },
        "the recall ended the batch"
    );
    let positions = traced
        .positions_after_the_hook
        .expect("the walk ran the hook");
    assert!(
        (1..=RECALL_STRIDE).contains(&positions),
        "the walk read the recall within a stride of the raise: {positions} positions"
    );

    unsafe { ll_gc_maybe_collect() };
    let stamped = take_stamps();
    assert!(
        (raised_after..raised_after + RECALL_STRIDE).contains(&stamped),
        "the rows listed up to the reading stayed listed: {stamped}"
    );
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A list the owner has not taken is the collector's to give back once the
/// epoch has advanced past its publication, and not before: the owner's take
/// would give it back unread from then on. The collector's side runs on a
/// thread of the case's that stands in for it, writing the instant of the
/// advance as a round would.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn a_list_is_given_back_by_the_collector_after_an_advance_and_not_before() {
    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let core = unsafe {
        live_core(
            &mut arena,
            &vec![member_class("LiveListStaleMember"); MEMBERS],
            ROOTS,
        )
    };
    unsafe { &*record() }.set_batch_size(ROOTS);
    let _ = take_stamps();
    let _ = served_by_a_collector();
    let published_at = unsafe { &*record() }.live_list_published_at();
    assert!(!unsafe { &*record() }.live_list().is_null());

    let sent = crate::cycle::testing::Sent(record());
    let (before, after, blocks) = std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        let record = unsafe { &*sent.into_inner() };
        let _ = take_stale_lists_given_back();
        record.note_advanced_at(published_at);
        crate::cycle::live_list::give_back_a_stale_list(record);
        let before = take_stale_lists_given_back();
        record.note_advanced_at(published_at + 1);
        crate::cycle::live_list::give_back_a_stale_list(record);
        (
            before,
            take_stale_lists_given_back(),
            take_blocks_across_a_list(),
        )
    })
    .join()
    .unwrap();
    assert_eq!(
        (before, after),
        (0, 1),
        "given back after an advance past the publication, and not at it"
    );
    let (taken_over, released) = blocks.expect("the collector gave the list back");
    assert_eq!(taken_over, released, "on the thread that took it over");

    unsafe { ll_gc_maybe_collect() };
    assert_eq!(take_stamps(), 0, "the owner's take found no list");
    unsafe { let_the_core_go(&mut arena, core) };
}

/// The collector's round gives a stale list back by itself: an owner that
/// does not take its token after a grant, its epoch advanced by the elder's
/// visits, finds the word null while its byte still reads `POSTED`.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn the_round_gives_back_a_list_its_owner_left_standing() {
    let _g = test_guard();
    let _end = RetireOnDrop;
    reset_lanes();
    let mut arena = Arena::new();
    let core = unsafe {
        live_core(
            &mut arena,
            &vec![member_class("LiveListRoundMember"); MEMBERS],
            ROOTS,
        )
    };
    unsafe { &*record() }.set_batch_size(ROOTS);
    let _ = take_stamps();
    let _ = served_by_a_collector();
    assert!(!unsafe { &*record() }.live_list().is_null());

    testing::advance_epochs_after(Some(std::time::Duration::from_millis(1)));
    born_over(&[record()]);
    // This thread reads its byte between sleeps and never takes the token, so
    // `POSTED` stands for the whole wait.
    let given_back = wait_until(|| unsafe { &*record() }.live_list().is_null(), A_BIRTH);
    let byte = unsafe { &*record() }.token.read();
    testing::retire();
    testing::advance_epochs_after(None);
    assert!(given_back, "the elder's round gave the list back");
    assert_eq!(
        crate::cycle::token::state(byte),
        crate::cycle::token::POSTED,
        "while the owner had not taken its token"
    );

    unsafe { ll_gc_maybe_collect() };
    assert_eq!(take_stamps(), 0);
    unsafe { let_the_core_go(&mut arena, core) };
}

/// A chain at its bound keeps what it holds and takes no more: a core past
/// one block's entries, the chain bounded to one block, stamps exactly the
/// entries that block holds.
#[test]
#[cfg_attr(miri, ignore = "8,260 members are past what Miri affords")]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "a batch whose roots all read live posts nothing into P under the chain and publishes no live list (`crate::cycle::chain`)"
)]
fn a_chain_at_its_bound_keeps_what_it_holds() {
    use crate::cycle::live_list::ENTRIES_PER_BLOCK;
    let _g = test_guard();
    reset_lanes();
    let _bound = bound_the_chain(1);
    let members = ENTRIES_PER_BLOCK + 100;
    let mut arena = Arena::new();
    let core = unsafe {
        live_core(
            &mut arena,
            &vec![member_class("LiveListBoundedMember"); members],
            1,
        )
    };
    unsafe { &*record() }.set_batch_size(1);
    let _ = take_stamps();
    let _ = served_by_a_collector();

    unsafe { ll_gc_maybe_collect() };
    assert_eq!(take_stamps(), ENTRIES_PER_BLOCK);
    let stamped = stamps(&core.members)
        .iter()
        .filter(|&&(_, age)| age == 1)
        .count();
    assert_eq!(stamped, ENTRIES_PER_BLOCK);
    unsafe { let_the_core_go(&mut arena, core) };
}
