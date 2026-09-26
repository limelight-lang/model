//! What one serve does for a mutator: the batch it takes from behind the
//! mutator's writer, clamped to P's room; the verdict per root, in the order
//! of the parts that gave them; the advance that follows the last post from
//! the return and from the unwind alike; the budget of each part, past which
//! a part with the grant's retry spent defers the roots it met and the batch
//! goes on, K standing; the
//! skip of a mutator collecting in line; and a mutator registering
//! throughout, whose registrations come out once each.
//!
//! The collector is a thread of the case's that calls [`serve`] on this
//! thread's record, started through `ll_thread_init` as the real one is.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::queue::verdicts::{
    Verdict, discard_standing_verdicts, standing_verdicts, verdict_count,
};
use crate::cycle::queue::{candidate_count, collect_lane_tokens, deferred_count, refill_spares};
use crate::cycle::testing::{Sent, ring};
use crate::gc::ll_gc_maybe_collect;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::ring::BLOCK_ENTRIES;
use std::sync::atomic::{AtomicUsize, Ordering};

/// One serve of this thread's record on a thread of its own, this thread
/// consenting at its byte meanwhile as its poll would, joined. A `POSTED`
/// left by an earlier batch is cleared first: these cases batch again
/// without the collection between that the byte asks for, and dispose of P
/// by hand at their end.
pub(super) fn served_by_a_collector() -> Served {
    unsafe { &*record() }.clear_posted_for_test();
    let sent = Sent(record());
    testing::consent_while(std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        unsafe { testing::serve_alone(sent.into_inner()) }
    }))
}

/// A class with one counted Box property at `prop_offset(0)`, which is what
/// [`ring`] links members through.
fn node_class(name: &str) -> *const Class {
    ClassBuilder::new(name)
        .prop("next", true)
        .destructor(counting_destructor as *const ())
        .build()
}

/// A class with one counted Box property, through which a case holds an
/// object from outside.
pub(super) fn keeper_class(name: &str) -> *const Class {
    ClassBuilder::new(name).prop("kept", true).build()
}

/// One object of `class` at count one, in this thread's heap.
pub(super) unsafe fn object(arena: &mut Arena, class: *const Class) -> *mut Object {
    let mut context = LLContext { arena };
    unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
}

/// A registered candidate whose death then completed in place.
unsafe fn completed_death(arena: &mut Arena, class: *const Class) -> *mut RcHeader {
    let entity = unsafe { object(arena, class) } as *mut RcHeader;
    unsafe {
        ll_retain(entity);
        assert!(!ll_release(entity), "registered at the non-final decrement");
        assert!(ll_release(entity));
        ll_object_die(entity as *mut Object);
    }
    entity
}

/// A registered root a keeper holds: the root, and the keeper the case
/// takes down afterwards.
pub(super) unsafe fn kept_root(
    arena: &mut Arena,
    node: *const Class,
    name: &str,
) -> (*mut RcHeader, *mut Object) {
    let root = unsafe { object(arena, node) };
    let keeper = unsafe { object(arena, keeper_class(name)) };
    unsafe {
        crate::test_support::store_prop(arena, keeper, crate::test_support::prop_offset(0), root);
        assert!(!ll_release(root as *mut RcHeader), "the keeper holds it");
    }
    (root as *mut RcHeader, keeper)
}

pub(super) unsafe fn release_keeper(keeper: *mut Object) {
    unsafe {
        assert!(ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }
}

/// Destructor bodies run since a case last cleared it.
static DESTRUCTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    DESTRUCTOR_RUNS.fetch_add(1, Ordering::Relaxed);
}

/// The verdicts standing in P, in order.
fn verdicts() -> Vec<Verdict> {
    standing_verdicts()
        .iter()
        .map(|&(_, verdict)| verdict)
        .collect()
}

/// The verdicts go into P in the order of the parts that gave them, the roots
/// no part can place ahead of every part: the completed death first, then
/// the ring's part with both its roots, then each kept root's part.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_batch_posts_one_verdict_per_root_in_the_parts_order_and_advances_past_them() {
    let _g = test_guard();
    reset_lanes();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let node = node_class("BatchNode");
    let mut arena = Arena::new();
    // R, in order: a garbage ring's two roots, a kept root, a completed
    // death, a second kept root.
    let _garbage = unsafe { ring(&mut arena, [node, node]) };
    let (_, keeper_a) = unsafe { kept_root(&mut arena, node, "BatchKeeperA") };
    let _dead = unsafe { completed_death(&mut arena, node) };
    let (_, keeper_b) = unsafe { kept_root(&mut arena, node, "BatchKeeperB") };
    assert_eq!(candidate_count(), 5);
    // The completed death ran its destructor on the way; the count from
    // here is the collection's.
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let mut expected = Vec::new();
    collect_lane_tokens(&mut expected);
    let batches = unsafe { &*record() }.batches_since_the_advance();
    // K at R's count, so that the batch takes its whole clamp.
    unsafe { &*record() }.set_batch_size(5);

    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 5,
            complete: true,
            backlog: false,
        }
    );
    assert_eq!(
        unsafe { &*record() }.batches_since_the_advance(),
        batches.saturating_add(1),
        "the batch counts toward the epoch's advance"
    );
    assert_eq!(candidate_count(), 0, "R's front moved past the batch");
    assert_eq!(
        verdicts(),
        vec![
            Verdict::ZeroCount,
            Verdict::Proposed,
            Verdict::Proposed,
            Verdict::ReadLive,
            Verdict::ReadLive,
        ],
        "one verdict per root, in the parts' order"
    );
    let mut standing = Vec::new();
    collect_lane_tokens(&mut standing);
    // R's order was the ring, the first kept root, the death, the second.
    let death = expected.remove(3);
    expected.insert(0, death);
    assert_eq!(standing, expected, "every token once, now in P");
    assert_eq!(
        record_batch_size(),
        10,
        "a completed batch that took its clamp doubles K"
    );

    // The mutator's poll: the deaths retired and the kept roots deferred up
    // to the first proposal — which follows the death, so the poll arms and
    // the collection takes the ring out of P and disposes of the rest.
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the ring was collected"
    );
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
    assert_eq!(verdict_count(), 0);
    assert_eq!(deferred_count(), 2, "the kept roots wait for the turnover");
    assert_eq!(candidate_count(), 0);

    unsafe {
        release_keeper(keeper_a);
        release_keeper(keeper_b);
    }
    reset_lanes();
}

/// This thread's record's batch size.
pub(super) fn record_batch_size() -> usize {
    unsafe { &*record() }.batch_size()
}

#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_batch_is_clamped_to_ps_room_and_to_k() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("ClampNode");
    let mut arena = Arena::new();
    let keepers: Vec<*mut Object> = (0..INITIAL_BATCH + 3)
        .map(|index| unsafe { kept_root(&mut arena, node, &format!("ClampKeeper{index}")) }.1)
        .collect();
    assert_eq!(candidate_count(), INITIAL_BATCH + 3);

    // K first: the first batch takes the starting size and leaves three.
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: INITIAL_BATCH,
            complete: true,
            backlog: true,
        }
    );
    assert_eq!(candidate_count(), 3);
    assert_eq!(verdict_count(), INITIAL_BATCH);
    assert_eq!(
        record_batch_size(),
        INITIAL_BATCH * 2,
        "a batch that took its clamp doubles K"
    );

    // Then P's room: filled to three short of full, the next batch takes
    // three and no more, whatever K says; and a full P takes nothing.
    let room_left = 3;
    unsafe {
        crate::cycle::queue::verdicts::testing::fill_for_test(
            BLOCK_ENTRIES - INITIAL_BATCH - room_left,
        )
    };
    assert_eq!(verdict_count(), BLOCK_ENTRIES - room_left);
    unsafe { kept_root(&mut arena, node, "ClampKeeperLate") };
    assert_eq!(candidate_count(), 4);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: room_left,
            complete: true,
            backlog: true,
        }
    );
    assert_eq!(candidate_count(), 1);
    assert_eq!(
        record_batch_size(),
        INITIAL_BATCH * 2,
        "a batch P's room cut short of its clamp leaves K"
    );
    assert_eq!(
        served_by_a_collector(),
        Served::Idle,
        "a full P takes nothing"
    );
    assert_eq!(candidate_count(), 1);
    assert!(
        !unsafe { &*record() }.token.is_held(),
        "and made no claim to find that out"
    );

    discard_standing_verdicts();
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: false,
        }
    );

    // K's bound: a completed batch that took its clamp at the bound stays at
    // it. Sized by hand, since a batch of the bound's thousand roots would
    // be built for this one reading.
    size_the_next_batch(unsafe { &*record() }, BATCH_BOUND, BATCH_BOUND, true);
    assert_eq!(
        record_batch_size(),
        BATCH_BOUND,
        "K does not pass its bound"
    );
    assert!(BATCH_BOUND < BLOCK_ENTRIES);

    discard_standing_verdicts();
    for keeper in keepers {
        unsafe { release_keeper(keeper) };
    }
    reset_lanes();
}

/// A part that meets its budget with the grant's retry spent defers the roots
/// it met read live and the batch goes on: the parts before it stand with
/// their verdicts, the retried one's among them, the root after it opens a
/// part of its own, and K stands, no root having been lost to `Unwalked`.
///
/// Two long rings registered in turn put both into one batch behind a small
/// one: the first long ring's part meets B and its retry under `B_max`
/// finishes, and the second's part meets B with the retry spent. A kept root
/// registered after the rings' first pair comes after that part.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_part_past_b_with_the_retry_spent_defers_the_roots_it_met_and_the_batch_goes_on() {
    let _g = test_guard();
    reset_lanes();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let node = node_class("BudgetNode");
    let mut arena = Arena::new();
    let small = unsafe { ring(&mut arena, [node, node]) };
    unsafe { &*record() }.set_batch_size(16);

    // Parts that may draw no block past the workspace: the small ring's part
    // and the kept root's fit it, and each long ring's meets the budget.
    testing::budget_the_next_batch(0);
    let big = node_class("BudgetBigRing");
    let mut keeper = std::ptr::null_mut();
    let mut kept = std::ptr::null_mut();
    let (retried, deferred) = unsafe {
        long_rings_registered_in_turn(&mut arena, big, 16_000, |arena| {
            (kept, keeper) = kept_root(arena, node, "BudgetKeeper");
        })
    };
    testing::read_traced_batches(true);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 16,
            complete: false,
            backlog: true,
        }
    );
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert_eq!(
        traced
            .iter()
            .map(|batch| (
                batch.parts,
                batch.parts_met_budget,
                batch.retried,
                batch.deferred_parts
            ))
            .collect::<Vec<_>>(),
        vec![(4, 2, true, 1)],
        "the second part met B and was retried, the third met B with the retry spent, and \
         the kept root's part followed it"
    );
    let ring_of = |entity: *mut RcHeader| {
        let object = entity as *mut Object;
        if small.contains(&object) {
            "small"
        } else if retried.contains(&object) {
            "retried"
        } else if deferred.contains(&object) {
            "deferred"
        } else {
            assert_eq!(entity, kept);
            "kept"
        }
    };
    let mut expected = vec![("small", Verdict::Proposed); 2];
    expected.extend([("retried", Verdict::Proposed); 7]);
    expected.extend([("deferred", Verdict::ReadLive); 6]);
    expected.push(("kept", Verdict::ReadLive));
    assert_eq!(
        standing_verdicts()
            .iter()
            .map(|&(entity, verdict)| (ring_of(entity), verdict))
            .collect::<Vec<_>>(),
        expected,
        "the first two parts' verdicts, the deferred roots, then the kept root's part"
    );
    assert_eq!(record_batch_size(), 16, "K stands");

    // The mutator's collection over P frees the two proposed rings and
    // defers the rest; the deferred ring stands in R and in the deferred
    // lane until a collection over R, which frees it whole.
    assert_eq!(unsafe { ll_gc_maybe_collect() }, 2 + retried.len());
    assert_eq!(
        deferred_count(),
        7,
        "the deferred part's six roots and the kept root"
    );
    unsafe { release_keeper(keeper) };
    assert!(refill_spares());
    assert_eq!(unsafe { crate::gc::ll_gc_collect_cycles() }, deferred.len());
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        2 + retried.len() + deferred.len() + 1
    );
    assert_eq!(verdict_count(), 0);
    reset_lanes();
}

/// Two rings of `members` each, their members released in turn — the
/// first's, then the second's — so that R holds them interleaved, with what
/// `after_the_first_pair` registers behind their first pair. The spare
/// segments are refilled after each pair, as the poll a loop this long owes
/// would refill them, so that R grows past what the overflow buffer holds.
///
/// # Safety
/// As [`crate::cycle::testing::long_ring`].
unsafe fn long_rings_registered_in_turn(
    arena: &mut Arena,
    class: *const Class,
    members: usize,
    after_the_first_pair: impl FnOnce(&mut Arena),
) -> (Vec<*mut Object>, Vec<*mut Object>) {
    let mut context = LLContext { arena: &mut *arena };
    let mut ring = || -> Vec<*mut Object> {
        (0..members)
            .map(|_| unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) })
            .collect()
    };
    let (first, second) = (ring(), ring());
    for ring in [&first, &second] {
        for (index, &member) in ring.iter().enumerate() {
            unsafe {
                crate::test_support::store_prop(
                    arena,
                    member,
                    crate::test_support::prop_offset(0),
                    ring[(index + 1) % members],
                )
            };
        }
    }

    let mut after_the_first_pair = Some(after_the_first_pair);
    for (&one, &other) in first.iter().zip(&second) {
        for member in [one, other] {
            assert!(
                !unsafe { ll_release(member as *mut RcHeader) },
                "an edge of the ring holds this member"
            );
        }

        if let Some(act) = after_the_first_pair.take() {
            act(arena);
        }

        assert!(refill_spares(), "the pool refilled the spares");
    }

    (first, second)
}

/// A root met by an earlier part opens none: over R = [r1, r3, r2], where
/// r2 is inside r1's closure and r3 is not, the batch runs two parts and
/// posts three verdicts, r2's with r1's.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_root_an_earlier_part_met_is_posted_with_that_part() {
    let _g = test_guard();
    reset_lanes();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let node = node_class("MetNode");
    let mut arena = Arena::new();
    let r1 = unsafe { object(&mut arena, node) };
    let r2 = unsafe { object(&mut arena, node) };
    unsafe {
        crate::test_support::store_prop(&mut arena, r1, crate::test_support::prop_offset(0), r2);
        crate::test_support::store_prop(&mut arena, r2, crate::test_support::prop_offset(0), r1);
        assert!(!ll_release(r1 as *mut RcHeader), "r2's edge holds r1");
    }
    let (r3, keeper) = unsafe { kept_root(&mut arena, node, "MetKeeper") };
    assert!(
        !unsafe { ll_release(r2 as *mut RcHeader) },
        "r1's edge holds r2"
    );
    let mut in_r = Vec::new();
    collect_lane_tokens(&mut in_r);
    assert_eq!(in_r.len(), 3);
    unsafe { &*record() }.set_batch_size(3);

    testing::read_traced_batches(true);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 3,
            complete: true,
            backlog: false,
        }
    );
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert_eq!(
        traced.iter().map(|batch| batch.parts).collect::<Vec<_>>(),
        vec![2],
        "r1's part and r3's, and none for r2"
    );
    assert_eq!(
        standing_verdicts(),
        vec![
            (r1 as *mut RcHeader, Verdict::Proposed),
            (r2 as *mut RcHeader, Verdict::Proposed),
            (r3, Verdict::ReadLive),
        ],
        "r2 posted with the part that met it, ahead of r3's"
    );

    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the ring was collected"
    );
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), 2);
    discard_standing_verdicts();
    unsafe { release_keeper(keeper) };
    reset_lanes();
}

/// The lookup of the roots a part met reads the copy in the order of the
/// roots' addresses: over R = [r1, y, r2], r2 inside r1's closure and y a
/// root of its own in a block above theirs, the part over r1 finds r2 past y,
/// which in R's order stands between them and past the block both are in.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_root_of_a_higher_block_between_two_met_roots_hides_neither() {
    let _g = test_guard();
    reset_lanes();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let node = node_class("OrderNode");
    let mut arena = Arena::new();
    let block_of =
        |object: *mut Object| object as usize & !(crate::memory::block_pool::BLOCK_SIZE - 1);
    // Objects until two of them share the lowest block among all of them and
    // a third stands in a block above it, whichever way the heap hands out
    // its blocks: the two are the ring, the third is y, the rest go back.
    let mut objects: Vec<*mut Object> = Vec::new();
    let (r1, r2, y) = loop {
        objects.push(unsafe { object(&mut arena, node) });
        let lowest = objects
            .iter()
            .map(|&object| block_of(object))
            .min()
            .expect("one object at least");
        let mut in_the_lowest = objects
            .iter()
            .copied()
            .filter(|&object| block_of(object) == lowest);
        let above = objects
            .iter()
            .copied()
            .find(|&object| block_of(object) > lowest);
        if let (Some(r1), Some(r2), Some(y)) = (in_the_lowest.next(), in_the_lowest.next(), above) {
            break (r1, r2, y);
        }

        assert!(
            objects.len() < 4 * crate::cycle::loads::slots_per_block(16),
            "the heap handed out two blocks"
        );
    };
    let fillers: Vec<*mut Object> = objects
        .into_iter()
        .filter(|&object| object != r1 && object != r2 && object != y)
        .collect();
    unsafe {
        crate::test_support::store_prop(&mut arena, r1, crate::test_support::prop_offset(0), r2);
        crate::test_support::store_prop(&mut arena, r2, crate::test_support::prop_offset(0), r1);
        assert!(!ll_release(r1 as *mut RcHeader), "r2's edge holds r1");
        ll_retain(y as *mut RcHeader);
        assert!(
            !ll_release(y as *mut RcHeader),
            "y's creation reference holds it"
        );
        assert!(!ll_release(r2 as *mut RcHeader), "r1's edge holds r2");
    }
    unsafe { &*record() }.set_batch_size(3);

    testing::read_traced_batches(true);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 3,
            complete: true,
            backlog: false,
        }
    );
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert_eq!(
        traced.iter().map(|batch| batch.parts).collect::<Vec<_>>(),
        vec![2],
        "r1's part met r2, and y opened the second"
    );
    assert_eq!(
        standing_verdicts(),
        vec![
            (r1 as *mut RcHeader, Verdict::Proposed),
            (r2 as *mut RcHeader, Verdict::Proposed),
            (y as *mut RcHeader, Verdict::ReadLive),
        ]
    );

    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the ring was collected"
    );
    discard_standing_verdicts();
    for object in fillers.into_iter().chain([y]) {
        unsafe {
            assert!(ll_release(object as *mut RcHeader));
            ll_object_die(object);
        }
    }
    reset_lanes();
}

/// Bytes of the objects [`spread_ring`] builds: a size class whose touched
/// block reserves a row array of about 2 KiB.
const SPREAD_CLASS_BYTES: usize = 128;

/// A class of [`SPREAD_CLASS_BYTES`]: the header and the class word take
/// sixteen bytes and each counted property sixteen.
fn spread_class(name: &str) -> *const Class {
    let mut builder = ClassBuilder::new(name).destructor(counting_destructor as *const ());
    for property in 0..(SPREAD_CLASS_BYTES - 16) / 16 {
        builder = builder.prop(&format!("p{property}"), true);
    }

    builder.build()
}

/// A garbage ring of `members` objects of `class`, one per block, its first
/// member the one registered root: every other slot of a member's block
/// holds a filler, so the part over the root reserves a row array per
/// member. Answers the members and the fillers, which the case kills.
///
/// # Safety
/// As `cycle::testing::ring`, and `class` is a [`spread_class`].
unsafe fn spread_ring(
    arena: &mut Arena,
    class: *const Class,
    members: usize,
) -> (Vec<*mut Object>, Vec<*mut Object>) {
    let fillers_per_member = crate::cycle::loads::slots_per_block(SPREAD_CLASS_BYTES) - 1;
    let mut fillers = Vec::with_capacity(members * fillers_per_member);
    let ring: Vec<*mut Object> = (0..members)
        .map(|_| {
            let member = unsafe { object(arena, class) };
            for _ in 0..fillers_per_member {
                fillers.push(unsafe { object(arena, class) });
            }

            member
        })
        .collect();
    unsafe {
        for (position, &member) in ring.iter().enumerate() {
            crate::cycle::testing::move_prop(
                member,
                crate::test_support::prop_offset(0),
                ring[(position + 1) % members],
            );
        }

        ll_retain(ring[0] as *mut RcHeader);
        assert!(
            !ll_release(ring[0] as *mut RcHeader),
            "the ring's edge holds the root"
        );
    }

    (ring, fillers)
}

/// Each part draws under the whole budget: under a budget of one block, two
/// rings whose rows each pass the workspace by less than a block both
/// complete with no part meeting B, where a budget shared by the batch would
/// have refused the second part's block and sent it to the retry.
#[test]
fn every_part_draws_under_the_budget_of_its_own() {
    let _g = test_guard();
    reset_lanes();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let class = spread_class("SpreadNode");
    let mut arena = Arena::new();
    let array = crate::cycle::shadow::bytes_for(crate::cycle::loads::slots_per_block(
        SPREAD_CLASS_BYTES,
    ) as u32);
    // Past the workspace by half a block of row arrays, and so one block
    // short of the budget's refusal.
    let members = (crate::cycle::arena::WORKSPACE_BUMP_BYTES
        + crate::memory::block_pool::BLOCK_PAYLOAD / 2)
        / array;
    assert!(members * array > crate::cycle::arena::WORKSPACE_BUMP_BYTES);
    assert!(
        members * array
            < crate::cycle::arena::WORKSPACE_BUMP_BYTES + crate::memory::block_pool::BLOCK_PAYLOAD
                - 8 * 1024,
        "a block and the worklist's segment hold what the workspace does not"
    );
    let (first, first_fillers) = unsafe { spread_ring(&mut arena, class, members) };
    let (second, second_fillers) = unsafe { spread_ring(&mut arena, class, members) };
    unsafe { &*record() }.set_batch_size(2);

    testing::budget_the_next_batch(1);
    testing::read_traced_batches(true);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 2,
            complete: true,
            backlog: false,
        }
    );
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert_eq!(
        traced
            .iter()
            .map(|batch| (batch.parts, batch.parts_met_budget))
            .collect::<Vec<_>>(),
        vec![(2, 0)],
        "the second part drew its block as the first did"
    );
    assert_eq!(verdicts(), vec![Verdict::Proposed; 2]);

    assert_eq!(unsafe { ll_gc_maybe_collect() }, first.len() + second.len());
    for filler in first_fillers.into_iter().chain(second_fillers) {
        unsafe {
            assert!(ll_release(filler as *mut RcHeader));
            ll_object_die(filler);
        }
    }
    reset_lanes();
}

#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn the_advance_follows_the_last_post_from_the_unwind_as_well() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("UnwindNode");
    let mut arena = Arena::new();
    let (_, keeper_a) = unsafe { kept_root(&mut arena, node, "UnwindKeeperA") };
    let (_, keeper_b) = unsafe { kept_root(&mut arena, node, "UnwindKeeperB") };

    testing::panic_before_the_next_advance();
    let sent = Sent(record());
    let outcome = testing::consent_while(std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        let record = sent.into_inner();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            testing::serve_alone(record)
        }))
    }));
    assert!(outcome.is_err(), "the batch panicked where the case asked");
    assert_eq!(
        candidate_count(),
        0,
        "the guard advanced R past the batch from the unwind"
    );
    assert_eq!(
        verdicts(),
        vec![Verdict::ReadLive; 2],
        "and both verdicts stand"
    );
    assert!(
        !unsafe { &*record() }.token.is_held(),
        "and the token was released"
    );

    unsafe {
        release_keeper(keeper_a);
        release_keeper(keeper_b);
    }
    reset_lanes();
}

/// An unwind inside the trace, before any verdict: the guard posts every root
/// `Unwalked` and advances R past them once, so the roots come back to the
/// mutator's exact trace rather than standing in R for the next batch, and the
/// release is to `POSTED`.
#[test]
fn an_unwind_inside_the_trace_posts_every_root_unwalked_and_advances_once() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("TraceUnwindNode");
    let mut arena = Arena::new();
    let (_, keeper_a) = unsafe { kept_root(&mut arena, node, "TraceUnwindKeeperA") };
    let (_, keeper_b) = unsafe { kept_root(&mut arena, node, "TraceUnwindKeeperB") };

    testing::at_the_start_of_the_next_trace(Box::new(|| {
        panic!("a trace unwound before its first verdict, by the case's request")
    }));
    let sent = Sent(record());
    let outcome = testing::consent_while(std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        let record = sent.into_inner();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            testing::serve_alone(record)
        }))
    }));
    assert!(outcome.is_err(), "the trace panicked where the case asked");
    assert_eq!(candidate_count(), 0, "the guard advanced R past the batch");
    assert_eq!(
        verdicts(),
        vec![Verdict::Unwalked; 2],
        "each root once, unwalked"
    );
    assert_eq!(
        crate::cycle::token::state(unsafe { &*record() }.token.read()),
        crate::cycle::token::POSTED,
        "and the release was to POSTED"
    );

    discard_standing_verdicts();
    unsafe { &*record() }.clear_posted_for_test();
    unsafe {
        release_keeper(keeper_a);
        release_keeper(keeper_b);
    }
    reset_lanes();
}

/// An unwind inside the second part, its trace done and its rows standing:
/// the first part's verdicts stand, the guard posts every other root
/// `Unwalked` and advances R once, and each root has one verdict.
#[test]
fn an_unwind_inside_the_second_part_keeps_the_first_parts_verdicts() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("PartUnwindNode");
    let mut arena = Arena::new();
    let _garbage = unsafe { ring(&mut arena, [node, node]) };
    let (_, keeper_a) = unsafe { kept_root(&mut arena, node, "PartUnwindKeeperA") };
    let (_, keeper_b) = unsafe { kept_root(&mut arena, node, "PartUnwindKeeperB") };
    unsafe { &*record() }.set_batch_size(4);

    testing::after_the_trace_of(
        2,
        Box::new(|| panic!("the second part unwound, by the case's request")),
    );
    let sent = Sent(record());
    let outcome = testing::consent_while(std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        let record = sent.into_inner();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            testing::serve_alone(record)
        }))
    }));
    assert!(outcome.is_err(), "the part panicked where the case asked");
    assert_eq!(candidate_count(), 0, "the guard advanced R past the batch");
    assert_eq!(
        verdicts(),
        vec![
            Verdict::Proposed,
            Verdict::Proposed,
            Verdict::Unwalked,
            Verdict::Unwalked,
        ],
        "the ring's part, then each root once, unwalked"
    );
    assert_eq!(
        crate::cycle::token::state(unsafe { &*record() }.token.read()),
        crate::cycle::token::POSTED,
        "and the release was to POSTED"
    );

    // The mutator's collection over P takes the ring the first part proposed
    // and traces the unwalked kept roots exactly.
    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the ring was collected"
    );
    assert_eq!(verdict_count(), 0);
    unsafe {
        release_keeper(keeper_a);
        release_keeper(keeper_b);
    }
    reset_lanes();
}

/// Set this thread's recall of its token from the collector's thread, as a
/// take would, without a mutator waiting on the token.
fn recall_from_the_collector() -> Box<dyn FnOnce() + Send> {
    let token = unsafe { &raw const (*record()).token } as usize;
    Box::new(move || {
        unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
    })
}

/// The traced batches' parts and verdicts after one serve, the recall
/// cleared and P disposed of by the mutator's collection over it.
fn parts_and_verdicts_of_a_serve() -> (Vec<usize>, Vec<Verdict>) {
    testing::read_traced_batches(true);
    let served = served_by_a_collector();
    assert!(matches!(served, Served::Batch { .. }), "{served:?}");
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    unsafe { &*record() }.token.recall_for_test(false);
    let posted = verdicts();
    unsafe { ll_gc_maybe_collect() };
    (traced.iter().map(|batch| batch.parts).collect(), posted)
}

/// A recall standing when the trace starts is read at the first root of the
/// pass before the parts: no part opens, and every root is posted `Unwalked`.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_recall_at_the_traces_start_opens_no_part() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("RecallFirstPassNode");
    let mut arena = Arena::new();
    let _garbage = unsafe { ring(&mut arena, [node, node]) };
    unsafe { &*record() }.set_batch_size(2);

    testing::at_the_start_of_the_next_trace(recall_from_the_collector());
    let (parts, posted) = parts_and_verdicts_of_a_serve();
    assert_eq!(parts, vec![0], "the pass before the parts read the recall");
    assert_eq!(posted, vec![Verdict::Unwalked; 2]);
    reset_lanes();
}

/// A recall made during a part shorter than a stride is read before the next
/// part opens: the part's verdicts stand and every later root is `Unwalked`.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_recall_during_a_part_is_read_before_the_next_part() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("RecallBetweenPartsNode");
    let mut arena = Arena::new();
    let _first = unsafe { ring(&mut arena, [node, node]) };
    let _second = unsafe { ring(&mut arena, [node, node]) };
    unsafe { &*record() }.set_batch_size(4);

    testing::after_the_trace_of(1, recall_from_the_collector());
    let (parts, posted) = parts_and_verdicts_of_a_serve();
    assert_eq!(parts, vec![1], "the second part never opened");
    assert_eq!(
        posted,
        vec![
            Verdict::Proposed,
            Verdict::Proposed,
            Verdict::Unwalked,
            Verdict::Unwalked,
        ]
    );
    reset_lanes();
}

/// No reading follows the last part: a recall made after the last part's
/// trace, with a lookup shorter than a stride behind it, leaves the batch
/// complete, every root posted from its part and K doubled.
#[test]
fn a_recall_after_the_last_parts_trace_leaves_the_batch_complete() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("RecallAfterTheLastNode");
    let mut arena = Arena::new();
    let _first = unsafe { ring(&mut arena, [node, node]) };
    let _second = unsafe { ring(&mut arena, [node, node]) };
    unsafe { &*record() }.set_batch_size(4);

    testing::after_the_trace_of(2, recall_from_the_collector());
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 4,
            complete: true,
            backlog: false,
        }
    );
    unsafe { &*record() }.token.recall_for_test(false);
    assert_eq!(verdicts(), vec![Verdict::Proposed; 4]);
    assert_eq!(record_batch_size(), 8, "a complete batch doubles K");
    unsafe { ll_gc_maybe_collect() };
    reset_lanes();
}

/// The lookup of a part's met roots counts a position per root it visits, so
/// a recall made before a lookup longer than a stride stops it: over one ring
/// of [`BATCH_BOUND`] members, every one a root, the lookup ends short of its
/// last root and the batch is incomplete. How many roots it posted first
/// depends on where the ring's blocks fall and in which order the touched list
/// holds them, so the case reads the lookup's visits and not the posts.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_recall_stops_the_lookup_of_met_roots() {
    // A lookup of BATCH_BOUND visits holds a reading of the stride wherever
    // the trace left its count.
    const _: () = assert!(BATCH_BOUND >= crate::cycle::arena::RECALL_STRIDE);
    let _g = test_guard();
    reset_lanes();
    let node = node_class("RecallLookupNode");
    let mut arena = Arena::new();
    let _ring = unsafe { crate::cycle::testing::long_ring(&mut arena, node, BATCH_BOUND) };
    unsafe { &*record() }.set_batch_size(BATCH_BOUND);

    testing::after_the_trace_of(1, recall_from_the_collector());
    testing::read_traced_batches(true);
    let served = served_by_a_collector();
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    unsafe { &*record() }.token.recall_for_test(false);
    assert_eq!(
        served,
        Served::Batch {
            roots: BATCH_BOUND,
            complete: false,
            backlog: false,
        },
        "the recall ended the batch"
    );
    assert_eq!(traced.len(), 1);
    assert_eq!(traced[0].parts, 1);
    assert!(
        traced[0].lookup_visits < BATCH_BOUND,
        "the lookup stopped short of its last root: {} visits",
        traced[0].lookup_visits
    );
    assert_eq!(
        verdict_count(),
        BATCH_BOUND,
        "every root has its one verdict"
    );
    unsafe { ll_gc_maybe_collect() };
    reset_lanes();
}

/// The lookup of a part's met roots walks the part's own rows where they are
/// fewer than the roots of the block: roots each a part of its own and all in
/// one block cost each part the group its row is in, and not the other roots
/// of the block (`dev/DECISIONS.md`, "A part's met roots are found by a walk
/// its own rows bound"). The walk over the
/// rows still finds a met root: over R = [k1 … k256, a1, a2], a2 inside a1's
/// closure, a1 and a2 built between the 128th kept root and the 129th so that
/// their block holds at least 128 roots, a2 is posted with a1's part.
#[test]
fn the_lookup_of_met_roots_is_bounded_by_the_parts_rows() {
    const ROOTS: usize = 256;
    let _g = test_guard();
    reset_lanes();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let node = node_class("DenseNode");
    let mut arena = Arena::new();
    let mut kept: Vec<(*mut RcHeader, *mut Object)> = (0..ROOTS / 2)
        .map(|index| unsafe { kept_root(&mut arena, node, &format!("DenseKeeper{index}")) })
        .collect();
    let a1 = unsafe { object(&mut arena, node) };
    let a2 = unsafe { object(&mut arena, node) };
    kept.extend(
        (ROOTS / 2..ROOTS)
            .map(|index| unsafe { kept_root(&mut arena, node, &format!("DenseKeeper{index}")) }),
    );
    // The walk over the rows is taken only where the block holds more roots
    // than eight times the groups a part met there, one or two for a1's part.
    let block_of = |address: usize| address & !(crate::memory::block_pool::BLOCK_SIZE - 1);
    for member in [a1, a2] {
        let roots_beside = kept
            .iter()
            .filter(|&&(root, _)| block_of(root as usize) == block_of(member as usize))
            .count();
        assert!(
            roots_beside > 2 * crate::cycle::shadow::GROUP as usize,
            "{roots_beside} roots share a block with the ring's member"
        );
    }
    let keepers: Vec<*mut Object> = kept.iter().map(|&(_, keeper)| keeper).collect();
    unsafe {
        crate::test_support::store_prop(&mut arena, a1, crate::test_support::prop_offset(0), a2);
        crate::test_support::store_prop(&mut arena, a2, crate::test_support::prop_offset(0), a1);
        assert!(!ll_release(a1 as *mut RcHeader), "a2's edge holds a1");
        assert!(!ll_release(a2 as *mut RcHeader), "a1's edge holds a2");
    }
    unsafe { &*record() }.set_batch_size(ROOTS + 2);

    testing::read_traced_batches(true);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: ROOTS + 2,
            complete: true,
            backlog: false,
        }
    );
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert_eq!(traced.len(), 1);
    assert_eq!(
        traced[0].parts,
        ROOTS + 1,
        "a1's part and every kept root's, and none for a2"
    );
    let posted = standing_verdicts();
    let at = posted
        .iter()
        .position(|&(root, _)| root == a1 as *mut RcHeader)
        .expect("a1 was posted");
    assert_eq!(
        posted[at..at + 2],
        [
            (a1 as *mut RcHeader, Verdict::Proposed),
            (a2 as *mut RcHeader, Verdict::Proposed),
        ],
        "a2 posted with a1's part"
    );
    assert!(
        traced[0].lookup_visits <= (ROOTS + 1) * crate::cycle::shadow::GROUP as usize,
        "{} visits, where every part walking the block's roots makes about {}",
        traced[0].lookup_visits,
        ROOTS * ROOTS / 2
    );

    assert_eq!(
        unsafe { ll_gc_maybe_collect() },
        2,
        "the ring was collected"
    );
    discard_standing_verdicts();
    for keeper in keepers {
        unsafe { release_keeper(keeper) };
    }
    reset_lanes();
}

/// The mutator's own claim, `MUTATOR` on the byte from its take through its
/// close, is what a collector's claim fails on: the collecting word is the
/// mutator's own gate and the collector never reads it.
#[test]
fn a_mutator_collecting_in_line_is_skipped() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("SkipNode");
    let mut arena = Arena::new();
    let (_, keeper) = unsafe { kept_root(&mut arena, node, "SkipKeeper") };

    let claim = crate::cycle::token::HeldToken::take();
    assert_eq!(served_by_a_collector(), Served::TokenHeld);
    assert_eq!(candidate_count(), 1, "nothing was taken");
    assert_eq!(verdict_count(), 0);
    drop(claim);
    assert_eq!(
        served_by_a_collector(),
        Served::Batch {
            roots: 1,
            complete: true,
            backlog: false,
        }
    );

    discard_standing_verdicts();
    unsafe { release_keeper(keeper) };
    reset_lanes();
}

#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_mutator_registering_throughout_the_batches_loses_no_root_and_doubles_none() {
    let _g = test_guard();
    reset_lanes();
    let node = node_class("ThroughoutNode");
    let mut arena = Arena::new();
    // Every object is built before the batches start, so the collector's
    // trace meets no header published under it; what the mutator does
    // during the batches is release, which registers.
    let count = 600;
    let objects: Vec<*mut Object> = (0..count)
        .map(|_| {
            let object = unsafe { object(&mut arena, node) };
            unsafe { ll_retain(object as *mut RcHeader) };
            object
        })
        .collect();
    let mut expected: Vec<*mut RcHeader> = objects.iter().map(|&o| o as *mut RcHeader).collect();
    expected.sort_unstable();

    // The first half is registered ahead, so the first batch has roots;
    // the batch waits between its last post and its advance, and the
    // second half registers while it stands peeked in R and posted in P.
    let half = count / 2;
    for &object in &objects[..half] {
        assert!(
            !unsafe { ll_release(object as *mut RcHeader) },
            "the case holds it"
        );
    }
    let (release, waiting_until) = std::sync::mpsc::channel::<()>();
    testing::make_the_next_batch_wait_before_its_advance(waiting_until);
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let (batched_tell, batched) = std::sync::mpsc::channel::<()>();
    let sent = Sent(record());
    // The collector waits for each consent as the thread does, on the slot's
    // wake word, so that neither side spins on the byte (`dev/WORKFLOW.md`,
    // Miri, "A test thread waits, it does not spin").
    let collector = std::thread::spawn(move || {
        assert!(crate::memory::heap::ll_thread_init());
        testing::stand_in_as_the_elder();
        let record = sent.into_inner();
        let mut batches = 0;
        loop {
            if let Served::Batch { .. } = unsafe { testing::serve_alone(record) } {
                batches += 1;
                batched_tell.send(()).expect("the case counts");
            }
            if stopped.try_recv().is_ok() {
                return batches;
            }
            std::thread::yield_now();
        }
    });

    // The batch stands waiting once its verdicts are in P; this thread
    // consents to the request meanwhile, as its poll would.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while verdict_count() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the first batch posted"
        );
        crate::cycle::token::read_and_act_on_this_thread();
        std::thread::yield_now();
    }
    let posted = verdict_count();
    assert!(posted >= 1 && posted <= INITIAL_BATCH);
    assert_eq!(
        candidate_count(),
        half,
        "R's front has not moved past the waiting batch"
    );
    for &object in &objects[half..] {
        assert!(
            !unsafe { ll_release(object as *mut RcHeader) },
            "the case holds it"
        );
    }
    assert_eq!(
        candidate_count(),
        count,
        "the second half landed behind the waiting batch"
    );
    release.send(()).expect("the batch is waiting");

    // The batches that follow take the rest, until R is empty, so that
    // every root the collector could take is taken. This thread consents
    // to each request and clears the `POSTED` each batch leaves, standing
    // in for the collections a mutator would run between them, which this
    // case makes by hand at its end.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut batches_seen = 0;
    loop {
        crate::cycle::token::read_and_act_on_this_thread();
        unsafe { &*record() }.clear_posted_for_test();
        while batched.try_recv().is_ok() {
            batches_seen += 1;
        }

        if batches_seen >= 2 && candidate_count() == 0 {
            break;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "the batches drained R: {batches_seen} so far, {} left",
            candidate_count()
        );
        std::thread::yield_now();
    }
    stop.send(()).expect("the collector is looping");
    let batches = collector.join().expect("the collector finished");
    assert!(batches >= 2, "the waiting batch and at least one after it");

    let mut standing = Vec::new();
    collect_lane_tokens(&mut standing);
    standing.sort_unstable();
    assert_eq!(
        standing, expected,
        "every registration once, across R and P, and none twice"
    );

    // The mutator's next collection defers what P holds and traces what R
    // holds live: every root ends in one lane once.
    assert!(refill_spares());
    assert_eq!(unsafe { crate::gc::ll_gc_collect_cycles() }, 0);
    let mut after = Vec::new();
    collect_lane_tokens(&mut after);
    after.sort_unstable();
    assert_eq!(after, expected, "and once after the collection");
    assert_eq!(verdict_count(), 0);

    for object in objects {
        unsafe {
            assert!(ll_release(object as *mut RcHeader));
            ll_object_die(object);
        }
    }
    reset_lanes();
}
