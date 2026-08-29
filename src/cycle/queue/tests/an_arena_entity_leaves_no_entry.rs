//! Why the enrolment gate's first clause is the memory category, and
//! what would go wrong without it.
//!
//! A GC-heap slot that dies while an entry names it is withheld from the
//! allocator, which is what lets the entry stay a raw pointer
//! (`memory::stdapi::ll_free`). **An arena slot has no such door**: it is
//! reclaimed wholesale by `ll_arena_reset`, which returns the block to
//! the pool without passing a free, so nothing consults the enrolled bit
//! and nothing withholds anything. An entry naming an arena slot would
//! therefore survive into memory the next request is handed.
//!
//! The gate closes that by refusing to enrol anything outside category
//! zero (`refcount::ENROLMENT_GATE_MASK`, and
//! `rfc/model/gc/rc-cycle.md`, "Enrolment requires the GC-heap
//! category"). `the_enrolment_gate` proves the clause rejects; this
//! proves what the rejection is worth — the arrangement it prevents,
//! carried through to the reuse.

use super::*;

use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::new_constructed;
use crate::promote::arena_reset_full;
use crate::refcount::{RcHeader, ll_release, ll_retain};

/// An arena object losing one of two holders — the gate's own case in
/// every respect but the category — and the block it sits in, followed
/// through the reset that hands it to somebody else.
#[test]
fn an_arena_entity_leaves_no_entry_for_the_reset_to_strand() {
    let _g = test_guard();
    reset();
    assert!(
        replenish(),
        "the queue is funded, so a refused segment cannot stand in for the gate"
    );

    let cls = ClassBuilder::new("Temp").prop("x", true).build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
    let header = obj as *mut RcHeader;
    let block = BlockHeader::of_ptr(obj as *const u8);

    // **Shared on write, or the gate is never reached.** `release_word`
    // returns before the decrement for a non-zero category unless `COW`
    // is set — an arena entity is not counted, its cell being the
    // reset's — so an arena object without it would satisfy every
    // assertion below whether the gate asked about the category or not.
    // This is the construction `the_enrolment_gate` uses for the same
    // clause, on a real entity instead of a bare header.
    unsafe { crate::refcount::update_header_flags(header, |f| f | crate::refcount::COW) };
    unsafe { ll_retain(header) };
    assert!(
        !unsafe { ll_release(header) },
        "a decrement that leaves a holder is the gate's case, not a death"
    );
    assert_eq!(
        unsafe { crate::refcount::header_refcount(header) },
        1,
        "the decrement happened, so the gate was reached"
    );

    assert_eq!(enrolled_count(), 0, "no entry names the arena slot");
    assert_eq!(escrowed_count(), 0, "and none was parked instead");
    assert_eq!(
        unsafe { mutator_flags(header) } & ENROLLED,
        0,
        "and the bit that would have named one is down"
    );

    // The reset gives the block back whole, without passing a free — so
    // an entry, had one been written, would now be naming memory the
    // allocator owns.
    unsafe { arena_reset_full(&raw mut arena) };

    let mut second = Arena::new();
    let handed = second.alloc(8);
    assert_eq!(
        BlockHeader::of_ptr(handed),
        block,
        "the reset returned the block and a fresh arena was handed it"
    );
    second.reset(|_| {});

    assert_eq!(enrolled_count(), 0, "and the queue still names nothing");

    reset();
}
