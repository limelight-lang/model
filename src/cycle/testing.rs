//! What the collector's tests read out of a shadow row.
//!
//! Two test trees ask the same question — the mark's and the scan's —
//! and a second copy of the row lookup would be a second opinion about
//! where a row is. Test builds only.
//!
//! Beside the row readers stand the fixtures every case of the tree needs: a
//! ring of GC-heap objects ([`ring`]), the same read as potentially
//! unreachable ([`traced_unreachable_ring`]) and the teardown that takes one
//! apart ([`dismantle_ring`]). What a case attaches to a ring — an outside
//! holder, an external child, a weak cell, a destructor — stays in the case.
//!
//! The row readers own nothing, allocate nothing and order nothing: each reads
//! a row the caller's arena holds, through
//! [`arena::find_initialized_row`](crate::cycle::arena::find_initialized_row),
//! and answers a value. A row read after its arena reset is the one thing a
//! caller can do wrong, and it is the caller's to avoid
//! (`rfc/model/gc/rc-cycle.md`, "Concurrency"; the row layout it reads is
//! `crate::cycle::shadow`). Beside them stand [`open_arena`], which hands the
//! caller an arena to own, and [`traced_unreachable_from`], the trace a
//! fixture runs before it asks about a component.

use crate::class::Class;
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::row::{EdgeTarget, RowKey, resolve_edge_target};
use crate::cycle::scan::{ScanResult, scan};
use crate::cycle::shadow::{self, Color, RowArray};
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::test_support::{prop_offset, store_prop};

/// The row word the trace left for `entity`, read the way the scan
/// reads it — through the block's own shadow pointer. A meeting would
/// answer too, and would be the wrong instrument: it initialises a row
/// the trace never reached, so a test built on it cannot tell an
/// untouched row from a met one.
///
/// # Safety
/// `entity` is a live entity of the GC heap whose block this collection
/// has touched.
pub(crate) unsafe fn row_word(entity: *mut RcHeader) -> u32 {
    let EdgeTarget::Tracked(RowKey {
        block,
        index,
        population: _,
    }) = (unsafe { resolve_edge_target(entity) })
    else {
        panic!("the fixture's entity is not a GC-heap entity");
    };

    let array = unsafe { crate::memory::heap::block_shadow(block as *mut u8) } as *mut RowArray;
    assert!(!array.is_null(), "the trace touched this entity's block");
    unsafe { *shadow::row(array, index) }
}

/// The colour the trace left for `entity`: what the mark met, or the
/// verdict the scan wrote over it.
///
/// # Safety
/// As [`row_word`].
pub(crate) unsafe fn row_color(entity: *mut RcHeader) -> Color {
    shadow::color(unsafe { row_word(entity) })
}

/// An arena over this thread's workspace, for a case that means to have one.
///
/// The refusal is somebody else's subject. Every case that calls this runs
/// under `memory::block_pool::test_guard`, which draws the workspace before the
/// case begins, so a `None` here is the fixture failing rather than the path
/// under test — and one message for all of them keeps that reading in one
/// place.
pub(crate) fn open_arena() -> TraceScratchArena {
    TraceScratchArena::open().expect("the guard drew this thread's workspace")
}

/// Trace the fixture from one root and assert every entity named is
/// unreachable, which is the state the exact test is asked about.
///
/// The arena comes back so the caller resets it before validating: the rows
/// die at the token's release and the exact test runs after it
/// (`rfc/model/gc/rc-cycle.md`, "Concurrency").
///
/// # Safety
/// As `mark` and `scan`: `root` is an entity header of this thread's heap
/// whose slot is still its own, on the owning thread with no mutator beside
/// it.
pub(crate) unsafe fn traced_unreachable_from(
    root: *mut Object,
    expected: &[*mut Object],
) -> TraceScratchArena {
    let mut arena = open_arena();
    assert_eq!(
        unsafe { mark(&mut arena, root as *mut RcHeader) },
        MarkResult::Complete
    );
    assert_eq!(
        unsafe { scan(&mut arena, root as *mut RcHeader) },
        ScanResult::Complete
    );

    for &entity in expected {
        assert_eq!(
            unsafe { row_color(entity as *mut RcHeader) },
            Color::PotentiallyUnreachable,
            "the trace read this entity as potentially unreachable"
        );
    }

    arena
}

/// A ring of GC-heap objects, one per class given, each member naming the next
/// through property 0 and the last naming the first, with every creation
/// reference spent — so the ring is held by its own edges and by whatever the
/// caller adds afterwards.
///
/// What a case adds is its own: an outside holder, an external child at
/// another property, a weak cell, a destructor. The ring is what they have in
/// common and all this builds.
///
/// The trace is the caller's too, and [`traced_unreachable_ring`] is the one
/// most of them want. A case that reads rows of its own — a colour outside the
/// component, a working count after the mark alone — runs the phases itself
/// and takes this.
///
/// # Safety
/// The caller runs on a quiescent heap under `memory::block_pool::test_guard`,
/// and takes the ring apart through [`dismantle_ring`] or by hand.
pub(crate) unsafe fn ring<const MEMBERS: usize>(
    arena: &mut Arena,
    classes: [*const Class; MEMBERS],
) -> [*mut Object; MEMBERS] {
    let mut context = LLContext { arena: &mut *arena };
    let members = classes
        .map(|class| unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) });

    unsafe {
        for (index, &member) in members.iter().enumerate() {
            store_prop(
                arena,
                member,
                prop_offset(0),
                members[(index + 1) % MEMBERS],
            );
        }

        for &member in &members {
            assert!(!ll_release(member as *mut RcHeader));
        }
    }

    members
}

/// A [`ring`] the trace has read as potentially unreachable, with the scratch
/// arena reset behind it — the state an exact validation is asked about.
///
/// The reset is here because the rows die at the trace token's release and
/// everything that reads a row happens before it
/// (`rfc/model/gc/rc-cycle.md`, "Concurrency"); a case that wants a row after
/// the trace builds its ring with [`ring`] and runs the phases itself.
///
/// # Safety
/// As [`ring`].
pub(crate) unsafe fn traced_unreachable_ring<const MEMBERS: usize>(
    arena: &mut Arena,
    classes: [*const Class; MEMBERS],
) -> [*mut Object; MEMBERS] {
    let members = unsafe { ring(arena, classes) };
    let expected: Vec<*mut Object> = members.to_vec();
    unsafe { traced_unreachable(members[0], &expected) };
    members
}

/// Read `members` as potentially unreachable through a real trace and let the
/// rows go — the state a component reaches an exact validation in.
///
/// It is [`traced_unreachable_ring`]'s second half, taken alone by a case that
/// attaches something to its ring before the trace: an external child the
/// trace has to meet as a live external, a second ring naming the first.
///
/// # Safety
/// As [`traced_unreachable_from`].
pub(crate) unsafe fn traced_unreachable(root: *mut Object, members: &[*mut Object]) {
    let mut scratch = unsafe { traced_unreachable_from(root, members) };
    scratch.reset();
}

/// Break every edge of a ring nothing else holds and free its members.
///
/// The retain is what the sever below spends: a member whose edge is nulled
/// while its count is one dies inside `store_prop`'s barrier, under the loop
/// that is still walking the ring.
///
/// A member holding a child at another property needs no null store of its
/// own — the death path releases every cell the member still holds.
///
/// # Safety
/// Every member is a live object of this thread's GC heap, unguarded, linked
/// into a ring through property 0 and held by nothing else.
pub(crate) unsafe fn dismantle_ring<const MEMBERS: usize>(
    arena: &mut Arena,
    members: [*mut Object; MEMBERS],
) {
    unsafe {
        for member in members {
            ll_retain(member as *mut RcHeader);
        }

        for member in members {
            store_prop(arena, member, prop_offset(0), std::ptr::null_mut());
        }

        for member in members {
            assert!(ll_release(member as *mut RcHeader));
            ll_object_die(member);
        }
    }
}
