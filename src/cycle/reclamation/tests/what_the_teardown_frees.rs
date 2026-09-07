//! What a confirmed component is left as: its internal edges cut, every member
//! freed through the ordinary death path, and the children the cut displaced
//! out of it dropped behind the last of those frees.
//!
//! The order is the subject of the first case, and it is read from inside a
//! displaced child's own destructor: user code that runs while a member's slot
//! is still live is user code that could store that member in a root, which is
//! what step 6 exists to make impossible (`rfc/model/gc/rc-cycle.md`, "Cycle
//! finalization and reclamation", step 6).

use super::*;
use crate::refcount::{is_registered_candidate, ll_retain, mutator_flags, take_admissions};

/// The members a displaced child's destructor reads the state of, and the two
/// readings it takes.
///
/// Statics rather than a fixture parameter because the destructor is a C ABI
/// function pointer a class carries: what it can see is what a class-less
/// place holds for it.
static WATCHED_MEMBERS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];

/// Watched members whose slot was already freed when the child's destructor
/// ran. Two is the reading step 6 asks for.
static MEMBERS_FREED_BEFORE_THE_DROP: AtomicUsize = AtomicUsize::new(0);

/// Displaced children whose destructor has run since a case cleared it.
static CHILD_DESTRUCTORS: AtomicUsize = AtomicUsize::new(0);

/// The sequence probe of the `done:` clause: a displaced child's destructor
/// reads the slot of every member of the component it was displaced out of.
///
/// A slot the teardown freed reads [`SlotState::DeadInPlace`] — the free took
/// it and nothing has handed it back — and one whose member is still guarded
/// reads [`SlotState::Live`]. Reading it is safe because the block is still
/// commissioned: the fixture holds an object of the members' own class for the
/// whole case, so the block cannot empty and reach the pool.
unsafe extern "C" fn watching_destructor(_obj: *mut Object) {
    CHILD_DESTRUCTORS.fetch_add(1, Ordering::Relaxed);
    let freed = WATCHED_MEMBERS
        .iter()
        .filter(|slot| {
            let member = slot.load(Ordering::Relaxed) as *const RcHeader;
            !member.is_null() && unsafe { slot_state(member) } == SlotState::DeadInPlace
        })
        .count();

    MEMBERS_FREED_BEFORE_THE_DROP.store(freed, Ordering::Relaxed);
}

/// A ring of two members holding one external child, which is the smallest
/// arrangement with both an internal edge and an external one.
///
/// The keeper is the object that holds the members' block open: a case reads a
/// freed member's slot, and a block whose last occupant died goes back to the
/// pool.
struct RingWithAChild {
    members: [*mut Object; 2],
    _keeper: *mut Object,
}

/// Build the ring, spend every creation reference and let a trace read the two
/// members as unreachable.
///
/// # Safety
/// `arena` is this thread's.
unsafe fn ring_with_a_child(arena: &mut Arena, child_class: *const Class) -> RingWithAChild {
    let node = node_class("ReclamationRingNode");
    let keeper = unsafe { object(arena, node) };
    let [first, second] = unsafe { ring(arena, [node, node]) };
    let child = unsafe { object(arena, child_class) };

    unsafe {
        store_prop(arena, first, prop_offset(1), child);
        spend_creation_references(&[child]);
        traced_unreachable(first, &[first, second]);
    }

    RingWithAChild {
        members: [first, second],
        _keeper: keeper,
    }
}

#[test]
fn every_member_is_freed_before_a_displaced_child_is_dropped() {
    let _g = test_guard();
    let watcher = ClassBuilder::new("ReclamationWatchingChild")
        .destructor(watching_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let ring = unsafe { ring_with_a_child(&mut arena, watcher) };
    for (slot, member) in WATCHED_MEMBERS.iter().zip(ring.members) {
        slot.store(member as usize, Ordering::Relaxed);
    }

    CHILD_DESTRUCTORS.store(0, Ordering::Relaxed);
    MEMBERS_FREED_BEFORE_THE_DROP.store(0, Ordering::Relaxed);

    let mut scratch = open_arena();
    let mut members = headers(ring.members);
    assert_eq!(
        unsafe { commit(&mut members, &mut scratch) },
        Reclaimed::Freed
    );

    assert_eq!(
        CHILD_DESTRUCTORS.load(Ordering::Relaxed),
        1,
        "the child the sever displaced was held by the component alone, so \
         the drop is its death and its destructor runs once"
    );
    assert_eq!(
        MEMBERS_FREED_BEFORE_THE_DROP.load(Ordering::Relaxed),
        2,
        "the drop is deferred past the last member's free: user code running \
         between the sever and the free could root a member of a component \
         whose fields are already null"
    );
    for &member in &members {
        assert_eq!(
            unsafe { slot_state(member) },
            SlotState::DeadInPlace,
            "every member's slot went back at its own free"
        );
    }

    scratch.reset();
}

#[test]
fn an_edge_inside_the_component_writes_no_candidate_entry() {
    let _g = test_guard();
    let node = node_class("ReclamationUnregisteredNode");

    // **One member of the ring carries no candidate bit**, and the case turns
    // on it: the gate refuses a member that is already a candidate, so a ring
    // whose every member was registered by its own creation release reads the
    // same whichever release the sever uses. The second member's creation
    // reference is spent through the narrow counter store instead, which is
    // the state of a member a trace reached rather than a root the queue
    // named. That is why the ring is built here rather than through
    // `cycle::testing::ring`, which spends both references through the
    // candidate gate.
    let mut arena = Arena::new();
    let first = unsafe { object(&mut arena, node) };
    let second = unsafe { object(&mut arena, node) };
    unsafe {
        store_prop(&mut arena, first, prop_offset(0), second);
        store_prop(&mut arena, second, prop_offset(0), first);
        spend_creation_references(&[first]);
        crate::refcount::mutator_unguard_release(second as *mut RcHeader);
        traced_unreachable(first, &[first, second]);
    }

    assert!(
        !unsafe { is_registered_candidate(mutator_flags(second as *mut RcHeader)) },
        "the fixture's own premise: no entry names this member, so a counted \
         release of the edge naming it would be admitted"
    );

    let mut scratch = open_arena();
    let mut members = headers([first, second]);
    take_admissions();
    assert_eq!(
        unsafe { commit(&mut members, &mut scratch) },
        Reclaimed::Freed
    );

    assert_eq!(
        take_admissions(),
        0,
        "an internal edge comes off through the narrow counter store: a \
         counted release the gate admits would write a queue entry naming a \
         slot this teardown is about to hand back"
    );
    scratch.reset();
}

#[test]
fn a_member_a_candidate_entry_names_keeps_its_slot_withheld() {
    let _g = test_guard();
    let plain = ClassBuilder::new("ReclamationCandidateChild").build();

    let mut arena = Arena::new();
    let ring = unsafe { ring_with_a_child(&mut arena, plain) };
    let [first, _second] = ring.members;

    // The ordinary road onto the queue: a decrement that does not reach zero.
    // What it leaves in the header is the bit `ll_free` refuses a return on.
    unsafe {
        ll_retain(first as *mut RcHeader);
        assert!(!ll_release(first as *mut RcHeader));
    }

    assert!(
        unsafe { is_registered_candidate(mutator_flags(first as *mut RcHeader)) },
        "the fixture's own premise: an entry of the queue names this member"
    );

    let mut scratch = open_arena();
    let mut members = headers(ring.members);
    assert_eq!(
        unsafe { commit(&mut members, &mut scratch) },
        Reclaimed::Freed
    );

    // **The teardown clears no candidate bit**, so the free withholds the slot
    // instead of returning it. That is what keeps the entry's address readable
    // for the trace that pops it: `cycle::mark` reads a root's refcount before
    // its cells and drops it at zero, and it may read at all only because the
    // mutator does not free a slot an entry names. The slot comes back when
    // the entry is retired, which no step builds yet (`PLAN.md` S39.1).
    assert!(
        unsafe { is_registered_candidate(mutator_flags(members[0])) },
        "the member the queue names keeps its bit"
    );
    // **Two probes, because the free list is a stack.** The other member's
    // slot went back and stands at the head, so one probe reads it whether or
    // not the withheld slot went back behind it; the second is what the
    // withheld one would answer.
    let probe_class = node_class("ReclamationCandidateProbe");
    let probes = [unsafe { object(&mut arena, probe_class) }, unsafe {
        object(&mut arena, probe_class)
    }];
    for probe in probes {
        assert_ne!(
            probe as *mut RcHeader, members[0],
            "the allocator is not handed the slot the candidate bit withholds"
        );
    }

    for probe in probes {
        unsafe {
            assert!(ll_release(probe as *mut RcHeader));
            crate::object::ll_object_die(probe);
        }
    }

    scratch.reset();
}
