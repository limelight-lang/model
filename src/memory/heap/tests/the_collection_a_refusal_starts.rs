//! What an entity allocation does when the allocator below it has refused:
//! it collects this thread's cycles once and asks once more.
//!
//! Every case runs on a thread of its own, because what it needs is an entity
//! heap whose block for one size class it filled itself — the harness reuses
//! its threads, and a class an earlier case allocated in holds a block with
//! room that would serve the request these mean to refuse. Each case fills
//! every available slot before observing the pool cap.
//!
//! The injection is `block_pool::budget_blocks` rather than `FORCE_OOM`: a
//! budget refuses this thread only, and refuses after counting the request, so
//! a case can read both that the pool was asked and what a teardown gave back.
//! Which allocation the pool refused is read off `take_refused_entity_refills`,
//! by size class: the pool's count is blind to its requester, and a case that
//! names the refusal it forces proves it there (`dev/POSTMORTEM.md`, "an
//! allocation moved earlier re-aimed four refusal tests, and their counters
//! could not see it").

use super::*;

use crate::class::{Class, ClassBuilder};
use crate::cycle::collect::take_pressure_collections;
use crate::cycle::token::testing::HeldByACollector;
use crate::cycle::token::this_thread_token;
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader, budget_blocks, take_pool_requests};
use crate::object::Object;
use crate::refcount::{RcHeader, SlotState, slot_state};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A class whose instance fills one size class exactly, with `fillers`
/// properties behind the first deciding which class that is.
///
/// **Each case of this module passes a different count**, because a block is
/// adopted by size class and not by class: `Heap::alloc_no_block` takes an
/// abandoned block of the requested class before it asks the pool. Distinct
/// widths keep the two fixtures' occupancy readings independent.
///
/// Two properties are asserted. The size class is an exact fit, so how many
/// slots a block holds is arithmetic rather than a guess. And it is not the
/// widest class, which `the_allocation_itself` drains and reads the identity
/// of the slot that comes back from.
fn a_class_of_its_own(name: &str, fillers: usize) -> *const Class {
    a_class_of_its_own_destroyed_by(name, fillers, None)
}

/// [`a_class_of_its_own`] with a destructor, for the case whose dying object
/// stands in the class it fills.
fn a_class_of_its_own_destroyed_by(
    name: &str,
    fillers: usize,
    destructor: Option<*const ()>,
) -> *const Class {
    let mut builder = ClassBuilder::new(name).prop("child", true);
    let names: Vec<String> = (0..fillers).map(|i| format!("f{i}")).collect();
    for filler in &names {
        builder = builder.prop(filler, true);
    }

    if let Some(destructor) = destructor {
        builder = builder.destructor(destructor);
    }

    let class = builder.build();
    let size = unsafe { (*class).object_size } as usize;
    let ci = size_class_index(size).expect("a size class serves this instance");
    assert_eq!(
        SIZE_CLASSES[ci], size,
        "the instance fits its size class exactly, so a block's slot count is arithmetic"
    );
    assert!(
        ci < SIZE_CLASSES.len() - 1,
        "and it is not the widest class, which another case drains and reads addresses from"
    );
    class
}

/// How many slots of `class`'s size one block holds.
fn slots_per_block(class: *const Class) -> usize {
    BLOCK_PAYLOAD / SIZE_CLASSES[class_index(class)]
}

/// The size class `class`'s instances are served from.
fn class_index(class: *const Class) -> usize {
    let size = unsafe { (*class).object_size } as usize;
    size_class_index(size).expect("a size class serves this instance")
}

/// Refused entity refills since the last reading, as the count at `class`'s
/// size class and the count at every other, so that a case asserts both that
/// its allocation was refused and that no other was.
fn refused_refills_at(class: *const Class) -> (usize, usize) {
    let refused = take_refused_entity_refills();
    let at_class = refused[class_index(class)];
    (at_class, refused.iter().sum::<usize>() - at_class)
}

/// The block `slot` stands in, as an address.
fn block_of(slot: *const u8) -> usize {
    BlockHeader::of_ptr(slot) as usize
}

/// Live occupants of the block `entity` stands in, which is what falls when a
/// slot is returned and stands when one is withheld
/// (`dev/DECISIONS.md`, "A block's `used` falls at the slot's return").
///
/// # Safety
/// `entity` addresses a slot of a commissioned entity block.
unsafe fn occupants_beside(entity: *mut RcHeader) -> u32 {
    let block = BlockHeader::of_ptr(entity as *const u8) as *mut HeapBlockHeader;
    unsafe { (*block).private.used }
}

/// Take slots of `size` until one is refused, and answer the slots taken.
///
/// **Allocating rather than counting is what makes a case independent of the
/// heap it starts from.** The blocks a class can be served from are its current
/// one and any the process abandoned at that size — an earlier case's thread
/// leaves such a block behind, and `Heap::adopt` takes it before the pool is
/// asked at all. Taking slots until the refusal empties whatever is there, so
/// the refusal a case reads is the pool's.
///
/// `bound` is how many slots the caller expects at most; past it the budget is
/// refusing nothing and the case has no refusal to read. The slots taken are
/// given back before that is reported, because a panic holding them would
/// abandon their blocks with live occupants and every later case of this class
/// would adopt them one at a time.
///
/// The slots carry no header: nothing is published into them, and they go back
/// through `free_unpublished`, which is the return an unpublished cell owes.
///
/// # Safety
/// Runs on a thread the runtime has started, under a budgeted pool.
unsafe fn take_slots_until_refused(size: usize, bound: usize) -> Vec<*mut u8> {
    let mut taken = Vec::new();
    loop {
        let slot = unsafe { entity_alloc(size) };
        if slot.is_null() {
            return taken;
        }

        taken.push(slot);
        if taken.len() > bound {
            unsafe { give_back(&taken) };
            panic!("the budgeted pool refused nothing, so this case reads no refusal");
        }
    }
}

/// Give back what [`take_slots_until_refused`] took.
///
/// # Safety
/// Every slot came from that call and none was published.
unsafe fn give_back(slots: &[*mut u8]) {
    for &slot in slots {
        unsafe { crate::memory::stdapi::free_unpublished(slot) };
    }
}

/// The wiring, read by what it asks of the pool: one collection for the
/// request, and two requests — the attempt that found no room, and the retry
/// the collection paid for.
#[test]
fn a_refusal_starts_one_collection_and_asks_once_more() {
    let _g = crate::memory::block_pool::test_guard();

    let (collections, requests, refused, armed) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        // The collection workspace is a block, drawn at a thread's first
        // collection: taken here rather than inside the budgeted window, where
        // it would be the refusal the case reads
        // (`cycle::queue::warm_workspace_base`).
        crate::cycle::queue::warm_workspace_base();

        let class = a_class_of_its_own("RefusalWiring", 254);
        let size = unsafe { (*class).object_size } as usize;
        let bound = 4 * slots_per_block(class);
        let mut arena = Arena::new();
        let ring = unsafe { crate::cycle::testing::ring(&mut arena, [class, class, class]) };
        crate::gc::disarm();

        let _budgeted = budget_blocks(0);
        let _ = take_pool_requests();
        let _ = take_pressure_collections();
        let _ = take_refused_entity_refills();
        let taken = unsafe { take_slots_until_refused(size, bound) };
        let answer = (
            take_pressure_collections(),
            take_pool_requests(),
            refused_refills_at(class),
            crate::gc::is_armed(),
        );

        drop(_budgeted);
        unsafe { give_back(&taken) };
        let _ = ring;
        answer
    })
    .join()
    .unwrap();

    assert_eq!(
        collections, 2,
        "the served retry and the final exhausted request each collect once"
    );
    assert_eq!(
        requests, 3,
        "one refused pool request before the served retry, then two at final exhaustion"
    );
    assert_eq!(
        refused,
        (3, 0),
        "and every one of the three was this class's entity refill: the collection's own \
         allocations were served"
    );
    assert!(
        !armed,
        "the round traced every root and freed, so it hands nothing to the poll"
    );
}

/// The control arm: with nothing to collect, the shape of the path is the same
/// — one collection, one retry — and the caller is answered the refusal it
/// raises memory-exhausted on (`rfc/runtime/exceptions.md`, "Allocation failure
/// is an ordinary exception").
///
/// It differs from the case above by the ring and by nothing else, so what the
/// pair reads is that neither the collection nor the retry is conditional on
/// there being garbage.
#[test]
fn a_refusal_with_nothing_to_collect_asks_once() {
    let _g = crate::memory::block_pool::test_guard();

    let (collections, requests) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();

        let class = a_class_of_its_own("RefusalNoGarbage", 126);
        let size = unsafe { (*class).object_size } as usize;
        let bound = 4 * slots_per_block(class);
        crate::gc::disarm();

        let _budgeted = budget_blocks(0);
        let _ = take_pool_requests();
        let _ = take_pressure_collections();
        let taken = unsafe { take_slots_until_refused(size, bound) };
        let answer = (take_pressure_collections(), take_pool_requests());

        drop(_budgeted);
        unsafe { give_back(&taken) };
        answer
    })
    .join()
    .unwrap();

    assert_eq!(collections, 1, "the refusal collects once here too");
    assert_eq!(
        requests, 2,
        "and asks twice: the retry is unconditional, because what a collection freed is not \
         what the allocator can use"
    );
}

/// The retry can reuse all three member slots under a pool cap: retirement
/// lowers occupancy and a later allocation returns each original address.
#[test]
fn a_freed_members_slot_serves_the_retry_under_the_pool_cap() {
    let _g = crate::memory::block_pool::test_guard();

    let (states, expected_occupants, occupants, reused) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();

        let class = a_class_of_its_own("RefusalWithholding", 62);
        let size = unsafe { (*class).object_size } as usize;
        let bound = 4 * slots_per_block(class);
        let mut arena = Arena::new();
        let ring = unsafe { crate::cycle::testing::ring(&mut arena, [class, class, class]) };
        let ring_block = block_of(ring[0] as *const u8);
        // Read before the loop, because the block need not have started empty:
        // an adopted one carries whatever its last thread left in it.
        let occupants_before = unsafe { occupants_beside(ring[0] as *mut RcHeader) } as usize;

        let _budgeted = budget_blocks(0);
        let taken = unsafe { take_slots_until_refused(size, bound) };

        let states: Vec<SlotState> = ring
            .iter()
            .map(|&member| unsafe { slot_state(member as *mut RcHeader) })
            .collect();
        // The block's own count, which is the statement the state cannot make:
        // `DeadInPlace` is also what a slot on its block's free list reads,
        // while `used` falls at a return and stands at a withholding. The loop
        // took whatever that block had free, so the count rises by exactly the
        // slots taken out of it when the three members are still occupants,
        // and by three fewer when they came back.
        let occupants = unsafe { occupants_beside(ring[0] as *mut RcHeader) } as usize;
        let taken_here = taken
            .iter()
            .filter(|&&slot| block_of(slot as *const u8) == ring_block)
            .count();
        // And the addresses, for the same return read as a slot rather than as
        // a count: a retry served off a member's slot hands out one of these
        // three.
        let members: Vec<usize> = ring.iter().map(|&member| member as usize).collect();
        let reused = taken
            .iter()
            .filter(|&&slot| members.contains(&(slot as usize)))
            .count();

        drop(_budgeted);
        unsafe { give_back(&taken) };
        (states, occupants_before + taken_here - 3, occupants, reused)
    })
    .join()
    .unwrap();

    assert_eq!(
        states,
        vec![SlotState::DeadInPlace; 3],
        "every member is torn down and its slot taken"
    );
    assert_eq!(
        occupants, expected_occupants,
        "retirement returned all three member slots before their reuse"
    );
    assert_eq!(
        reused, 3,
        "the cap forces all three member slots to serve later requests"
    );
}

/// The slow path waits on this thread's own token while a collector holds it,
/// and the collection it then runs serves the retry. Whether it waited is read
/// off the token's count of waits, and the holder lets go only once that count
/// has moved: a case that only terminates terminates most easily when the
/// wait is never taken.
#[test]
fn a_refusal_under_a_held_token_waits_for_the_release_and_is_then_served() {
    let _g = crate::memory::block_pool::test_guard();

    let (waited, collections, reused) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();

        let class = a_class_of_its_own("RefusalHeldToken", 30);
        let size = unsafe { (*class).object_size } as usize;
        let bound = 4 * slots_per_block(class);
        let mut arena = Arena::new();
        let ring = unsafe { crate::cycle::testing::ring(&mut arena, [class, class, class]) };
        crate::gc::disarm();

        let waits_before = unsafe { (*this_thread_token()).waits() };
        let _budgeted = budget_blocks(0);
        let _ = take_pressure_collections();
        // Released by the holder itself, once the count says this thread is
        // waiting; the guard joins it on the way out either way.
        let _held = HeldByACollector::take(this_thread_token(), true);
        let taken = unsafe { take_slots_until_refused(size, bound) };

        let members: Vec<usize> = ring.iter().map(|&member| member as usize).collect();
        let reused = taken
            .iter()
            .filter(|&&slot| members.contains(&(slot as usize)))
            .count();
        let answer = (
            unsafe { (*this_thread_token()).waits() } - waits_before,
            take_pressure_collections(),
            reused,
        );

        drop(_budgeted);
        unsafe { give_back(&taken) };
        answer
    })
    .join()
    .unwrap();

    assert_ne!(
        waited, 0,
        "the collection the refusal started went to wait on the held token"
    );
    assert_eq!(
        collections, 2,
        "and ran once released, the final exhaustion collecting a second time"
    );
    assert_eq!(reused, 3, "the ring it freed is what served the retry");
}

/// What `allocating_destructor` was answered, summed over its runs since
/// `clear_destructor_readings`: runs, runs answered null, pressure
/// collections opened inside a run, refills refused at the filler's class
/// and at every other, and the address the last run was answered. Sums in
/// atomics rather than a `Mutex`-held list, because the destructor runs
/// inside a collection.
static DESTRUCTOR_READINGS: AtomicUsize = AtomicUsize::new(0);
static DESTRUCTOR_NULLS: AtomicUsize = AtomicUsize::new(0);
static DESTRUCTOR_COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
static DESTRUCTOR_REFUSED_HERE: AtomicUsize = AtomicUsize::new(0);
static DESTRUCTOR_REFUSED_ELSEWHERE: AtomicUsize = AtomicUsize::new(0);
static DESTRUCTOR_SLOT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// The filler class the destructor below allocates in, set by the case
    /// on its own thread before the collection runs.
    static FILLER: std::cell::Cell<*const Class> = const { std::cell::Cell::new(std::ptr::null()) };
}

/// A destructor that asks for a slot of the filler's class, which the case
/// has filled under a budget of zero, and records what it was answered.
unsafe extern "C" fn allocating_destructor(_object: *mut Object) {
    let filler = FILLER.with(std::cell::Cell::get);
    let size = unsafe { (*filler).object_size } as usize;
    let _ = take_pressure_collections();
    let _ = take_refused_entity_refills();
    let slot = unsafe { entity_alloc(size) };
    let (here, elsewhere) = refused_refills_at(filler);
    DESTRUCTOR_SLOT.store(slot as usize, Ordering::Relaxed);
    DESTRUCTOR_READINGS.fetch_add(1, Ordering::Relaxed);
    DESTRUCTOR_NULLS.fetch_add(slot.is_null() as usize, Ordering::Relaxed);
    DESTRUCTOR_COLLECTIONS.fetch_add(take_pressure_collections(), Ordering::Relaxed);
    DESTRUCTOR_REFUSED_HERE.fetch_add(here, Ordering::Relaxed);
    DESTRUCTOR_REFUSED_ELSEWHERE.fetch_add(elsewhere, Ordering::Relaxed);
    if !slot.is_null() {
        unsafe { give_back(&[slot]) };
    }
}

/// Zero the destructor's readings, under the guard that serialises the
/// cases that take them.
fn clear_destructor_readings() {
    for reading in [
        &DESTRUCTOR_READINGS,
        &DESTRUCTOR_NULLS,
        &DESTRUCTOR_COLLECTIONS,
        &DESTRUCTOR_REFUSED_HERE,
        &DESTRUCTOR_REFUSED_ELSEWHERE,
        &DESTRUCTOR_SLOT,
    ] {
        reading.store(0, Ordering::Relaxed);
    }
}

/// A shortage inside a destructor of a collection already running collects
/// nothing and reports: the gate refuses the collection — on the collecting
/// flag, the collection's destructor pass running no teardown of its own —
/// the retry is asked all the same, and the destructor is answered null.
///
/// The filler's class is filled under the budget before the ring is built and
/// the budget is lifted for the build, because filling it is itself a refusal
/// and the collection that refusal starts would take the ring.
#[test]
fn a_shortage_inside_a_destructor_collects_nothing_and_reports() {
    let _g = crate::memory::block_pool::test_guard();
    clear_destructor_readings();

    let freed = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();
        crate::gc::disarm();

        let filler = a_class_of_its_own("RefusalDepthFiller", 446);
        FILLER.with(|cell| cell.set(filler));
        let size = unsafe { (*filler).object_size } as usize;
        let bound = 4 * slots_per_block(filler);
        let budgeted = budget_blocks(0);
        let taken = unsafe { take_slots_until_refused(size, bound) };
        drop(budgeted);

        let dying = ClassBuilder::new("RefusalDepthNode")
            .prop("next", true)
            .destructor(allocating_destructor as *const ())
            .build();
        let mut arena = Arena::new();
        let _ring = unsafe { crate::cycle::testing::ring(&mut arena, [dying, dying]) };

        let _budgeted = budget_blocks(0);
        let freed = unsafe { crate::gc::ll_gc_collect_cycles() };

        drop(_budgeted);
        unsafe { give_back(&taken) };
        freed
    })
    .join()
    .unwrap();

    assert_eq!(freed, 2, "the outer collection frees its ring");
    assert_eq!(
        DESTRUCTOR_READINGS.load(Ordering::Relaxed),
        2,
        "both destructors ran"
    );
    assert_eq!(
        DESTRUCTOR_NULLS.load(Ordering::Relaxed),
        2,
        "and each was answered null"
    );
    assert_eq!(
        DESTRUCTOR_COLLECTIONS.load(Ordering::Relaxed),
        0,
        "with no collection opened inside the one running"
    );
    assert_eq!(
        (
            DESTRUCTOR_REFUSED_HERE.load(Ordering::Relaxed),
            DESTRUCTOR_REFUSED_ELSEWHERE.load(Ordering::Relaxed)
        ),
        (4, 0),
        "each refusal being the filler's refill, asked twice: the attempt and the retry"
    );
}

/// A size class whose block holds nothing but cyclic garbage serves a request
/// for every one of its slots, with no collection asked for by name: the
/// first refusal collects the rings, and the retry and every request after it
/// are served off their slots.
///
/// The block is filled by taking every free slot under the budget, giving
/// them back, and building rings out of that room; what the rings leave over
/// is taken again as unpublished fillers, so the rings are the only garbage
/// in the class and the count of slots served is the count of their members.
#[test]
fn a_class_full_of_cyclic_garbage_serves_a_request_for_each_member() {
    let _g = crate::memory::block_pool::test_guard();

    let (members, served, collections, refused, armed) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();
        crate::gc::disarm();

        let class = a_class_of_its_own("RefusalFullOfRings", 222);
        let size = unsafe { (*class).object_size } as usize;
        let bound = 4 * slots_per_block(class);
        // One slot before the budget, so that the class has a block at all:
        // a class this thread never allocated in holds nothing to fill.
        let first = unsafe { entity_alloc(size) };
        assert!(!first.is_null(), "the pool served the class's first block");
        let _budgeted = budget_blocks(0);
        let mut room = unsafe { take_slots_until_refused(size, bound) };
        room.push(first);
        let free_slots = room.len();
        unsafe { give_back(&room) };

        let rings = free_slots / 3;
        let mut arena = Arena::new();
        for _ in 0..rings {
            let _ = unsafe { crate::cycle::testing::ring(&mut arena, [class, class, class]) };
        }

        let leftover: Vec<*mut u8> = (0..free_slots % 3)
            .map(|_| {
                let slot = unsafe { entity_alloc(size) };
                assert!(!slot.is_null(), "the room the rings left over is served");
                slot
            })
            .collect();

        let _ = take_pressure_collections();
        let _ = take_refused_entity_refills();
        let served = unsafe { take_slots_until_refused(size, bound) };
        let answer = (
            3 * rings,
            served.len(),
            take_pressure_collections(),
            refused_refills_at(class),
            crate::gc::is_armed(),
        );

        drop(_budgeted);
        unsafe { give_back(&served) };
        unsafe { give_back(&leftover) };
        answer
    })
    .join()
    .unwrap();

    assert!(members > 0, "the class held at least one ring");
    assert_eq!(
        served, members,
        "one slot per ring member was served before the class was exhausted"
    );
    assert_eq!(
        collections, 2,
        "the first refusal collected the rings and the exhaustion collected once more"
    );
    assert_eq!(
        refused,
        (3, 0),
        "three refusals of this class's refill: one served by the rings, two at exhaustion"
    );
    assert!(
        !armed,
        "every round traced every root, so nothing is handed to the poll"
    );
}

/// The same shortage at teardown depth one with no collection running: an
/// ordinary release's destructor. The teardown alone closes the gate, so the
/// destructor is answered null with no collection opened; the refusal arms the
/// thread, and the ring that is garbage meanwhile is collected by the poll at
/// the next clean point.
#[test]
fn a_shortage_inside_an_ordinary_teardown_collects_nothing_and_reports() {
    let _g = crate::memory::block_pool::test_guard();
    clear_destructor_readings();

    let collected_after = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();
        crate::gc::disarm();

        let filler = a_class_of_its_own("RefusalTeardownFiller", 382);
        FILLER.with(|cell| cell.set(filler));
        let size = unsafe { (*filler).object_size } as usize;
        let bound = 4 * slots_per_block(filler);
        let budgeted = budget_blocks(0);
        let taken = unsafe { take_slots_until_refused(size, bound) };
        drop(budgeted);

        let dying = ClassBuilder::new("RefusalTeardownNode")
            .prop("next", true)
            .destructor(allocating_destructor as *const ())
            .build();
        let garbage = ClassBuilder::new("RefusalTeardownGarbage")
            .prop("next", true)
            .build();
        let mut arena = Arena::new();
        let _ring = unsafe { crate::cycle::testing::ring(&mut arena, [garbage, garbage]) };
        let mut context = crate::memory::context::LLContext { arena: &mut arena };
        let object = unsafe {
            crate::object::new_constructed(
                &mut context,
                dying,
                crate::refcount::MemoryCategory::GcHeap,
            )
        };

        let _budgeted = budget_blocks(0);
        assert!(
            !crate::gc::is_armed(),
            "nothing before the death armed the thread"
        );
        // The verdict and the death, as the compiler emits them: the release
        // answers and the caller runs the teardown.
        let died = unsafe { crate::refcount::ll_release(object as *mut RcHeader) };
        assert!(died, "the release was the last reference");
        unsafe { crate::object::ll_object_die(object) };
        // The poll and not the explicit fire: what it reads is that the
        // refusal armed the thread, since nothing else in this case does.
        let collected_after = unsafe { crate::gc::ll_gc_maybe_collect() };

        drop(_budgeted);
        unsafe { give_back(&taken) };
        collected_after
    })
    .join()
    .unwrap();

    assert_eq!(
        DESTRUCTOR_READINGS.load(Ordering::Relaxed),
        1,
        "the destructor ran"
    );
    assert_eq!(
        DESTRUCTOR_NULLS.load(Ordering::Relaxed),
        1,
        "and was answered null"
    );
    assert_eq!(
        DESTRUCTOR_COLLECTIONS.load(Ordering::Relaxed),
        0,
        "with no collection opened inside the teardown"
    );
    assert_eq!(
        (
            DESTRUCTOR_REFUSED_HERE.load(Ordering::Relaxed),
            DESTRUCTOR_REFUSED_ELSEWHERE.load(Ordering::Relaxed)
        ),
        (2, 0),
        "the refusal being the filler's refill, asked twice"
    );
    assert_eq!(
        collected_after, 2,
        "and the poll at the clean point after it collects the ring the refusal armed it for"
    );
}

/// A refusal at teardown depth one still returns what a completed death
/// withheld: the slot of a candidate that died before this teardown stands
/// withheld until a retirement, every retirement runs inside a collection, and
/// the gate refuses the collection — so the refused branch retires on its own,
/// and the retry is served off that slot with no collection opened.
///
/// The withheld slot is made after the fill and before the budget, because
/// the fill's own refusal collects, and that collection would retire it.
#[test]
fn a_refusal_inside_a_teardown_retires_the_completed_deaths_and_is_served() {
    let _g = crate::memory::block_pool::test_guard();
    clear_destructor_readings();

    let (withheld, armed) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();
        crate::gc::disarm();

        let filler = a_class_of_its_own("RefusalRetiringFiller", 158);
        FILLER.with(|cell| cell.set(filler));
        let size = unsafe { (*filler).object_size } as usize;
        let bound = 4 * slots_per_block(filler);
        let mut arena = Arena::new();
        let mut context = crate::memory::context::LLContext { arena: &mut arena };
        // A registered candidate of the filler's class, alive through the
        // fill: a retain and a release are the non-zero decrement that
        // registers it.
        let candidate = unsafe {
            crate::object::new_constructed(
                &mut context,
                filler,
                crate::refcount::MemoryCategory::GcHeap,
            )
        };
        unsafe {
            crate::refcount::ll_retain(candidate as *mut RcHeader);
            assert!(!crate::refcount::ll_release(candidate as *mut RcHeader));
        }
        let dying = ClassBuilder::new("RefusalRetiringNode")
            .prop("next", true)
            .destructor(allocating_destructor as *const ())
            .build();
        let object = unsafe {
            crate::object::new_constructed(
                &mut context,
                dying,
                crate::refcount::MemoryCategory::GcHeap,
            )
        };

        let budgeted = budget_blocks(0);
        let taken = unsafe { take_slots_until_refused(size, bound) };
        drop(budgeted);

        // The candidate dies now, with the class full: its slot is withheld,
        // the queue entry naming it.
        assert!(unsafe { crate::refcount::ll_release(candidate as *mut RcHeader) });
        unsafe { crate::object::ll_object_die(candidate as *mut Object) };
        assert_eq!(
            unsafe { slot_state(candidate as *mut RcHeader) },
            SlotState::DeadInPlace,
            "the candidate's slot is withheld, not on the free list"
        );

        let _budgeted = budget_blocks(0);
        assert!(
            !crate::gc::is_armed(),
            "nothing before the death armed the thread"
        );
        assert!(unsafe { crate::refcount::ll_release(object as *mut RcHeader) });
        unsafe { crate::object::ll_object_die(object) };
        let armed = crate::gc::is_armed();

        drop(_budgeted);
        unsafe { give_back(&taken) };
        (candidate as usize, armed)
    })
    .join()
    .unwrap();

    assert_eq!(
        DESTRUCTOR_READINGS.load(Ordering::Relaxed),
        1,
        "the destructor ran"
    );
    assert_eq!(
        DESTRUCTOR_NULLS.load(Ordering::Relaxed),
        0,
        "and was served"
    );
    assert_eq!(
        DESTRUCTOR_SLOT.load(Ordering::Relaxed),
        withheld,
        "off the slot the completed death had withheld"
    );
    assert_eq!(
        DESTRUCTOR_COLLECTIONS.load(Ordering::Relaxed),
        0,
        "with no collection opened inside the teardown"
    );
    assert_eq!(
        (
            DESTRUCTOR_REFUSED_HERE.load(Ordering::Relaxed),
            DESTRUCTOR_REFUSED_ELSEWHERE.load(Ordering::Relaxed)
        ),
        (1, 0),
        "one refusal of the filler's refill, the retry served without one"
    );
    assert!(armed, "and the refusal armed the thread");
}

/// The retirement a depth refusal runs passes over a dying object a queue
/// entry names: at teardown depth two — a child's destructor inside its
/// holder's `dispose` — the holder stands at count zero with no
/// `DEAD_IN_PLACE` until the free at the end of its own frame, so it is kept
/// as an unfinished death and that free is the first. A retirement that
/// returned it would make the free a second one, which `ll_free` refuses and
/// the crate aborts on.
///
/// The holder is of the filler's class, so that its own slot is the one a
/// wrong retirement would hand the child's destructor.
#[test]
fn a_refusal_inside_a_teardown_keeps_the_dying_candidate_registered() {
    let _g = crate::memory::block_pool::test_guard();
    clear_destructor_readings();

    let (holder_address, state_after) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();
        crate::gc::disarm();

        let filler = a_class_of_its_own("RefusalDyingCandidateFiller", 94);
        FILLER.with(|cell| cell.set(filler));
        let size = unsafe { (*filler).object_size } as usize;
        let bound = 4 * slots_per_block(filler);
        let child_class = ClassBuilder::new("RefusalDyingCandidateChild")
            .prop("next", true)
            .destructor(allocating_destructor as *const ())
            .build();
        let mut arena = Arena::new();
        let mut context = crate::memory::context::LLContext { arena: &mut arena };
        let holder = unsafe {
            crate::object::new_constructed(
                &mut context,
                filler,
                crate::refcount::MemoryCategory::GcHeap,
            )
        };
        let child = unsafe {
            crate::object::new_constructed(
                &mut context,
                child_class,
                crate::refcount::MemoryCategory::GcHeap,
            )
        };
        // The holder's slot is the child's only reference once the creation
        // reference is spent, and the holder is registered by a retain and a
        // release — the non-zero decrement.
        unsafe {
            crate::test_support::store_prop(
                &mut arena,
                holder,
                crate::test_support::prop_offset(0),
                child,
            );
            assert!(!crate::refcount::ll_release(child as *mut RcHeader));
            crate::refcount::ll_retain(holder as *mut RcHeader);
            assert!(!crate::refcount::ll_release(holder as *mut RcHeader));
        }

        let budgeted = budget_blocks(0);
        let taken = unsafe { take_slots_until_refused(size, bound) };
        drop(budgeted);

        let _budgeted = budget_blocks(0);
        assert!(unsafe { crate::refcount::ll_release(holder as *mut RcHeader) });
        unsafe { crate::object::ll_object_die(holder) };
        let state_after = unsafe { slot_state(holder as *mut RcHeader) };

        drop(_budgeted);
        unsafe { give_back(&taken) };
        (holder as usize, state_after)
    })
    .join()
    .unwrap();

    assert_eq!(
        DESTRUCTOR_READINGS.load(Ordering::Relaxed),
        1,
        "the child's destructor ran"
    );
    assert_eq!(
        DESTRUCTOR_NULLS.load(Ordering::Relaxed),
        1,
        "and was answered null: the one zero-count slot of the class is its holder's, unfinished"
    );
    assert_ne!(
        DESTRUCTOR_SLOT.load(Ordering::Relaxed),
        holder_address,
        "so the retirement did not hand the destructor its holder's slot"
    );
    assert_eq!(
        state_after,
        SlotState::DeadInPlace,
        "and the holder's own free took the slot once"
    );
}

/// A destructor that asks for the thread's exit, from inside the collection
/// an allocation refusal started.
unsafe extern "C" fn exit_requesting_destructor(_object: *mut Object) {
    crate::memory::heap::ll_thread_exit();
}

/// A destructor of the collection the refusal started asks for the thread's
/// exit: the request waits for the thread's top, the retry is served off the
/// ring the collection freed, and the loop goes on to a second refusal on a
/// live heap (`dev/DECISIONS.md`, "an exit requested inside a collection runs
/// at the thread's top").
#[test]
fn a_refusal_whose_destructor_asks_for_the_exit_is_still_served() {
    let _g = crate::memory::block_pool::test_guard();

    let (collections, heap_alive, pending) = std::thread::spawn(|| {
        assert!(ll_thread_init(), "the pool served this thread");
        crate::cycle::queue::warm_workspace_base();

        let class = a_class_of_its_own_destroyed_by(
            "RefusalExitRequesting",
            94,
            Some(exit_requesting_destructor as *const ()),
        );
        let size = unsafe { (*class).object_size } as usize;
        let bound = 4 * slots_per_block(class);
        let mut arena = Arena::new();
        let _ring = unsafe { crate::cycle::testing::ring(&mut arena, [class, class, class]) };
        drop(arena);
        crate::gc::disarm();

        let _budgeted = budget_blocks(0);
        let _ = take_pressure_collections();
        let taken = unsafe { take_slots_until_refused(size, bound) };
        let answer = (
            take_pressure_collections(),
            !crate::memory::heap::thread_entity_heap().is_null(),
            crate::memory::heap::thread_exit_pending(),
        );

        drop(_budgeted);
        unsafe { give_back(&taken) };
        answer
    })
    .join()
    .unwrap();

    assert_eq!(
        collections, 2,
        "the first refusal collected the ring and its retry was served; the second found nothing"
    );
    assert!(heap_alive, "the thread kept its heap");
    assert!(pending, "with the exit waiting for the top");
}
