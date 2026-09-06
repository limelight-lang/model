//! What a destructor of one member finds: a cell naming the other member
//! reads null, and a release of that member stops at its guard.
//!
//! Both are the state the finalization leaves behind before any user code
//! runs (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation",
//! steps 2 and 3). The pass that runs the destructors is `PLAN.md` S36.4's, so
//! each case here runs one by hand, on the member whose class carries it.

use super::*;

/// The cell the destructor loads, published before the destructor runs.
static PROBE_CELL: AtomicUsize = AtomicUsize::new(0);

/// What `get()` answered inside the destructor. `usize::MAX` while no
/// destructor has answered, which no entity address can be.
static SEEN_THROUGH_THE_CELL: AtomicUsize = AtomicUsize::new(usize::MAX);

/// The member the releasing destructor releases.
static RELEASED_MEMBER: AtomicUsize = AtomicUsize::new(0);

/// Whether that release read the member's count as zero. `true` until the
/// destructor answers, so a destructor that never ran fails the case.
static RELEASE_REACHED_ZERO: AtomicBool = AtomicBool::new(true);

/// A destructor that loads the cell naming the ring's other member.
unsafe extern "C" fn cell_probing_destructor(_obj: *mut Object) {
    let cell = PROBE_CELL.load(Ordering::Relaxed) as *mut LLWeakRef;
    let got = unsafe { ll_weakref_get(cell) };
    SEEN_THROUGH_THE_CELL.store(got as usize, Ordering::Relaxed);
    if !got.is_null() {
        // `get` retains what it resolves. The case asserts outside the
        // destructor, so the reference goes back here and the counts it reads
        // are the ring's own.
        let _ = unsafe { ll_release(got) };
    }
}

/// A destructor that releases the ring's other member through the counted
/// path, which is what user code does when it drops its last name for it.
unsafe extern "C" fn releasing_destructor(_obj: *mut Object) {
    let member = RELEASED_MEMBER.load(Ordering::Relaxed) as *mut RcHeader;
    RELEASE_REACHED_ZERO.store(unsafe { ll_release(member) }, Ordering::Relaxed);
}

/// Two objects linked into a ring nothing else holds, each at count one and
/// both read as unreachable by a trace that has released its rows.
///
/// # Safety
/// `arena` is this thread's, and both classes carry one Box property at
/// `prop_offset(0)`.
unsafe fn unreachable_ring(
    arena: &mut Arena,
    first_class: *const crate::class::Class,
    second_class: *const crate::class::Class,
    weak_cell_on_first: bool,
) -> (*mut Object, *mut Object, *mut LLWeakRef) {
    let mut context = LLContext { arena: &mut *arena };
    let first = unsafe { new_constructed(&mut context, first_class, MemoryCategory::GcHeap) };
    let second = unsafe { new_constructed(&mut context, second_class, MemoryCategory::GcHeap) };
    let cell = if weak_cell_on_first {
        let cell = unsafe { ll_weakref_create(&mut context, first as *mut RcHeader) };
        assert!(!cell.is_null(), "the fixture's weak cell");
        cell
    } else {
        std::ptr::null_mut()
    };

    unsafe {
        store_prop(arena, first, prop_offset(0), second);
        store_prop(arena, second, prop_offset(0), first);
        // From here the ring holds both entities and nothing else does.
        assert!(!ll_release(first as *mut RcHeader));
        assert!(!ll_release(second as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(first, &[first, second]) };
    shadow_arena.reset();
    (first, second, cell)
}

#[test]
fn a_destructor_reads_null_through_the_cell_naming_the_other_member() {
    let _g = test_guard();
    let named = ClassBuilder::new("FinalizationWeakTarget")
        .prop("next", true)
        .build();
    let prober = ClassBuilder::new("FinalizationCellProber")
        .prop("next", true)
        .destructor(cell_probing_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let (target, probe, cell) = unsafe { unreachable_ring(&mut arena, named, prober, true) };

    PROBE_CELL.store(cell as usize, Ordering::Relaxed);
    SEEN_THROUGH_THE_CELL.store(usize::MAX, Ordering::Relaxed);

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

    assert!(
        unsafe { run_user_destructor(probe) },
        "the fixture's destructor is the one this case reads"
    );
    assert_eq!(
        SEEN_THROUGH_THE_CELL.load(Ordering::Relaxed),
        0,
        "the cell naming the other member is null inside the destructor: \
         a resolving cell hands user code a reference to an entity the \
         teardown is about to free"
    );

    unsafe {
        assert!(ll_release(cell as *mut RcHeader));
        ll_entity_die(cell as *mut RcHeader);
        unwind_guarded_ring(&mut arena, [target, probe], &[]);
    }

    invalidated.guards_released();
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
        .destructor(cell_probing_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let (target, target_peer, cell) = unsafe { unreachable_ring(&mut arena, named, plain, true) };
    let (probe, probe_peer, _) = unsafe { unreachable_ring(&mut arena, prober, plain, false) };

    PROBE_CELL.store(cell as usize, Ordering::Relaxed);
    SEEN_THROUGH_THE_CELL.store(usize::MAX, Ordering::Relaxed);

    let ring_members = [target, target_peer, probe, probe_peer];
    let before = unsafe { refcounts(&ring_members) };
    let mut finalization = Finalization::begin();
    for component in [[target, target_peer], [probe, probe_peer]] {
        let mut members = [component[0] as *mut RcHeader, component[1] as *mut RcHeader];
        assert_eq!(
            unsafe { finalization.confirm(&mut members) },
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

    assert!(unsafe { run_user_destructor(probe) });
    assert_eq!(
        SEEN_THROUGH_THE_CELL.load(Ordering::Relaxed),
        0,
        "a cell naming a member of the other component is null too: the \
         invalidation covers the finalization rather than the component whose \
         destructor is running"
    );

    unsafe {
        assert!(ll_release(cell as *mut RcHeader));
        ll_entity_die(cell as *mut RcHeader);
        unwind_guarded_ring(&mut arena, [target, target_peer], &[]);
        unwind_guarded_ring(&mut arena, [probe, probe_peer], &[]);
    }

    invalidated.guards_released();
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
    let (target, probe, _) = unsafe { unreachable_ring(&mut arena, plain, releaser, false) };

    RELEASED_MEMBER.store(target as usize, Ordering::Relaxed);
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

    assert!(unsafe { run_user_destructor(probe) });
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
         having been spent by the destructor"
    );

    unsafe { unwind_guarded_ring(&mut arena, [target, probe], &[target]) };
    invalidated.guards_released();
}
