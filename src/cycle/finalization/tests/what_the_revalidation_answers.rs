//! What the exact validation reads about a component once its destructors have
//! run, and what each answer leaves behind.
//!
//! A destructor is handed `$this`, so unreachability stops being monotone at
//! step 4: the component is read again with the guard reference subtracted,
//! and a component user code kept a reference to gets its true counts back
//! with its destructors already behind it (`rfc/model/gc/rc-cycle.md`, "Cycle
//! finalization and reclamation", step 5).

use super::*;
use crate::memory::barrier::write_value_slot;
use crate::object::ll_default_dispose;
use crate::refcount::take_admissions;
use crate::test_support::entity_checked;
use crate::value::{Tag, Value};

/// The member a destructor of this file keeps a reference to, standing for the
/// root user code stored `$this` in.
static KEPT_MEMBER: AtomicUsize = AtomicUsize::new(0);

/// Teardown bodies run since a case last cleared it, counted by a `dispose` of
/// the fixture's own.
///
/// It is what tells a member that died at the release from one that survived
/// it: a destructor already run is not run again, so the death of a member
/// whose guard was its last reference leaves no other mark.
static DEATHS: AtomicUsize = AtomicUsize::new(0);

/// A destructor that stores `$this` where the component cannot reach it, which
/// is the resurrection step 5 exists to read.
///
/// The reference is a retain and a static rather than a store into an entity:
/// what the revalidation reads is the count, and a root outside the GC heap
/// contributes exactly this.
unsafe extern "C" fn keeping_destructor(obj: *mut Object) {
    unsafe { ll_retain(obj as *mut RcHeader) };
    KEPT_MEMBER.store(obj as usize, Ordering::Relaxed);
}

/// A destructor that keeps `$this` and gives up its own edge to the next
/// member: the slot is emptied and the reference released, which is the pair
/// of acts a store of null through the barrier performs over a Box property
/// whose owner and old value are both of the GC heap.
unsafe extern "C" fn keeping_and_releasing_destructor(obj: *mut Object) {
    let slot = unsafe { Object::prop_at(obj, prop_offset(0)) };
    let next = unsafe { crate::test_support::entity_checked(&*slot) };
    unsafe { write_value_slot(slot, Value::null()) };
    assert!(
        !unsafe { ll_release(next) },
        "the member this edge named carries a guard of its own"
    );
    unsafe { keeping_destructor(obj) };
}

/// The external child of a dying member, whose destructor runs inside the
/// release rather than in the destructor pass.
static CHILD_DESTRUCTOR_RUNS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn child_destructor(_obj: *mut Object) {
    CHILD_DESTRUCTOR_RUNS.fetch_add(1, Ordering::Relaxed);
}

/// A `dispose` that counts the teardown and then runs the one every class
/// carries until the compiler emits its own.
unsafe extern "C" fn counting_dispose(obj: *mut Object) -> bool {
    DEATHS.fetch_add(1, Ordering::Relaxed);
    unsafe { ll_default_dispose(obj) }
}

/// A ring of `MEMBERS` objects, each naming the next through `prop_offset(0)`
/// and the last naming the first, held by nothing else and read as unreachable
/// by a trace that has released its rows.
///
/// `classes` is one class per member, in ring order.
///
/// # Safety
/// `arena` is this thread's and every class carries one Box property at
/// `prop_offset(0)`.
unsafe fn unreachable_ring<const MEMBERS: usize>(
    arena: &mut Arena,
    classes: [*const crate::class::Class; MEMBERS],
) -> [*mut Object; MEMBERS] {
    let mut context = LLContext { arena: &mut *arena };
    let ring = classes
        .map(|class| unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) });
    unsafe {
        for (index, &member) in ring.iter().enumerate() {
            store_prop(arena, member, prop_offset(0), ring[(index + 1) % MEMBERS]);
        }

        for &member in &ring {
            assert!(!ll_release(member as *mut RcHeader));
        }
    }

    let expected: Vec<*mut Object> = ring.to_vec();
    let mut shadow_arena = unsafe { traced_unreachable_from(ring[0], &expected) };
    shadow_arena.reset();
    ring
}

/// The members of `ring` as the header pointers a finalization takes.
fn headers<const MEMBERS: usize>(ring: &[*mut Object; MEMBERS]) -> [*mut RcHeader; MEMBERS] {
    ring.map(|member| member as *mut RcHeader)
}

#[test]
#[cfg(debug_assertions)]
fn a_finalization_no_destructor_ran_in_is_not_read_again() {
    use crate::cycle::validation::premise_cell_walks;

    let _g = test_guard();
    let silent = ClassBuilder::new("FinalizationSilentNode")
        .prop("next", true)
        .build();
    let speaking = ClassBuilder::new("FinalizationSpeakingNode")
        .prop("next", true)
        .destructor(keeping_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    // One member of the second ring carries a destructor and the rest of the
    // two arms is the same fixture: what separates the readings is whether a
    // destructor ran, and nothing else.
    for (classes, walks_owed) in [([silent, silent], 0), ([silent, speaking], 2)] {
        let ring = unsafe { unreachable_ring(&mut arena, classes) };
        KEPT_MEMBER.store(0, Ordering::Relaxed);

        let mut finalization = Finalization::begin();
        let mut members = headers(&ring);
        assert_eq!(
            unsafe { finalization.confirm(&mut members) },
            ValidationResult::Unreachable
        );

        let mut pass = finalization.seal().destructors();
        unsafe { pass.run(&members) };
        let mut revalidation = pass.close();

        // The answer holds the revalidation, so it is read and discharged
        // inside a block of its own: past it the revalidation is the caller's
        // again and can be closed.
        {
            // Handed back in the caller's own order, which the sever's
            // membership test reads by binary search: the fast path sorts
            // where the exact validation would have.
            members.reverse();
            let before = premise_cell_walks();
            let answer = unsafe { revalidation.revalidate(&mut members) };
            assert!(
                members.is_sorted(),
                "a component comes back sorted whether or not it was read again"
            );
            assert_eq!(
                premise_cell_walks() - before,
                walks_owed,
                "the exact validation is taken again only where a destructor \
                 ran, and its premise check walks one member's cells per member"
            );

            // The arm that ran a destructor kept `$this`, so its component is
            // the one the revalidation reads as externally referenced.
            let kept = KEPT_MEMBER.swap(0, Ordering::Relaxed) as *mut Object;
            match answer {
                Revalidated::Unreachable(guarded) => {
                    unsafe { unwind_guarded_ring(&mut arena, ring) };
                    unsafe { guarded.guards_released() };
                }
                Revalidated::ExternallyReferenced => {
                    assert!(!kept.is_null(), "only a destructor can have kept one");
                    unsafe {
                        assert!(!ll_release(kept as *mut RcHeader));
                        dismantle_ring(&mut arena, ring);
                    }
                }
            }
        }

        revalidation.close();
    }
}

#[test]
fn a_destructor_that_keeps_this_leaves_the_component_with_its_true_counts() {
    let _g = test_guard();
    let plain = ClassBuilder::new("FinalizationResurrectedPeer")
        .prop("next", true)
        .build();
    let keeper = ClassBuilder::new("FinalizationResurrectingNode")
        .prop("next", true)
        .destructor(keeping_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let ring = unsafe { unreachable_ring(&mut arena, [plain, keeper]) };
    let [peer, keeper_member] = ring;
    KEPT_MEMBER.store(0, Ordering::Relaxed);

    let mut context = LLContext { arena: &mut arena };
    let cell = unsafe { ll_weakref_create(&mut context, peer as *mut RcHeader) };
    assert!(!cell.is_null(), "the fixture's weak cell");

    let mut finalization = Finalization::begin();
    let mut members = headers(&ring);
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable
    );

    let mut pass = finalization.seal().destructors();
    unsafe { pass.run(&members) };
    assert_eq!(
        KEPT_MEMBER.load(Ordering::Relaxed) as *mut Object,
        keeper_member,
        "the destructor of this component is the one that kept `$this`"
    );

    let mut revalidation = pass.close();
    assert!(
        matches!(
            unsafe { revalidation.revalidate(&mut members) },
            Revalidated::ExternallyReferenced
        ),
        "a reference the component does not contain is what the second \
         reading is taken for"
    );
    revalidation.close();

    assert_eq!(
        unsafe { refcounts(&ring) },
        vec![1, 2],
        "the guards are off and every member carries its true count: the \
         ring's own edge on the peer, and the edge plus the reference user \
         code kept on the member whose destructor ran"
    );
    assert!(
        unsafe { ll_weakref_get(cell) }.is_null(),
        "the cell nulled before the destructors stays null: this design does \
         not restore a weak reference after a resurrection, which is where it \
         parts from PHP"
    );

    unsafe {
        drop_cell(cell);
        assert!(!ll_release(keeper_member as *mut RcHeader));
        dismantle_ring(&mut arena, ring);
    }
}

#[test]
fn a_member_whose_guard_was_its_last_reference_dies_at_the_release() {
    let _g = test_guard();
    let plain = ClassBuilder::new("FinalizationReleasedPeer")
        .prop("next", true)
        .dispose(counting_dispose as *const ())
        .build();
    let keeper = ClassBuilder::new("FinalizationReleasingKeeper")
        .prop("next", true)
        .dispose(counting_dispose as *const ())
        .destructor(keeping_and_releasing_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    // The keeper is last, so the edge it gives up is the one naming the first
    // member — which leaves that member holding its guard and nothing else.
    let ring = unsafe { unreachable_ring(&mut arena, [plain, plain, keeper]) };
    let [first, _, keeper_member] = ring;
    KEPT_MEMBER.store(0, Ordering::Relaxed);
    DEATHS.store(0, Ordering::Relaxed);

    let mut finalization = Finalization::begin();
    let mut members = headers(&ring);
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable
    );

    let mut pass = finalization.seal().destructors();
    unsafe { pass.run(&members) };
    assert_eq!(
        unsafe { header_refcount(first as *mut RcHeader) },
        1,
        "the destructor gave up the only in-component edge into the first \
         member, which leaves it carrying its guard alone"
    );

    let mut revalidation = pass.close();
    assert!(
        matches!(
            unsafe { revalidation.revalidate(&mut members) },
            Revalidated::ExternallyReferenced
        ),
        "the reference the destructor kept is one the component does not contain"
    );
    revalidation.close();

    assert_eq!(
        DEATHS.load(Ordering::Relaxed),
        2,
        "the counted release lets a member nothing else names die the \
         ordinary death, and the second member dies in the cascade the first \
         one's teardown starts"
    );
    assert_eq!(
        unsafe { header_refcount(keeper_member as *mut RcHeader) },
        1,
        "what the survivor carries is the reference user code kept, its own \
         in-component edge having died with the member that held it"
    );

    unsafe {
        assert!(ll_release(keeper_member as *mut RcHeader));
        ll_object_die(keeper_member);
    }

    assert_eq!(
        DEATHS.load(Ordering::Relaxed),
        3,
        "and the survivor dies ordinarily once the kept reference goes"
    );
}

#[test]
fn a_survivor_the_release_decrements_is_registered_as_a_candidate() {
    let _g = test_guard();
    let plain = ClassBuilder::new("FinalizationUnregisteredPeer")
        .prop("next", true)
        .build();
    let keeper = ClassBuilder::new("FinalizationRegisteringKeeper")
        .prop("next", true)
        .destructor(keeping_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let peer = unsafe { new_constructed(&mut context, plain, MemoryCategory::GcHeap) };
    let keeper_member = unsafe { new_constructed(&mut context, keeper, MemoryCategory::GcHeap) };
    KEPT_MEMBER.store(0, Ordering::Relaxed);

    unsafe {
        // The fixture's own reference to the peer is moved into the keeper's
        // slot rather than copied: the peer takes no decrement at all, so its
        // `CANDIDATE_BIT` is clear where every other fixture's members carry
        // one, and the release below is the first decrement it ever sees.
        write_value_slot(
            Object::prop_at(keeper_member, prop_offset(0)),
            Value::entity(Tag::Object, peer as *mut RcHeader),
        );
        store_prop(&mut arena, peer, prop_offset(0), keeper_member);
        assert!(!ll_release(keeper_member as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(peer, &[peer, keeper_member]) };
    shadow_arena.reset();

    let mut finalization = Finalization::begin();
    let mut members = [peer as *mut RcHeader, keeper_member as *mut RcHeader];
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable
    );

    let mut pass = finalization.seal().destructors();
    unsafe { pass.run(&members) };
    let mut revalidation = pass.close();

    let _ = take_admissions();
    assert!(matches!(
        unsafe { revalidation.revalidate(&mut members) },
        Revalidated::ExternallyReferenced
    ));
    assert_eq!(
        take_admissions(),
        1,
        "the survivor whose registration bit was clear is registered again by \
         the counted release: registration is edge-triggered, and a decrement \
         that skipped the gate would leave the ring no later trace can propose"
    );
    revalidation.close();

    unsafe {
        assert!(!ll_release(keeper_member as *mut RcHeader));
        dismantle_ring(&mut arena, [peer, keeper_member]);
    }
}

#[test]
fn a_child_of_a_dying_member_runs_its_destructor_inside_the_release() {
    let _g = test_guard();
    let holder = ClassBuilder::new("FinalizationChildHolder")
        .prop("next", true)
        .prop("child", true)
        .dispose(counting_dispose as *const ())
        .build();
    let keeper = ClassBuilder::new("FinalizationChildKeeper")
        .prop("next", true)
        .dispose(counting_dispose as *const ())
        .destructor(keeping_and_releasing_destructor as *const ())
        .build();
    let child_class = ClassBuilder::new("FinalizationExternalChild")
        .prop("next", true)
        .destructor(child_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let ring = unsafe { unreachable_ring(&mut arena, [holder, keeper]) };
    let [holder_member, keeper_member] = ring;
    KEPT_MEMBER.store(0, Ordering::Relaxed);
    DEATHS.store(0, Ordering::Relaxed);
    CHILD_DESTRUCTOR_RUNS.store(0, Ordering::Relaxed);

    // The child stands outside the batch, which is what the design calls a
    // deferred external child: a child of a member that is no member itself
    // (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation",
    // step 6). Which commits hold one is the driver's partition, `PLAN.md`
    // S36.7's; the fixture builds the shape rather than deriving it.
    let mut context = LLContext { arena: &mut arena };
    let child = unsafe { new_constructed(&mut context, child_class, MemoryCategory::GcHeap) };
    unsafe {
        store_prop(&mut arena, holder_member, prop_offset(1), child);
        assert!(!ll_release(child as *mut RcHeader));
    }

    let mut finalization = Finalization::begin();
    let mut members = headers(&ring);
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable,
        "the child is a child rather than a holder, so it counts for neither side"
    );

    let mut pass = finalization.seal().destructors();
    unsafe { pass.run(&members) };
    assert_eq!(
        CHILD_DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        0,
        "the child is no member of the finalization, so the pass never reaches it"
    );

    let mut revalidation = pass.close();
    assert!(matches!(
        unsafe { revalidation.revalidate(&mut members) },
        Revalidated::ExternallyReferenced
    ));
    assert_eq!(
        CHILD_DESTRUCTOR_RUNS.load(Ordering::Relaxed),
        1,
        "the release takes the holder to zero, its teardown drops the child, \
         and the child's `__destruct` is user code running inside the \
         revalidation rather than inside the pass"
    );
    assert_eq!(
        DEATHS.load(Ordering::Relaxed),
        1,
        "and the member that ran it is the holder alone: the keeper carries \
         the reference its own destructor kept"
    );
    revalidation.close();

    unsafe {
        assert!(ll_release(keeper_member as *mut RcHeader));
        ll_object_die(keeper_member);
    }
}

/// The member a step-4 destructor published, as an address no count covers.
///
/// A weak cell created at step 4 is the channel the design names, and the cell
/// itself is not built here: creating one inside a destructor means reaching
/// the ambient arena from user code, which the fixtures of this tree do not
/// do. The static carries what such a cell would answer — the address of a
/// live member — and the destructor that reads it retains through the counted
/// path, exactly as `ll_weakref_get` does
/// (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", the two
/// consequences after step 6).
static PUBLISHED_MEMBER: AtomicUsize = AtomicUsize::new(0);

/// What the external child's destructor retained, held until the case releases
/// it.
static ROOTED_BY_THE_CHILD: AtomicUsize = AtomicUsize::new(0);

/// A destructor that publishes a member of its own component where the
/// component's counts do not name it.
unsafe extern "C" fn publishing_destructor(obj: *mut Object) {
    let slot = unsafe { Object::prop_at(obj, prop_offset(0)) };
    PUBLISHED_MEMBER.store(
        unsafe { entity_checked(&*slot) } as usize,
        Ordering::Relaxed,
    );
}

/// The external child's destructor: it takes what step 4 published and keeps a
/// counted reference to it.
unsafe extern "C" fn rooting_child_destructor(_obj: *mut Object) {
    let published = PUBLISHED_MEMBER.load(Ordering::Relaxed) as *mut RcHeader;
    assert!(!published.is_null(), "the fixture published a member");
    unsafe { ll_retain(published) };
    ROOTED_BY_THE_CHILD.store(published as usize, Ordering::Relaxed);
}

/// The teardown of one component roots a member of another, and the second
/// component's own reading is taken after it.
///
/// This is the order `dev/DECISIONS.md`, "the revalidation of a component and
/// its teardown are adjacent" exists for, and the only case that separates it
/// from the shape that reads every component before tearing any down.
#[test]
fn a_component_rooted_by_an_earlier_teardown_is_read_after_it() {
    let _g = test_guard();
    let holder = ClassBuilder::new("FinalizationAdjacencyHolder")
        .prop("next", true)
        .prop("child", true)
        .build();
    let keeper = ClassBuilder::new("FinalizationAdjacencyKeeper")
        .prop("next", true)
        .destructor(keeping_and_releasing_destructor as *const ())
        .build();
    let publisher = ClassBuilder::new("FinalizationAdjacencyPublisher")
        .prop("next", true)
        .destructor(publishing_destructor as *const ())
        .build();
    let plain = ClassBuilder::new("FinalizationAdjacencyPeer")
        .prop("next", true)
        .build();
    let child_class = ClassBuilder::new("FinalizationAdjacencyChild")
        .prop("next", true)
        .destructor(rooting_child_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let torn_down = unsafe { unreachable_ring(&mut arena, [holder, keeper]) };
    let read_later = unsafe { unreachable_ring(&mut arena, [publisher, plain]) };
    let [holder_member, keeper_member] = torn_down;
    let [_, peer] = read_later;
    KEPT_MEMBER.store(0, Ordering::Relaxed);
    PUBLISHED_MEMBER.store(0, Ordering::Relaxed);
    ROOTED_BY_THE_CHILD.store(0, Ordering::Relaxed);

    let mut context = LLContext { arena: &mut arena };
    let child = unsafe { new_constructed(&mut context, child_class, MemoryCategory::GcHeap) };
    unsafe {
        store_prop(&mut arena, holder_member, prop_offset(1), child);
        assert!(!ll_release(child as *mut RcHeader));
    }

    let mut finalization = Finalization::begin();
    let mut components = [headers(&torn_down), headers(&read_later)];
    for members in &mut components {
        assert_eq!(
            unsafe { finalization.confirm(members) },
            ValidationResult::Unreachable
        );
    }

    let mut pass = finalization.seal().destructors();
    for members in &components {
        unsafe { pass.run(members) };
    }
    assert_eq!(
        PUBLISHED_MEMBER.load(Ordering::Relaxed) as *mut Object,
        peer,
        "the second component's destructor published its neighbour, which no \
         count of that component names"
    );

    let mut revalidation = pass.close();
    assert!(matches!(
        unsafe { revalidation.revalidate(&mut components[0]) },
        Revalidated::ExternallyReferenced
    ));
    assert_eq!(
        ROOTED_BY_THE_CHILD.load(Ordering::Relaxed) as *mut Object,
        peer,
        "the first component's teardown ran its external child's destructor, \
         and that destructor rooted a member of the second"
    );
    assert!(
        matches!(
            unsafe { revalidation.revalidate(&mut components[1]) },
            Revalidated::ExternallyReferenced
        ),
        "the second component is read after the first is torn down, so the \
         reference that teardown created is in the count it reads; a \
         revalidation taken before it would free an entity a root holds"
    );
    revalidation.close();

    unsafe {
        assert!(!ll_release(peer as *mut RcHeader));
        assert!(ll_release(keeper_member as *mut RcHeader));
        ll_object_die(keeper_member);
        dismantle_ring(&mut arena, read_later);
    }
}
