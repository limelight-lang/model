//! What a collection asks the global allocator between an entry and its last
//! free, which the GC-memory contract answers with "nothing": every byte a
//! collection holds comes through the memory manager, and a counting allocator
//! standing under the whole crate is what says so (`dev/DECISIONS.md`, "GC
//! memory is counted once, and the block kind is the split").
//!
//! **One site is exempt and it is a debug build's alone.**
//! `cycle::validation`'s two `debug_assert!`s materialise the membership as a
//! sorted list and index an in-degree array by it, three allocations a release
//! build makes none of. Each case here subtracts that figure rather than
//! asserting a bare zero, and reads the number of validations off the premise
//! counter instead of deriving it from the finalization protocol
//! (`crate::cycle::validation::EXEMPT_ALLOCATIONS_PER_VALIDATION`, calibrated
//! by `validation::tests::what_the_premise_check_costs`).
//!
//! **Frees are counted beside allocations**, because the source audit reserved
//! the sites that give memory back rather than take it, and a counting
//! allocator reads a `free` as nothing on the allocation axis alone.
//!
//! **A zero is admitted only where the same entry over the same graph answers
//! a teardown.** An arm whose collection refused would allocate nothing by
//! doing nothing, so each case names what it collected: the answer, the
//! destructor count, the slot states, and the marked slots the close popped.

use super::*;
use crate::cycle::deferred_slot_reuse::take_slots_popped;
use crate::cycle::validation::{EXEMPT_ALLOCATIONS_PER_VALIDATION, premise_cell_walks};
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::test_support::allocation_probe;
use crate::value::{Tag, Value};

/// What `cycle::validation`'s two debug checks allocated inside a bracket.
///
/// `walks` is the premise counter's rise across the bracket and `members` the
/// membership of the one commit inside it. One `validate_component` walks every
/// member's cells once, so their quotient is how many validations the commit
/// ran — read off the instrument, and held against `validations`, which is
/// what the protocol owes the case's component: one for a component with no
/// destructor and two for one with. Without that check a commit that skipped
/// its second reading would lower the exemption and the count together, and
/// the case would pass over an unvalidated teardown. A release build runs
/// neither check, records no walk, and this answers zero.
fn exempt_allocations(walks: usize, members: usize, validations: usize) -> usize {
    assert_eq!(
        walks % members,
        0,
        "the walks of a bracket holding one commit over `members` members are a \
         multiple of it; what pins the membership itself is the caller's reading of \
         what the collection freed"
    );
    if cfg!(debug_assertions) {
        assert_eq!(
            walks / members,
            validations,
            "the commit ran the validations its component's destructors call for"
        );
    }

    (walks / members) * EXEMPT_ALLOCATIONS_PER_VALIDATION
}

/// The validations a confirmed component with a destructor goes through: the
/// exact test, and the second reading after the destructor pass.
const VALIDATIONS_WITH_A_DESTRUCTOR: usize = 2;

/// A class of `properties` Box properties whose instance fits one size class
/// exactly, with a destructor and with the ring's edge at `prop_offset(0)`.
///
/// Exact sizing makes block occupancy predictable. Separate widths keep these
/// fixtures independent of one another; completed registered slots return at
/// the mutator's final reading, after every membership reader has finished.
fn a_class_of_its_own(name: &str, properties: usize, destructor: *const ()) -> *const Class {
    let mut builder = ClassBuilder::new(name).prop("next", true);
    let fillers: Vec<String> = (1..properties).map(|index| format!("f{index}")).collect();
    for filler in &fillers {
        builder = builder.prop(filler, true);
    }

    // A null pointer is no destructor rather than one that is called:
    // `ClassBuilder` registers whatever it is handed.
    if !destructor.is_null() {
        builder = builder.destructor(destructor);
    }

    let class = builder.build();
    let size = unsafe { (*class).object_size } as usize;
    let index = crate::memory::heap::size_class_index(size).expect("a size class serves it");
    assert_eq!(
        crate::memory::heap::SIZE_CLASSES[index],
        size,
        "the instance fits its size class exactly, so it rounds up into nobody else's block"
    );
    class
}

/// A class whose instance is wider than a block's payload, so its body is an
/// OS-direct run rather than a slot of a block.
///
/// The property count is derived from the payload and the property stride
/// rather than written down: a block size that moves would otherwise leave the
/// case building a pooled large entity and reading the registry that never
/// held it.
fn os_direct_class(name: &str) -> *const Class {
    let stride = (prop_offset(1) - prop_offset(0)) as usize;
    let mut builder = ClassBuilder::new(name).prop("first", true);
    let fillers: Vec<String> = (0..BLOCK_PAYLOAD / stride)
        .map(|index| format!("f{index}"))
        .collect();
    for filler in &fillers {
        builder = builder.prop(filler, true);
    }

    let class = builder.build();
    assert!(
        unsafe { (*class).object_size } as usize > BLOCK_PAYLOAD,
        "the instance passes a block's payload, which is what makes its body OS-direct"
    );
    class
}

/// The block an entity's body stands in, as an address.
fn block_of(entity: *mut Object) -> usize {
    BlockHeader::of_ptr(entity as *const u8) as usize
}

#[test]
fn registered_large_candidates_return_their_mappings_at_mutator_retirement() {
    let _g = test_guard();
    crate::cycle::queue::release_queue_segments();
    let class = os_direct_class("RetiredLargeRing");
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [class; 2]) };
    let runs = members.map(block_of);
    assert!(
        runs.iter()
            .all(|&run| crate::memory::large_entity::holds_run(run))
    );
    assert_eq!(unsafe { ll_gc_collect_cycles() }, 2);
    assert!(
        runs.iter()
            .all(|&run| !crate::memory::large_entity::holds_run(run))
    );
    assert_eq!(crate::cycle::queue::candidate_count(), 0);
}

/// The ordinary path, and with it the frames a large body's death enters: a
/// garbage ring holding one child whose body is an OS-direct run.
///
/// The child takes its holder's edge as its creation reference, so no decrement
/// ever reached the candidate gate and no queue entry names it. Its free is
/// therefore withheld by the trace window alone and replayed at the close,
/// where `memory::large_entity::free` unlinks the run and returns the mapping
/// through `memory::os::unmap`. **Neither counter sees that return** — an unmap
/// is not a global free — so what says it happened is the registry, asked
/// before and after. On the way every member's free passes the reset window's
/// deferral arm, which `memory::stdapi::ll_free` tests ahead of the
/// withholding; with no reset in flight each takes its null-window return,
/// and a counting allocator is what says the return was taken.
#[test]
fn an_ordinary_collection_asks_only_what_its_debug_checks_ask() {
    let _g = test_guard();
    crate::cycle::queue::warm_workspace_base();
    // 383 properties is 6,144 bytes, a width nothing else in the suite builds.
    let node = a_class_of_its_own("DenyOrdinaryNode", 383, counting_destructor as *const ());
    let wide = os_direct_class("DenyOrdinaryChild");

    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node, node]) };
    let mut context = LLContext { arena: &mut arena };
    // The child takes the edge as its creation reference rather than through
    // the barrier: no decrement happens, so the candidate gate never sees it
    // and no queue entry names its body. Its free therefore belongs to the
    // trace window rather than the mutator's later candidate retirement.
    let child = unsafe {
        let child = new_constructed(&mut context, wide, MemoryCategory::GcHeap);
        *Object::prop_at(members[0], prop_offset(1)) =
            Value::entity(Tag::Object, child as *mut RcHeader);
        child
    };

    let run = block_of(child);
    assert!(
        crate::memory::large_entity::holds_run(run),
        "the child stands in an OS-direct run, which is the registry the close reaches"
    );
    let _ = take_slots_popped();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let walks_before = premise_cell_walks();
    let _ = allocation_probe::take_heap_deallocations();
    let _ = allocation_probe::take_allocations();
    let freed = unsafe { ll_gc_collect_cycles() };
    let drawn = allocation_probe::take_allocations();
    let given_back = allocation_probe::take_heap_deallocations();
    let walks = premise_cell_walks() - walks_before;
    let marked = take_slots_popped();

    // The membership is the ring and the child: the trace reaches it over its
    // holder's edge and trial deletion leaves it at zero.
    const MEMBERS: usize = 4;
    assert_eq!(freed, MEMBERS, "three members and the child they hold");
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        3,
        "each member ran its destructor, the child's class declaring none"
    );
    for &member in &members {
        assert_eq!(
            unsafe { slot_state(member as *mut RcHeader) },
            SlotState::DeadInPlace
        );
    }

    assert_eq!(
        marked, 1,
        "one return was withheld and popped by the close, and it is the child's: \
         a member is a registered candidate, and `ll_free`'s candidate arm answers \
         ahead of the trace window's"
    );
    assert!(
        !crate::memory::large_entity::holds_run(run),
        "and that replay unlinked the run and gave the mapping back, inside the close"
    );

    let exempt = exempt_allocations(walks, MEMBERS, VALIDATIONS_WITH_A_DESTRUCTOR);
    assert_eq!(
        drawn,
        (exempt, 0),
        "the debug checks and nothing else, and no request to the pool"
    );
    assert_eq!(
        given_back, exempt,
        "and the checks gave back what they took, no other site of the path freeing"
    );
}

/// The cell the case below hands its destructor, and what that destructor read
/// through it. `usize::MAX` until the destructor answers, which no entity
/// address is, so a destructor that never ran fails the case rather than
/// passing it.
static WEAK_CELL: AtomicUsize = AtomicUsize::new(0);
static SEEN_THROUGH_THE_CELL: AtomicUsize = AtomicUsize::new(usize::MAX);

/// A destructor that loads the cell naming the other member, which the
/// finalization nulled before any destructor ran.
unsafe extern "C" fn cell_reading_destructor(_object: *mut Object) {
    let cell = WEAK_CELL.load(Ordering::Relaxed) as *mut crate::weak::LLWeakRef;
    let seen = unsafe { crate::weak::ll_weakref_get(cell) };
    SEEN_THROUGH_THE_CELL.store(seen as usize, Ordering::Relaxed);
    if !seen.is_null() {
        // `get` retains what it resolves, and the case asserts outside the
        // destructor, so the reference goes back here.
        unsafe { ll_release(seen) };
    }
}

/// The weak path: a member a weak cell names is torn down, so the commit walks
/// the per-thread weak table, nulls the cell and unregisters the row before the
/// destructors run, and the cell's own death follows.
///
/// The table's storage is a long-lived buffer payload rather than the global
/// allocator, and the cell is created before the bracket so a growth of that
/// table is the fixture's rather than the collection's.
#[test]
fn a_collection_that_nulls_a_weak_cell_asks_only_what_its_debug_checks_ask() {
    let _g = test_guard();
    crate::cycle::queue::warm_workspace_base();
    // 79 properties is 1,280 bytes and 95 is 1,536: a width of its own for each
    // member, both being freed and both keeping their slots.
    let named = a_class_of_its_own("DenyWeakTarget", 79, std::ptr::null());
    let prober = a_class_of_its_own("DenyWeakProber", 95, cell_reading_destructor as *const ());

    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [named, prober]) };
    let cell = unsafe {
        let mut context = LLContext { arena: &mut arena };
        let cell = crate::weak::ll_weakref_create(&mut context, members[0] as *mut RcHeader);
        assert!(!cell.is_null(), "the fixture's weak cell");
        cell
    };

    WEAK_CELL.store(cell as usize, Ordering::Relaxed);
    SEEN_THROUGH_THE_CELL.store(usize::MAX, Ordering::Relaxed);
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let walks_before = premise_cell_walks();
    let _ = allocation_probe::take_heap_deallocations();
    let _ = allocation_probe::take_allocations();
    let freed = unsafe { ll_gc_collect_cycles() };
    let drawn = allocation_probe::take_allocations();
    let given_back = allocation_probe::take_heap_deallocations();
    let walks = premise_cell_walks() - walks_before;

    const MEMBERS: usize = 2;
    assert_eq!(freed, MEMBERS, "both members of the ring");
    assert_eq!(
        SEEN_THROUGH_THE_CELL.load(Ordering::Relaxed),
        0,
        "the destructor ran and read null through the cell naming the other member"
    );
    for &member in &members {
        assert_eq!(
            unsafe { slot_state(member as *mut RcHeader) },
            SlotState::DeadInPlace
        );
    }

    let exempt = exempt_allocations(walks, MEMBERS, VALIDATIONS_WITH_A_DESTRUCTOR);
    assert_eq!(
        drawn,
        (exempt, 0),
        "the weak table's walk, its removal and the cell's death draw nothing"
    );
    assert_eq!(given_back, exempt, "and free nothing beside the checks");

    unsafe { ll_release(cell as *mut RcHeader) };
}

/// The retained path: both members are former arena objects a reset promoted,
/// so their rows come out of the block's published survivor list rather than a
/// slot index, and their frees take `memory::stdapi::ll_free`'s retained arm.
///
/// The reset runs before the bracket, so what this reads is the collection
/// that follows it; the reset's own frames are the two cases at the end of
/// this module.
///
/// **This is the one case with no size class of its own, and it does not need
/// one**: its members stand in a retained block, which no thread adopts. The
/// block remains held through the membership reading and returns at retirement.
#[test]
fn a_collection_of_retained_survivors_asks_only_what_its_debug_checks_ask() {
    let _g = test_guard();
    crate::cycle::queue::warm_workspace_base();
    let node = ClassBuilder::new("DenyRetainedNode")
        .prop("next", true)
        .destructor(counting_destructor as *const ())
        .build();
    let holder_class = ClassBuilder::new("DenyRetainedHolder")
        .prop("first", true)
        .prop("second", true)
        .build();

    let mut arena = Arena::new();
    let (holder, members) = unsafe {
        let mut context = LLContext { arena: &mut arena };
        let holder = new_constructed(&mut context, holder_class, MemoryCategory::GcHeap);
        let first = new_constructed(&mut context, node, MemoryCategory::RequestArena);
        let second = new_constructed(&mut context, node, MemoryCategory::RequestArena);
        (holder, [first, second])
    };

    unsafe {
        store_prop(&mut arena, members[0], prop_offset(0), members[1]);
        store_prop(&mut arena, members[1], prop_offset(0), members[0]);
        store_prop(&mut arena, holder, prop_offset(0), members[0]);
        store_prop(&mut arena, holder, prop_offset(1), members[1]);
        assert!(!ll_release(members[0] as *mut RcHeader));
        assert!(!ll_release(members[1] as *mut RcHeader));
        crate::promote::arena_reset_full(&mut arena);
    }

    let block = block_of(members[0]);
    assert_eq!(
        unsafe {
            crate::memory::block_pool::load_block_kind(
                &raw const (*BlockHeader::of_ptr(members[0] as *const u8)).kind,
            )
        },
        crate::memory::block_pool::BLOCK_KIND_RETAINED,
        "the reset kept the block the two survivors share"
    );
    for &member in &members {
        let target = unsafe { crate::cycle::row::resolve_edge_target(member as *mut RcHeader) };
        let crate::cycle::row::EdgeTarget::Tracked(row) = target else {
            panic!("a survivor of a retained block resolves to a row");
        };

        assert_eq!(
            row.population,
            crate::cycle::row::Population::Retained,
            "and the trace reaches it through the block's published survivor list"
        );
    }

    // The holder's death is what leaves the ring holding itself: each member
    // takes a non-final decrement and registers as a candidate.
    unsafe {
        assert!(ll_release(holder as *mut RcHeader));
        crate::object::ll_object_die(holder);
    }

    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let walks_before = premise_cell_walks();
    let _ = allocation_probe::take_heap_deallocations();
    let _ = allocation_probe::take_allocations();
    let freed = unsafe { ll_gc_collect_cycles() };
    let drawn = allocation_probe::take_allocations();
    let given_back = allocation_probe::take_heap_deallocations();
    let walks = premise_cell_walks() - walks_before;

    const MEMBERS: usize = 2;
    assert_eq!(freed, MEMBERS, "both survivors");
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), MEMBERS);
    assert_eq!(
        unsafe {
            crate::memory::block_pool::load_block_kind(
                &raw const (*(block as *const BlockHeader)).kind,
            )
        },
        crate::memory::block_pool::BLOCK_KIND_FREE,
        "mutator retirement returned the retained block; its count word is no longer readable as retained"
    );

    let exempt = exempt_allocations(walks, MEMBERS, VALIDATIONS_WITH_A_DESTRUCTOR);
    assert_eq!(drawn, (exempt, 0));
    assert_eq!(given_back, exempt);
}

/// The pressure path: the same graph through the driver an allocation failure
/// starts, which gives its blocks back before the first destructor and tears
/// down the list its sweep harvested rather than the rows.
///
/// The harvest region is a fixed part of the thread's workspace, and the second
/// arena the sever's displaced children wait in is drawn over that same
/// workspace, so the path that returns memory in the middle of itself asks the
/// global allocator for none of it.
#[test]
fn a_collection_under_pressure_asks_only_what_its_debug_checks_ask() {
    let _g = test_guard();
    crate::cycle::queue::warm_workspace_base();
    // 111 properties is 1,792 bytes, a width of its own for the same reason.
    let node = a_class_of_its_own("DenyPressureNode", 111, counting_destructor as *const ());
    let mut arena = Arena::new();
    let members = unsafe { ring(&mut arena, [node, node, node]) };
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    crate::gc::disarm();

    let walks_before = premise_cell_walks();
    let _ = allocation_probe::take_heap_deallocations();
    let _ = allocation_probe::take_allocations();
    let freed = unsafe { collect_under_pressure() };
    let (heap, pool) = allocation_probe::take_allocations();
    let given_back = allocation_probe::take_heap_deallocations();
    let walks = premise_cell_walks() - walks_before;

    const MEMBERS: usize = 3;
    assert_eq!(freed, MEMBERS, "the whole ring, in one round");
    assert_eq!(DESTRUCTOR_RUNS.load(Ordering::Relaxed), MEMBERS);
    assert!(
        !crate::gc::is_armed(),
        "the round traced every root and freed, so it hands nothing to the poll"
    );
    for &member in &members {
        assert_eq!(
            unsafe { slot_state(member as *mut RcHeader) },
            SlotState::DeadInPlace
        );
    }

    let exempt = exempt_allocations(walks, MEMBERS, VALIDATIONS_WITH_A_DESTRUCTOR);
    assert_eq!(heap, exempt, "the debug checks and nothing else");
    assert_eq!(given_back, exempt);
    assert_eq!(
        pool, 0,
        "and the manager was not asked either: this ring's rows fit the workspace \
         bump, so the close returns no block and the teardown's arena re-lends the \
         base the thread already holds"
    );
}

/// The abort: a collection the memory manager refused in the middle of its
/// trace. It ends itself, gives back what it held, and asks the global
/// allocator for nothing at all — a refused round runs no validation, so the
/// debug checks are not even the exemption here.
///
/// The graph is sized rather than guessed: the trace reserves one row array per
/// touched block, and the workspace bump holds a fixed number of them, so the
/// ring spans one block more than fit and the growth past them is what the
/// injection refuses. Both allocation paths of that growth are closed — the
/// pool by this thread's budget and the critical reserve by draining it.
///
/// A thread of its own, because the count above is arithmetic only over a heap
/// whose blocks for this size class this case filled itself.
#[test]
fn a_collection_the_manager_refused_asks_the_allocator_for_nothing() {
    let _g = test_guard();

    let (refused, drawn, given_back, collected, blocks_before, blocks_after, dispatches) =
        std::thread::spawn(|| {
            assert!(
                crate::memory::heap::ll_thread_init(),
                "the pool served this thread"
            );
            crate::cycle::queue::warm_workspace_base();

            // Built here rather than through `node_class`, which registers
            // whatever pointer it is handed: this ring wants no destructor.
            // One property, 32 bytes, and the width every other case avoids.
            // Two reasons it is right here and wrong there. The block count
            // below falls as the row array grows and the array grows with the
            // slots a block holds, so a wider class would put hundreds of
            // blocks under one trace instead of eight. And this ring fills
            // every block it takes exactly, so each is abandoned full: an
            // adopter is served nothing by it and asks the pool, which leaves
            // untouched the premise the cases of a shared class stand on.
            let node = a_class_of_its_own("DenyRefusedNode", 1, std::ptr::null());
            let size = unsafe { (*node).object_size } as usize;
            let class_index =
                crate::memory::heap::size_class_index(size).expect("a size class serves it");
            let slots = BLOCK_PAYLOAD / crate::memory::heap::SIZE_CLASSES[class_index];
            let array = crate::cycle::shadow::bytes_for(slots as u32);
            let blocks = crate::cycle::arena::WORKSPACE_BUMP_BYTES / array + 2;

            let mut arena = Arena::new();
            let ring = unsafe { long_ring(&mut arena, node, blocks * slots) };
            crate::gc::disarm();

            let blocks_before = crate::memory::gc_metadata::thread_stats().current_blocks();
            crate::memory::critical::drain_for_test();
            let _budgeted = crate::memory::block_pool::budget_blocks(0);
            let _ = allocation_probe::take_heap_deallocations();
            let _ = allocation_probe::take_allocations();
            let _ = crate::cycle::row::take_edge_dispatches();
            let refused = unsafe { ll_gc_collect_cycles() };
            let dispatches = crate::cycle::row::take_edge_dispatches();
            let drawn = allocation_probe::take_allocations();
            let given_back = allocation_probe::take_heap_deallocations();
            let blocks_after = crate::memory::gc_metadata::thread_stats().current_blocks();

            // The same entry over the same graph with the injection lifted,
            // which is what makes the zero above the injection's and not the
            // fixture's.
            drop(_budgeted);
            let collected = unsafe { ll_gc_collect_cycles() };
            let _ = ring;
            (
                refused,
                drawn,
                given_back,
                collected,
                blocks_before,
                blocks_after,
                dispatches,
            )
        })
        .join()
        .unwrap();

    assert_eq!(refused, 0, "the refused round freed nothing");
    assert!(
        dispatches > 0,
        "and it was refused inside its trace rather than at the window's open, which \
         resolves no edge at all: the one refused pool request is a growth of the \
         arena and not the draw of the workspace this thread already holds"
    );
    assert!(
        collected > 0,
        "and the same entry over the same graph collects it once the pool answers"
    );
    assert_eq!(
        blocks_after, blocks_before,
        "the round gave back every block it had taken"
    );
    assert_eq!(
        drawn,
        (0, 1),
        "one refused request to the pool, and no heap allocation"
    );
    assert_eq!(given_back, 0);
}

/// The context a destructor below hands to `ll_arena_reset`, and how many
/// resets it ran. Statics because a destructor takes no argument of the
/// case's; the test bracket serializes the suite.
static RESET_CONTEXT: AtomicUsize = AtomicUsize::new(0);
static RESET_RUNS: AtomicUsize = AtomicUsize::new(0);

/// A cycle member's destructor resetting the arena its case mounted, through
/// the C ABI entry generated code calls, so the frames are production's.
unsafe extern "C" fn arena_resetting_destructor(_object: *mut Object) {
    let context = RESET_CONTEXT.load(Ordering::Relaxed) as *mut LLContext;
    assert!(!context.is_null(), "the collection had no arena to reset");
    unsafe { crate::memory::context::ll_arena_reset(context) };
    RESET_RUNS.fetch_add(1, Ordering::Relaxed);
}

/// The entities of [`an_arena_a_destructor_resets`], by role.
struct ResetShape {
    /// A heap object holding `survivor`, which is what makes it an escapee.
    keeper: *mut Object,
    /// An arena object the reset promotes into a retained block.
    survivor: *mut Object,
    /// A COW array `survivor` holds, promoted with it and reconciled.
    array: *mut crate::array::entity::LLArray,
    /// A heap entity an unescaped arena object held, whose free the release
    /// log makes inside the window: an OS-direct run, so the free is a large
    /// one and takes the window's deferred stack to the close.
    kept: *mut Object,
}

/// An arena carrying one of each thing a reset keeps a record of: a
/// survivor a heap keeper holds, a COW array that survivor holds, an
/// unescaped object with a destructor of its own, and a heap entity that
/// object held. `unescaped_destructor` is the unescaped object's.
///
/// Both first touches the reset would otherwise make on this thread are made
/// here, so a pool request inside the bracket is the reset's and not the
/// thread's history: one `ll_alloc` at the window's segment size opens the
/// heap's block at that class, and one long-lived payload opens the buffer
/// arena the array's storage is carried into.
///
/// # Safety
/// As [`ring`]. The caller mounts `context` as the current one and hands
/// it to the destructor through `RESET_CONTEXT`.
unsafe fn an_arena_a_destructor_resets(
    context: *mut LLContext,
    unescaped_destructor: *const (),
) -> ResetShape {
    use crate::array::entity::ll_array_new;
    // 159 properties is 2,560 bytes, a width of its own for the keeper.
    let keeper_class = a_class_of_its_own("DenyResetKeeper", 159, std::ptr::null());
    let kept_class = os_direct_class("DenyResetKept");
    let survivor_class = ClassBuilder::new("DenyResetSurvivor")
        .prop("items", true)
        .build();
    let unescaped_class = ClassBuilder::new("DenyResetUnescaped")
        .prop("kept", true)
        .destructor(unescaped_destructor)
        .build();

    unsafe {
        const SEGMENT_BYTES: usize = 4096;
        let warmed = crate::memory::stdapi::ll_alloc(SEGMENT_BYTES, 8);
        assert!(!warmed.is_null());
        crate::memory::stdapi::ll_free(warmed);
        let (payload, capacity) = crate::memory::buffer_arena::buffer_alloc_longlived_payload(64);
        assert!(!payload.is_null());
        crate::memory::buffer_arena::buffer_free_longlived_payload(payload, capacity);
    }

    let arena = unsafe { (*context).arena };
    let (keeper, survivor, array, unescaped, kept) = unsafe {
        let keeper = new_constructed(context, keeper_class, MemoryCategory::GcHeap);
        let survivor = new_constructed(context, survivor_class, MemoryCategory::RequestArena);
        let array = ll_array_new(MemoryCategory::RequestArena);
        let unescaped = new_constructed(context, unescaped_class, MemoryCategory::RequestArena);
        let kept = new_constructed(context, kept_class, MemoryCategory::GcHeap);
        (keeper, survivor, array, unescaped, kept)
    };

    unsafe {
        assert!(crate::array::testing::push(array, Value::int(7)));
        let slot = Object::prop_at(survivor, prop_offset(0));
        assert!(crate::memory::barrier::ref_store(
            arena,
            survivor as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        store_prop(arena, keeper, prop_offset(0), survivor);
        store_prop(arena, unescaped, prop_offset(0), kept);
        // The unescaped object's edge is what holds `kept` now, so the release
        // log's one record is a death rather than a decrement.
        assert!(!ll_release(kept as *mut RcHeader));
    }

    ResetShape {
        keeper,
        survivor,
        array,
        kept,
    }
}

/// What both reset cases read after the bracket: the ring collected, the
/// reset run once inside it, the survivor promoted into a retained block
/// listed beside its array, the array's count settled to that one holder,
/// and `kept`'s run given back by the window's close.
///
/// # Safety
/// The bracket has closed and `shape` is the one the case built.
unsafe fn read_the_reset(shape: &ResetShape, members: &[*mut Object], freed: usize) {
    assert_eq!(freed, members.len(), "every member of the ring");
    assert_eq!(
        RESET_RUNS.load(Ordering::Relaxed),
        1,
        "the reset ran inside the collection"
    );
    for &member in members {
        assert_eq!(
            unsafe { slot_state(member as *mut RcHeader) },
            SlotState::DeadInPlace
        );
    }

    unsafe {
        assert_eq!(
            crate::refcount::entity_category(shape.survivor as *mut RcHeader),
            MemoryCategory::GcHeap,
            "the keeper's survivor was promoted"
        );
        let block = block_of(shape.survivor);
        assert_eq!(
            crate::memory::block_pool::load_block_kind(
                &raw const (*(block as *const BlockHeader)).kind
            ),
            crate::memory::block_pool::BLOCK_KIND_RETAINED,
            "into a retained block"
        );
        let mut listed = vec![shape.survivor as usize, shape.array as usize];
        listed.sort_unstable();
        assert_eq!(
            crate::memory::retained::survivor_list_copy(block),
            listed,
            "whose list names it and the array it holds"
        );
        assert_eq!(
            crate::refcount::entity_category(shape.array as *mut RcHeader),
            MemoryCategory::GcHeap,
            "the array went with it"
        );
        assert_eq!(
            crate::refcount::entity_refcount(shape.array as *mut RcHeader),
            1,
            "held by the survivor and by nothing else"
        );
        // `kept`'s header is unmapped with its run, so the registry is what
        // is asked, and its address is arithmetic on a pointer never read.
        assert!(
            !crate::memory::large_entity::holds_run(block_of(shape.kept)),
            "the release log released what the unescaped object held, to its death, \
             and the window's close gave its run back"
        );
    }
}

/// Give a reset case's heap holder back, and with it the survivor, the array
/// and the retained block they stand in.
///
/// # Safety
/// As [`read_the_reset`].
unsafe fn release_the_keeper(shape: &ResetShape) {
    unsafe {
        assert!(ll_release(shape.keeper as *mut RcHeader));
        ll_object_die(shape.keeper);
    }
}

/// The frames a step-4 destructor's `ll_arena_reset` enters from inside a
/// collection: the reset's fixpoint, its counting pass, the block retention
/// and the survivor list it publishes, the COW reconciliation off the
/// window's log, the release-at-reset drain, and the window's deferred stack
/// of large frees — none of them a frame of the collection's own, and none
/// exempt (`dev/DECISIONS.md`, "the reset window's memory comes from the
/// manager", the ruling's second paragraph).
///
/// Which arm each record took is read rather than assumed: no promotion
/// edge and no capture was refused, `kept` died inside the window and its
/// run was unlinked by the close, and the collection's own trace withheld
/// nothing, `kept`'s block never having been met by it.
///
/// What the counter cannot see is an allocation that bypasses the crate's
/// `#[global_allocator]` — libc's own, as the registration of a
/// thread-local's drop glue makes on its first touch. The census of the
/// crate's thread-locals is that rule's coverage
/// (`memory::critical::tests::where_the_first_touch_happens`), not this case.
#[test]
fn a_collection_whose_destructor_resets_an_arena_asks_only_what_its_debug_checks_ask() {
    let _g = test_guard();
    crate::cycle::queue::warm_workspace_base();
    // 127 properties is 2,048 bytes, a width of its own for the ring.
    let node = a_class_of_its_own(
        "DenyResetNode",
        127,
        arena_resetting_destructor as *const (),
    );
    let other = a_class_of_its_own("DenyResetOther", 127, counting_destructor as *const ());

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    crate::memory::context::set_current_context(context_ptr);
    let shape =
        unsafe { an_arena_a_destructor_resets(context_ptr, counting_destructor as *const ()) };
    let run = block_of(shape.kept);
    assert!(crate::memory::large_entity::holds_run(run));

    let members = unsafe { ring(&mut *arena_ptr, [node, other]) };
    RESET_CONTEXT.store(context_ptr as usize, Ordering::Relaxed);
    RESET_RUNS.store(0, Ordering::Relaxed);
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let _ = crate::memory::reset_window::take_counters();
    let _ = crate::memory::reset_window::take_refused_records();
    let _ = crate::promote::take_refused_captures();
    let _ = take_slots_popped();

    let walks_before = premise_cell_walks();
    let _ = allocation_probe::take_heap_deallocations();
    let _ = allocation_probe::take_allocations();
    let freed = unsafe { ll_gc_collect_cycles() };
    let (heap, pool) = allocation_probe::take_allocations();
    let given_back = allocation_probe::take_heap_deallocations();
    let walks = premise_cell_walks() - walks_before;
    RESET_CONTEXT.store(0, Ordering::Relaxed);
    crate::memory::context::set_current_context(std::ptr::null_mut());

    const MEMBERS: usize = 2;
    unsafe { read_the_reset(&shape, &members, freed) };
    assert_eq!(
        DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        2,
        "the other member's destructor, and the unescaped object's inside the reset"
    );
    assert_eq!(
        crate::memory::reset_window::take_counters().0,
        1,
        "one slot was taken inside the window, `kept`'s, so its free went to the \
         deferred stack rather than straight to the registry"
    );
    assert_eq!(crate::memory::reset_window::take_refused_records(), 0);
    assert_eq!(crate::promote::take_refused_captures(), 0);
    assert_eq!(
        take_slots_popped(),
        0,
        "the trace never met `kept`'s run, so the collection's own window withheld \
         nothing: the close that returned the run was the reset's"
    );

    let exempt = exempt_allocations(walks, MEMBERS, VALIDATIONS_WITH_A_DESTRUCTOR);
    assert_eq!(
        heap, exempt,
        "the collection's debug checks and nothing else: the reset's own frames \
         drew nothing from the global allocator"
    );
    assert_eq!(
        given_back, exempt,
        "and the reset freed nothing there either"
    );
    assert_eq!(
        pool, 0,
        "and asked the manager for no block: the window's segment came out of the \
         warmed class and the array's storage out of the opened buffer arena"
    );

    unsafe { release_the_keeper(&shape) };
}

/// The same reset with the array's carry refused, which is the arm the
/// pinned set was deleted for: the storage stays where it is, the block it
/// stands in is pinned for it, and the reset spends that pin past
/// `finish_reset` by walking its chain through the block.
///
/// The refusal is the buffer arena's injected one, proven by its own count,
/// so the pool is asked for nothing and the zero on that axis is the reset's.
#[test]
fn a_collection_whose_destructor_resets_an_arena_with_a_refused_carry_asks_only_what_its_debug_checks_ask()
 {
    use crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED;
    let _g = test_guard();
    crate::cycle::queue::warm_workspace_base();
    let node = a_class_of_its_own(
        "DenyResetPinningNode",
        127,
        arena_resetting_destructor as *const (),
    );
    let other = a_class_of_its_own(
        "DenyResetPinningOther",
        127,
        counting_destructor as *const (),
    );

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    crate::memory::context::set_current_context(context_ptr);
    let shape =
        unsafe { an_arena_a_destructor_resets(context_ptr, counting_destructor as *const ()) };
    let storage_before = unsafe { crate::array::entity::storage_address(shape.array) };

    let members = unsafe { ring(&mut *arena_ptr, [node, other]) };
    RESET_CONTEXT.store(context_ptr as usize, Ordering::Relaxed);
    RESET_RUNS.store(0, Ordering::Relaxed);
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);
    let _ = crate::promote::take_pins_spent();

    let refusals_before = crate::memory::buffer_arena::refusals();
    let walks_before = premise_cell_walks();
    let _ = allocation_probe::take_heap_deallocations();
    let _ = allocation_probe::take_allocations();
    FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
    let freed = unsafe { ll_gc_collect_cycles() };
    FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);
    let (heap, pool) = allocation_probe::take_allocations();
    let given_back = allocation_probe::take_heap_deallocations();
    let walks = premise_cell_walks() - walks_before;
    RESET_CONTEXT.store(0, Ordering::Relaxed);
    crate::memory::context::set_current_context(std::ptr::null_mut());

    const MEMBERS: usize = 2;
    unsafe { read_the_reset(&shape, &members, freed) };
    assert_eq!(
        crate::memory::buffer_arena::refusals() - refusals_before,
        1,
        "the carry was refused once, and nothing else asked the buffer arena"
    );
    assert_eq!(
        unsafe { crate::array::entity::storage_address(shape.array) },
        storage_before,
        "so the storage stayed in the arena's block"
    );
    assert_eq!(
        crate::promote::take_pins_spent(),
        1,
        "which the reset pinned for it and unpinned past `finish_reset` by walking \
         its chain"
    );

    let exempt = exempt_allocations(walks, MEMBERS, VALIDATIONS_WITH_A_DESTRUCTOR);
    assert_eq!(
        heap, exempt,
        "the pin, its chain and its release draw nothing"
    );
    assert_eq!(given_back, exempt);
    assert_eq!(pool, 0);

    unsafe { release_the_keeper(&shape) };
}
