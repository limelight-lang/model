//! What a destructor of one member finds: a cell naming the other member
//! reads null, and a release of that member stops at its guard.
//!
//! Both are the state the finalization leaves behind before any user code
//! runs (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation",
//! steps 2 and 3). The destructors are the pass's, and the cases drive it the
//! way a teardown does — every member of every component through
//! `DestructorPass::run`, then each component read again.

use super::*;
use crate::memory::barrier::write_value_slot;
use crate::test_support::entity_checked;
use crate::value::Value;

/// The cell naming the first member of the ring, published before the
/// destructors run.
static FIRST_CELL: AtomicUsize = AtomicUsize::new(0);

/// The cell naming the second member.
static SECOND_CELL: AtomicUsize = AtomicUsize::new(0);

/// What `get()` answered inside the destructor of the second member, which
/// loads [`FIRST_CELL`]. `usize::MAX` while no destructor has answered, which
/// no entity address can be.
static SEEN_BY_THE_SECOND: AtomicUsize = AtomicUsize::new(usize::MAX);

/// What `get()` answered inside the destructor of the first member, which
/// loads [`SECOND_CELL`].
static SEEN_BY_THE_FIRST: AtomicUsize = AtomicUsize::new(usize::MAX);

/// The member the releasing destructor gave up, read out of the slot it
/// emptied.
static RELEASED_MEMBER: AtomicUsize = AtomicUsize::new(0);

/// Whether that release read the member's count as zero. `true` until the
/// destructor answers, so a destructor that never ran fails the case.
static RELEASE_REACHED_ZERO: AtomicBool = AtomicBool::new(true);

/// Load `cell` the way user code does and record what it resolved to.
///
/// `get` retains what it resolves. The cases assert outside the destructor, so
/// the reference goes back here and the counts they read are the ring's own.
fn read_through(cell: &AtomicUsize, seen: &AtomicUsize) {
    let cell = cell.load(Ordering::Relaxed) as *mut LLWeakRef;
    let got = unsafe { ll_weakref_get(cell) };
    seen.store(got as usize, Ordering::Relaxed);
    if !got.is_null() {
        let _ = unsafe { ll_release(got) };
    }
}

/// A destructor that loads the cell naming the ring's second member.
unsafe extern "C" fn first_member_destructor(_obj: *mut Object) {
    read_through(&SECOND_CELL, &SEEN_BY_THE_FIRST);
}

/// A destructor that loads the cell naming the ring's first member.
unsafe extern "C" fn second_member_destructor(_obj: *mut Object) {
    read_through(&FIRST_CELL, &SEEN_BY_THE_SECOND);
}

/// A destructor that gives up its own edge to the ring's other member: the
/// slot is emptied and the reference it held released, which is the pair of
/// acts a store of null through the barrier performs over a Box property whose
/// owner and old value are both of the GC heap.
unsafe extern "C" fn releasing_destructor(obj: *mut Object) {
    let slot = unsafe { Object::prop_at(obj, prop_offset(0)) };
    let member = unsafe { entity_checked(&*slot) };
    unsafe { write_value_slot(slot, Value::null()) };
    RELEASED_MEMBER.store(member as usize, Ordering::Relaxed);
    RELEASE_REACHED_ZERO.store(unsafe { ll_release(member) }, Ordering::Relaxed);
}

/// Two objects linked into a ring nothing else holds, each at count one and
/// both read as unreachable by a trace that has released its rows.
///
/// `weak_cells` says which member gets a weak cell of its own; a member
/// without one answers a null cell.
///
/// # Safety
/// `arena` is this thread's, and both classes carry one Box property at
/// `prop_offset(0)`.
unsafe fn unreachable_ring(
    arena: &mut Arena,
    first_class: *const crate::class::Class,
    second_class: *const crate::class::Class,
    weak_cells: [bool; 2],
) -> (*mut Object, *mut Object, [*mut LLWeakRef; 2]) {
    let mut context = LLContext { arena: &mut *arena };
    let first = unsafe { new_constructed(&mut context, first_class, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, second_class, MemoryCategory::GcHeap) };
    let mut cells = [std::ptr::null_mut(); 2];
    for (index, member) in [first, second].into_iter().enumerate() {
        if !weak_cells[index] {
            continue;
        }

        let cell = unsafe { ll_weakref_create(&mut context, member as *mut RcHeader) };
        assert!(!cell.is_null(), "the fixture's weak cell");
        cells[index] = cell;
    }

    unsafe {
        store_prop(arena, first, prop_offset(0), second);
        store_prop(arena, second, prop_offset(0), first);
        // From here the ring holds both entities and nothing else does.
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(first, &[first, second]) };
    shadow_arena.reset();
    (first, second, cells)
}

#[test]
fn a_destructor_reads_null_through_the_cell_naming_the_other_member() {
    let _g = test_guard();
    let named = ClassBuilder::new("FinalizationWeakTarget")
        .prop("next", true)
        .build();
    let prober = ClassBuilder::new("FinalizationCellProber")
        .prop("next", true)
        .destructor(second_member_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let (target, probe, cells) =
        unsafe { unreachable_ring(&mut arena, named, prober, [true, false]) };

    FIRST_CELL.store(cells[0] as usize, Ordering::Relaxed);
    SEEN_BY_THE_SECOND.store(usize::MAX, Ordering::Relaxed);

    let before = unsafe { refcounts(&[target, probe]) };
    let mut finalization = Finalization::begin();
    let mut members = [target as *mut RcHeader, probe as *mut RcHeader];
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable,
        "the ring is held by nothing outside it"
    );
    assert_eq!(
        unsafe { refcounts(&[target, probe]) },
        one_guard_each(&before),
        "every member of the component takes a guard reference"
    );
    let invalidated = finalization.seal();
    assert_eq!(invalidated.members(), 2);

    let mut pass = invalidated.destructors();
    unsafe { pass.run(&members) };
    assert_eq!(
        SEEN_BY_THE_SECOND.load(Ordering::Relaxed),
        0,
        "the cell naming the other member is null inside the destructor: \
         a resolving cell hands user code a reference to an entity the \
         teardown is about to free"
    );

    let mut revalidation = pass.close();
    let Revalidated::Unreachable(guarded) = (unsafe { revalidation.revalidate(&mut members) })
    else {
        panic!("a destructor that resolves nothing leaves the ring unreachable");
    };
    assert_eq!(guarded.members(), 2);

    unsafe {
        drop_cell(cells[0]);
        unwind_guarded_ring(&mut arena, [target, probe]);
    }

    unsafe { guarded.guards_released() };
    revalidation.close();
}

#[test]
fn a_destructor_of_one_component_reads_null_through_a_cell_naming_another() {
    let _g = test_guard();
    let named = ClassBuilder::new("FinalizationCrossWeakTarget")
        .prop("next", true)
        .build();
    let plain = ClassBuilder::new("FinalizationCrossNode")
        .prop("next", true)
        .build();
    let prober = ClassBuilder::new("FinalizationCrossProber")
        .prop("next", true)
        .destructor(second_member_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    // The prober's component is confirmed first, so that the cell its
    // destructor loads belongs to a component confirmed after it. A per-member
    // or per-component nulling would leave that cell resolving; only an
    // invalidation whole over the finalization nulls it in time.
    let (probe, probe_peer, _) =
        unsafe { unreachable_ring(&mut arena, prober, plain, [false, false]) };
    let (target, target_peer, cells) =
        unsafe { unreachable_ring(&mut arena, named, plain, [true, false]) };

    FIRST_CELL.store(cells[0] as usize, Ordering::Relaxed);
    SEEN_BY_THE_SECOND.store(usize::MAX, Ordering::Relaxed);

    let ring_members = [probe, probe_peer, target, target_peer];
    let before = unsafe { refcounts(&ring_members) };
    let mut finalization = Finalization::begin();
    let mut components = [
        [probe as *mut RcHeader, probe_peer as *mut RcHeader],
        [target as *mut RcHeader, target_peer as *mut RcHeader],
    ];
    for members in &mut components {
        assert_eq!(
            unsafe { finalization.confirm(members) },
            ValidationResult::Unreachable
        );
    }

    assert_eq!(
        unsafe { refcounts(&ring_members) },
        one_guard_each(&before),
        "every member of both components takes a guard reference"
    );
    let invalidated = finalization.seal();
    assert_eq!(
        invalidated.members(),
        4,
        "the finalization spans both components"
    );

    let mut pass = invalidated.destructors();
    for members in &components {
        unsafe { pass.run(members) };
    }

    assert_eq!(
        SEEN_BY_THE_SECOND.load(Ordering::Relaxed),
        0,
        "a cell naming a member of the other component is null too: the \
         invalidation covers the finalization rather than the component whose \
         destructor is running"
    );

    let mut revalidation = pass.close();
    unsafe { drop_cell(cells[0]) };
    for (index, members) in components.iter_mut().enumerate() {
        let Revalidated::Unreachable(guarded) = (unsafe { revalidation.revalidate(members) })
        else {
            panic!("neither component is held from outside");
        };

        let ring = if index == 0 {
            [probe, probe_peer]
        } else {
            [target, target_peer]
        };
        unsafe { unwind_guarded_ring(&mut arena, ring) };
        unsafe { guarded.guards_released() };
    }

    revalidation.close();
}

#[test]
fn every_destructor_of_the_finalization_reads_null_through_the_other_s_cell() {
    let _g = test_guard();
    let first_class = ClassBuilder::new("FinalizationPairedProberFirst")
        .prop("next", true)
        .destructor(first_member_destructor as *const ())
        .build();
    let second_class = ClassBuilder::new("FinalizationPairedProberSecond")
        .prop("next", true)
        .destructor(second_member_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let (first, second, cells) =
        unsafe { unreachable_ring(&mut arena, first_class, second_class, [true, true]) };

    FIRST_CELL.store(cells[0] as usize, Ordering::Relaxed);
    SECOND_CELL.store(cells[1] as usize, Ordering::Relaxed);
    SEEN_BY_THE_FIRST.store(usize::MAX, Ordering::Relaxed);
    SEEN_BY_THE_SECOND.store(usize::MAX, Ordering::Relaxed);

    let mut finalization = Finalization::begin();
    let mut members = [first as *mut RcHeader, second as *mut RcHeader];
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable
    );
    let invalidated = finalization.seal();

    let mut pass = invalidated.destructors();
    unsafe { pass.run(&members) };

    // Both members bear a cell and each destructor loads the other's, so
    // whichever of the two runs first meets a cell the invalidation had to
    // have nulled already. Nulling per member instead — this member's cell,
    // then this member's destructor — leaves the first one resolving, and the
    // order of the two is the sorted slice's rather than the fixture's.
    assert_eq!(
        SEEN_BY_THE_FIRST.load(Ordering::Relaxed),
        0,
        "the invalidation of the whole finalization precedes its first destructor"
    );
    assert_eq!(
        SEEN_BY_THE_SECOND.load(Ordering::Relaxed),
        0,
        "the invalidation of the whole finalization precedes its first destructor"
    );

    let mut revalidation = pass.close();
    let Revalidated::Unreachable(guarded) = (unsafe { revalidation.revalidate(&mut members) })
    else {
        panic!("a destructor that resolves nothing leaves the ring unreachable");
    };

    unsafe {
        drop_cell(cells[0]);
        drop_cell(cells[1]);
        unwind_guarded_ring(&mut arena, [first, second]);
    }

    unsafe { guarded.guards_released() };
    revalidation.close();
}

#[test]
fn a_release_inside_a_destructor_stops_at_the_other_member_s_guard() {
    let _g = test_guard();
    let plain = ClassBuilder::new("FinalizationGuardedNode")
        .prop("next", true)
        .build();
    let releaser = ClassBuilder::new("FinalizationReleasingProber")
        .prop("next", true)
        .destructor(releasing_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let (target, probe, _) =
        unsafe { unreachable_ring(&mut arena, plain, releaser, [false, false]) };

    RELEASED_MEMBER.store(0, Ordering::Relaxed);
    RELEASE_REACHED_ZERO.store(true, Ordering::Relaxed);

    let before = unsafe { refcounts(&[target, probe]) };
    let mut finalization = Finalization::begin();
    let mut members = [target as *mut RcHeader, probe as *mut RcHeader];
    assert_eq!(
        unsafe { finalization.confirm(&mut members) },
        ValidationResult::Unreachable
    );
    assert_eq!(
        unsafe { refcounts(&[target, probe]) },
        one_guard_each(&before),
        "every member of the component takes a guard reference"
    );
    let invalidated = finalization.seal();

    let mut pass = invalidated.destructors();
    unsafe { pass.run(&members) };
    assert_eq!(
        RELEASED_MEMBER.load(Ordering::Relaxed) as *mut RcHeader,
        target as *mut RcHeader,
        "the edge the destructor emptied is the one naming the other member"
    );
    assert!(
        !RELEASE_REACHED_ZERO.load(Ordering::Relaxed),
        "the release inside the destructor stops at the guard, which is what \
         keeps the member off the zero-count transition; what starts ordinary \
         teardown at zero is the store barrier a sever goes through, and no \
         case here drives one"
    );
    assert_eq!(
        unsafe { header_refcount(target as *mut RcHeader) },
        1,
        "what the released member carries is its guard, the ring's own edge \
         into it having been emptied and spent by the destructor"
    );

    let mut revalidation = pass.close();
    let Revalidated::Unreachable(guarded) = (unsafe { revalidation.revalidate(&mut members) })
    else {
        panic!(
            "the edge the destructor gave up was the component's own, not a reference from outside"
        );
    };

    unsafe { unwind_guarded_ring(&mut arena, [target, probe]) };
    unsafe { guarded.guards_released() };
    revalidation.close();
}
