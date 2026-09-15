//! What the collector's tests read out of a shadow row.
//!
//! Two test trees ask the same question — the mark's and the scan's —
//! and a second copy of the row lookup would be a second opinion about
//! where a row is. Test builds only.
//!
//! Beside the row readers stand the fixtures every case of the tree needs: a
//! ring of GC-heap objects ([`ring`]), the same read as potentially
//! unreachable ([`traced_unreachable_ring`]), that reading taken alone
//! ([`read_as_unreachable`]) and the teardown that takes a ring apart
//! ([`dismantle_ring`]). What a case attaches to a ring — an outside holder,
//! an external child, a weak cell, a destructor — stays in the case.
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
//!
//! [`stamp_of`] and [`ages`] read a header rather than a row, and they are
//! here for the reason the row readers are: the maturation stamp has two test
//! trees, the commit that writes it and the descent that stops at it, and a
//! second copy of the read would be a second opinion about where the stamp
//! lives.
//!
//! [`on_a_fresh_thread`] is where a load runs: the measurement groups —
//! `density`'s, `mark`'s and `census`'s — each build a population eight
//! collections deep, and a heap no other case has touched is what makes the
//! first collection's reading the same on every run.
//!
//! [`ArmedInjection`] is the one shape every fault injection in the tree takes:
//! a thread-local flag armed for one firing and restored when the guard dies.

use crate::cells::PlainCells;
use crate::class::{Class, ClassBuilder};
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::row::{EdgeTarget, RowKey, resolve_edge_target};
use crate::cycle::scan::{ScanResult, scan};
use crate::cycle::shadow::{self, Color, RowArray};
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain, read_maturation_stamp};
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

/// The epoch and the age `entity`'s maturation stamp carries.
///
/// Read by both trees the stamp has: the commit that writes it
/// (`crate::cycle::maturation`) and the descent that stops at it
/// (`crate::cycle::mark`).
///
/// # Safety
/// `entity` is a live entity of this thread's GC heap.
pub(crate) unsafe fn stamp_of(entity: *mut Object) -> (u32, u32) {
    let stamp = unsafe { read_maturation_stamp(entity as *const RcHeader) };
    (stamp.epoch, stamp.age)
}

/// The ages `members` carry, in their order.
///
/// # Safety
/// As [`stamp_of`].
pub(crate) unsafe fn ages(members: &[*mut Object]) -> Vec<u32> {
    members
        .iter()
        .map(|&member| unsafe { stamp_of(member) }.1)
        .collect()
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
/// The arena comes back so the caller resets it before validating: on the
/// pressure path the rows have gone back before the exact test runs, and this
/// is the state such a case asks about (`rfc/model/gc/rc-cycle.md`,
/// "Concurrency").
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
        unsafe { mark::<PlainCells>(&mut arena, root as *mut RcHeader) },
        MarkResult::Complete
    );
    assert_eq!(
        unsafe { scan::<PlainCells>(&mut arena, root as *mut RcHeader) },
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

/// What a destructor does when it gives up its own edge to the next member:
/// the Box property at [`prop_offset`] 0 is emptied and the reference it held
/// released, which is the pair of acts a store of null through the barrier
/// performs over a slot whose owner and old value are both of the GC heap.
/// Answers the member the edge named and whether the release reached zero.
///
/// # Safety
/// `obj` is a live object whose property 0 holds a counted entity pointer.
pub(crate) unsafe fn release_own_edge(obj: *mut Object) -> (*mut RcHeader, bool) {
    let slot = unsafe { Object::prop_at(obj, prop_offset(0)) };
    let member = unsafe { crate::test_support::entity_checked(&*slot) };
    unsafe { crate::memory::barrier::write_value_slot(slot, crate::value::Value::null()) };
    (member, unsafe { ll_release(member) })
}

/// The row `ensure_row` handed back, or a panic naming what it answered
/// instead, for a case that asks for a row it expects to get.
pub(crate) fn met(answer: crate::cycle::arena::RowLookup) -> *mut u32 {
    match answer {
        crate::cycle::arena::RowLookup::Ready { row, .. } => row,
        other => panic!("the arena refused a row: {other:?}"),
    }
}

/// The members of a fixture as the header pointers a commit takes.
pub(crate) fn headers<const MEMBERS: usize>(
    members: [*mut Object; MEMBERS],
) -> [*mut RcHeader; MEMBERS] {
    members.map(|member| member as *mut RcHeader)
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
/// **The trace is the caller's.** [`traced_unreachable_ring`] is this followed
/// by one, and it is what a case takes when the ring is the whole graph. This
/// one is for the three that cannot use it: a case that attaches a child or a
/// chain the trace has to meet, and then calls [`read_as_unreachable`]; one
/// that runs the phases itself to read a row of its own; and one that traces
/// nothing at all.
///
/// **Every member comes back registered as a candidate.** The creation
/// references are spent through `ll_release`, so the gate admits each member
/// and leaves `CANDIDATE_BIT` up with one queue entry naming it — the state a
/// root of a real collection is in, and the reason a case whose subject is
/// that bit builds its ring itself.
///
/// # Safety
/// The caller runs on a quiescent heap under `memory::block_pool::test_guard`,
/// `arena` is this thread's, every class carries one Box property at
/// `prop_offset(0)`, and the ring is taken apart through [`dismantle_ring`] or
/// by hand.
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
            assert!(
                !ll_release(member as *mut RcHeader),
                "an edge of the ring holds this member"
            );
        }
    }

    members
}

/// A [`ring`] the trace has read as potentially unreachable, with the scratch
/// arena reset behind it — the state an exact validation is asked about.
///
/// The reset is here because the rows die at the window's close, which on
/// the pressure path is before the exact test
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
    unsafe { read_as_unreachable(members[0], &expected) };
    members
}

/// Read `members` as potentially unreachable through a real trace and let the
/// rows go — the state a component reaches an exact validation in.
///
/// It is [`traced_unreachable_ring`]'s second half, taken alone by a case that
/// builds more than a ring: an external child at another property, a chain
/// hanging off one member. Such a graph is traced after it is whole, and
/// `members` names the component the case will ask about rather than
/// everything the trace meets.
///
/// # Safety
/// As [`traced_unreachable_from`].
pub(crate) unsafe fn read_as_unreachable(root: *mut Object, members: &[*mut Object]) {
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
/// **The slots do not come back to the allocator.** The null store decrements
/// each member through the candidate gate, so a queue entry names it at its
/// free and `ll_free` withholds the slot until the entry is retired at a
/// collection's close or at thread exit (`cycle::queue::retire_candidates`).
/// A case that counts free slots is counting something else.
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

/// A value handed to another thread by a case that knows the pointee
/// outlives it — an entity pointer, or an array of them — because the owner
/// joins that thread before it touches the pointee again.
pub(crate) struct Sent<T>(pub(crate) T);

unsafe impl<T> Send for Sent<T> {}

impl<T> Sent<T> {
    /// The value, through a method so that a closure captures the wrapper
    /// rather than its field.
    pub(crate) fn into_inner(self) -> T {
        self.0
    }
}

/// Trace `root` from a second thread holding this thread's token, `traces`
/// times through the collector's reader, and answer what `read` found in
/// the rows of the last trace before they went back with the collector's
/// workspace. Returns once the collector holds the token, so the owner's
/// next free is under it.
///
/// With `wait_for` the collector holds the token and waits on it before its
/// first trace, for a case whose mutator half has to be running beside the
/// trace. The thread is the stand-in for the collector worker
/// (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff"): it takes the
/// owner's token, opens a workspace of its own and runs the two phases
/// through `cells::AtomicCells`.
///
/// # Safety
/// `root` is a candidate of this thread's heap, and nothing frees a storage
/// it reaches until this returns — an entity's death goes through `ll_free`,
/// which withholds the slot under the holder (`cycle::deferred_slot_reuse`).
pub(crate) unsafe fn traced_from_a_collector_thread<T: Send + 'static>(
    root: *mut RcHeader,
    traces: usize,
    wait_for: Option<std::sync::mpsc::Receiver<()>>,
    read: impl FnOnce() -> T + Send + 'static,
) -> std::thread::JoinHandle<T> {
    let token = crate::cycle::token::testing::Handed(crate::cycle::token::this_thread_token());
    let root = Sent(root);
    let (held_sender, held) = std::sync::mpsc::channel();
    let collector = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        let token = token.token();
        let root = root.into_inner();
        assert!(unsafe { (*token).try_take() }, "the owner was tracing");
        held_sender.send(()).expect("the owner waits for this");
        // Released on the unwind as well as on the return: a collector that
        // failed an assertion while holding the token would leave the
        // owner's exit waiting for it forever, and the case's own failure
        // would never be reported.
        struct ReleaseOnDrop(*const crate::cycle::token::TraceToken);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                unsafe { (*self.0).release() };
            }
        }
        let _held = ReleaseOnDrop(token);
        if let Some(wait_for) = wait_for {
            wait_for.recv().expect("the owner signalled");
        }

        let mut read = Some(read);
        let mut answer = None;
        for trace in 0..traces {
            let mut arena =
                TraceScratchArena::open().expect("the collector thread drew a workspace");
            assert_eq!(
                unsafe { mark::<crate::cells::AtomicCells>(&mut arena, root) },
                MarkResult::Complete
            );
            assert_eq!(
                unsafe { scan::<crate::cells::AtomicCells>(&mut arena, root) },
                ScanResult::Complete
            );
            if trace + 1 == traces {
                answer = Some((read.take().expect("read once"))());
            }

            arena.reset();
        }

        drop(_held);
        crate::cycle::queue::release_queue_segments();
        answer.expect("at least one trace ran")
    });
    held.recv().expect("the collector took the token");
    collector
}

/// The colours the last trace left for `entities`, in their order.
///
/// # Safety
/// As [`row_color`]: every entity's block was touched by the trace whose
/// rows are still standing.
pub(crate) unsafe fn colors(entities: &[*mut RcHeader]) -> Vec<Color> {
    entities
        .iter()
        .map(|&entity| unsafe { row_color(entity) })
        .collect()
}

/// A ring of three whose first member holds a second property, which is
/// where a case hangs its subject.
///
/// # Safety
/// As [`ring`].
pub(crate) unsafe fn ring_with_a_spare_property(arena: &mut Arena, name: &str) -> [*mut Object; 3] {
    let first = ClassBuilder::new(&format!("{name}First"))
        .prop("next", true)
        .prop("held", true)
        .build();
    let member = ClassBuilder::new(&format!("{name}Member"))
        .prop("next", true)
        .build();
    unsafe { ring(arena, [first, member, member]) }
}

/// Run `case` on a thread whose heap no other case has touched, and answer
/// what it returned.
///
/// The lane goes back unread at the end: the exit collects what it holds, and
/// a load's teardown leaves it naming slots it freed by hand.
pub(crate) fn on_a_fresh_thread<T: Send + 'static>(case: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served this thread"
        );
        let reading = case();
        crate::cycle::queue::release_queue_segments();
        reading
    })
    .join()
    .expect("the case finished")
}

/// A fault injection armed on one thread-local flag for a single firing —
/// the firing site reads the flag with `replace(false)` — and restored to what
/// it was when this guard dies, including on the unwind the injected panic
/// itself raises, so nothing of it reaches the next test on the thread.
///
/// The arming sites are the constructors beside each flag:
/// `arena::inject_reset_failure`, `arena::inject_harvest_failure`,
/// `arena::refuse_drop_reservation` and
/// `deferred_slot_reuse::inject_close_unwind`, each with what its firing
/// leaves half-done.
pub(crate) struct ArmedInjection {
    flag: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
    before: bool,
}

impl ArmedInjection {
    /// Arm `flag`, remembering what it held.
    pub(crate) fn arm(flag: &'static std::thread::LocalKey<std::cell::Cell<bool>>) -> Self {
        Self::hold(flag, true)
    }

    /// Hold `flag` at `value` for the guard's life, remembering what it held:
    /// the form a knob whose armed state is `false` takes.
    pub(crate) fn hold(
        flag: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
        value: bool,
    ) -> Self {
        Self {
            flag,
            before: flag.with(|armed| armed.replace(value)),
        }
    }
}

impl Drop for ArmedInjection {
    fn drop(&mut self) {
        self.flag.with(|armed| armed.set(self.before));
    }
}
