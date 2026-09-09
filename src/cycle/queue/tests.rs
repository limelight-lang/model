use super::*;

use crate::class::{Class, ClassBuilder};
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{
    ACYCLIC_GATE, CANDIDATE_BIT, EntityKind, MemoryCategory, ll_release, mutator_flags,
};

/// A header the candidate gate admits, at count `holders`: a heap object
/// with no clause of the gate against it, so a decrement that leaves a
/// holder behind reaches the registration.
///
/// A bare header rather than an allocated entity, which is what the
/// gate's own tests use and for the same reason: the registration path
/// dereferences no entry it writes. **The reader that does is
/// `cycle::mark`**, so a case whose poll can fire a collection over the
/// lane builds its candidates with [`allocated_candidate`] instead: the
/// trace resolves a root through the block header under its address, and
/// a header in a local has none (`cycle::row::resolve_edge_target`).
fn candidate(holders: u32) -> RcHeader {
    candidate_with(holders, 0)
}

/// The same, with one more flag set — a clause of the gate, for a test
/// that wants the decrement refused.
fn candidate_with(holders: u32, extra: u32) -> RcHeader {
    let mut header = RcHeader::new(
        MemoryCategory::GcHeap,
        EntityKind::Object.to_flags() | extra,
    );
    for _ in 1..holders {
        unsafe { crate::refcount::ll_retain(&raw mut header) };
    }

    header
}

/// The class every [`allocated_candidate`] of a case is built from: an
/// object with no property, which is all a root needs. The trace marks it,
/// finds no out-edge and scans it at the count the case left, so nothing
/// the collection does can reach the case's assertions about the lane.
fn candidate_class(name: &str) -> *const Class {
    ClassBuilder::new(name).build()
}

/// A candidate a collection can trace: a live GC-heap object of
/// `class` at count `holders`, in an entity block of this thread's heap.
///
/// The counterpart of [`candidate`] for a case that reaches
/// `ll_gc_maybe_collect` with the thread armed, which every case that
/// draws the critical reserve does — the draw is what arms it. Its caller
/// keeps the pointer this returns and reuses it, a fresh borrow of the
/// object being a retag that invalidates the one before it.
///
/// # Safety
/// `class` is a live class descriptor, and the entity is taken down with
/// [`dismantle_candidate`] before the case ends.
unsafe fn allocated_candidate(
    arena: &mut Arena,
    class: *const Class,
    holders: u32,
) -> *mut RcHeader {
    let mut context = LLContext { arena };
    let object = unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) };
    for _ in 1..holders {
        unsafe { crate::refcount::ll_retain(object as *mut RcHeader) };
    }

    object as *mut RcHeader
}

/// Take an [`allocated_candidate`] down: the last reference goes and the
/// object dies the ordinary death of an object with no child.
///
/// The candidate bit is still up, so `ll_free` withholds the slot and it
/// stays out of the allocator's hands for the rest of the process
/// (`memory::stdapi::ll_free`, the candidate arm). What the suite reads is
/// the state of the slot rather than its return: dead, so no census of a
/// later test counts it as a live entity.
///
/// # Safety
/// `entity` came from [`allocated_candidate`] and the caller holds its last
/// reference.
unsafe fn dismantle_candidate(entity: *mut RcHeader) {
    assert!(
        unsafe { ll_release(entity) },
        "the last reference to this candidate is the case's"
    );
    unsafe { ll_object_die(entity as *mut Object) };
}

/// Release once through the ABI, which is the only entry point registration
/// has.
///
/// **A raw pointer, and the caller keeps one per header and reuses it.**
/// A fresh `&mut` per call is a Unique retag that invalidates every raw
/// pointer taken before it, so a test that read the flags back through
/// one would be reading through a dead tag — a Miri failure of the
/// fixture rather than of the runtime (`dev/WORKFLOW.md`, Miri).
///
/// # Safety
/// `entity` points at a header this thread owns and outlives the call.
unsafe fn release(entity: *mut RcHeader) -> bool {
    unsafe { crate::refcount::ll_release(entity) }
}

/// Empty the queue and the spare cells, and give every block back.
///
/// Every test here starts and ends with it, because the queue is per
/// thread and the harness reuses threads: a segment another test left
/// live would put this one's first registration in a half-full segment, and
/// a spare it left held would answer this one's pool arithmetic.
fn reset() {
    release_queue_segments();
    crate::memory::critical::drain_for_test();
}

mod an_arena_entity_leaves_no_entry;
mod owner_retirement;
mod the_base_block_a_thread_holds_for_its_life;
mod the_batch_a_collection_detaches;
mod the_tokens_every_lane_holds;
mod what_a_registration_writes;
mod what_gc_owns;
mod what_the_poll_owes_the_queue;
mod where_a_full_segment_comes_from;
