//! What a collection asks the global allocator between an entry and its last
//! free, which the GC-memory contract answers with "nothing": every byte a
//! collection holds comes through the memory manager, and a counting allocator
//! standing under the whole crate is what says so (`PLAN.md` S36.9).
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
//! the sites that give memory back rather than take it — `reset_window`'s
//! `snapshots.remove` among them — and a counting allocator reads a `free` as
//! nothing on the allocation axis alone.
//!
//! **A zero is admitted only where the same entry over the same graph answers
//! a teardown.** An arm whose collection refused would allocate nothing by
//! doing nothing, so each case names what it collected: the answer, the
//! destructor count, the slot states, and the marked slots the close popped.

use super::*;
use crate::cycle::deferred_slot_reuse::take_marked_slots_visited;
use crate::cycle::validation::{EXEMPT_ALLOCATIONS_PER_VALIDATION, premise_cell_walks};
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::test_support::allocation_probe;
use crate::value::{Tag, Value};

/// What `cycle::validation`'s two debug checks allocated inside a bracket.
///
/// `walks` is the premise counter's rise across the bracket and `members` the
/// membership of the one commit inside it. One `validate_component` walks every
/// member's cells once, so their quotient is how many validations the commit
/// ran — read off the instrument rather than taken from the protocol, which
/// runs one validation for a component with no destructor and two for one with.
/// A release build runs neither check, records no walk, and this answers zero.
fn exempt_allocations(walks: usize, members: usize) -> usize {
    assert_eq!(
        walks % members,
        0,
        "the walks of a bracket holding one commit over `members` members are a \
         multiple of it; what pins the membership itself is the caller's reading of \
         what the collection freed"
    );
    (walks / members) * EXEMPT_ALLOCATIONS_PER_VALIDATION
}

/// A class of `properties` Box properties whose instance fits one size class
/// exactly, with a destructor and with the ring's edge at `prop_offset(0)`.
///
/// **The exact fit is what keeps the case's blocks its own**, and it is not
/// decoration: the members a case frees keep their slots, the entry naming each
/// one withholding the return (`PLAN.md` S39.2), so the block never empties and
/// is abandoned full when the thread exits. A case that reasons over a block of
/// its class as one it filled itself then reads foreign occupants
/// (`memory::heap::tests::the_collection_a_refusal_starts`, which sizes its
/// classes the same way, and `dev/POSTMORTEM.md`, 2026-09-07).
///
/// Exactness pins that this instance does not round up into another class's
/// bucket. It does not stop a narrower instance from rounding up into this one,
/// so a width shared with the suite is chosen only where the case leaves its
/// blocks **full** — an adopter of a full block is served nothing by it and asks
/// the pool, which is the premise other cases stand on.
///
/// The price is stated rather than hidden: a block of a width nothing else
/// builds is never adopted either, so what these cases withhold stays out of
/// circulation for the life of the process. Four blocks at the four wide
/// classes, and eight at the abort case's.
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

/// The ordinary path, and with it the frames a large body's death enters: a
/// garbage ring holding one child whose body is an OS-direct run.
///
/// The child takes its holder's edge as its creation reference, so no decrement
/// ever reached the candidate gate and no queue entry names it. Its free is
/// therefore withheld by the trace window alone and replayed at the close,
/// where `memory::large_entity::free` unlinks the run and returns the mapping
/// through `memory::os::unmap`. **Neither counter sees that return** — an unmap
/// is not a global free — so what says it happened is the registry, asked
/// before and after. On the way the free passes the large-body arm
/// `memory::stdapi::ll_free`
/// tests ahead of the withholding, and every member's death passes
/// `memory::reset_window::record_death`; with no reset in flight each takes its
/// null-window return, and a counting allocator is what says the return was
/// taken.
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
    // and no queue entry names its body. That is what lets its free run to the
    // end inside the collection instead of standing withheld past it
    // (`PLAN.md` S39.2).
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
    let _ = take_marked_slots_visited();
    DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    let walks_before = premise_cell_walks();
    let _ = allocation_probe::take_heap_deallocations();
    let _ = allocation_probe::take_allocations();
    let freed = unsafe { ll_gc_collect_cycles() };
    let drawn = allocation_probe::take_allocations();
    let given_back = allocation_probe::take_heap_deallocations();
    let walks = premise_cell_walks() - walks_before;
    let marked = take_marked_slots_visited();

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
         ahead of the trace window's (`PLAN.md` S39.2)"
    );
    assert!(
        !crate::memory::large_entity::holds_run(run),
        "and that replay unlinked the run and gave the mapping back, inside the close"
    );

    let exempt = exempt_allocations(walks, MEMBERS);
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

    let exempt = exempt_allocations(walks, MEMBERS);
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
/// The reset runs before the bracket, its own containers being the disclosed
/// exemption of another module (`dev/design/retained-index-ownership.md`); what
/// this reads is the collection that follows it.
///
/// **This is the one case with no size class of its own, and it does not need
/// one**: its members stand in a retained block, which no thread adopts. What
/// it does leave is that block, with two occupants the entries naming their
/// slots withhold for the life of the process, and every later heap census
/// visits them.
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
        unsafe { crate::memory::retained::live_occupant_count(block) } as usize,
        MEMBERS,
        "and the block still counts them, the entries naming their slots withholding \
         the return (`PLAN.md` S39.2)"
    );

    let exempt = exempt_allocations(walks, MEMBERS);
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

    let exempt = exempt_allocations(walks, MEMBERS);
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
