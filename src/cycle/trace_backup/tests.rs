//! What the backup trace frees, what it keeps, where its verdict agrees with
//! trial deletion's, and when the poll runs one.
//!
//! Every case reads the fate of the entities it built and not a trace's
//! totals: the trace walks the thread's whole entity heap, adopted blocks
//! included, so a total can carry what an earlier case on another thread left
//! behind.

use super::*;
use crate::class::{Class, ClassBuilder};
use crate::cycle::testing::ring;
use crate::memory::arena::Arena;
use crate::memory::block_pool::test_guard;
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::refcount::{MemoryCategory, SlotState, ll_release, ll_retain, slot_state};
use crate::test_support::{prop_offset, store_prop};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

/// A class with one counted Box property at `prop_offset(0)`, which is what
/// [`ring`] links its members through, and the destructor the case wants, or
/// none for a null one.
fn node_class(name: &str, destructor: *const ()) -> *const Class {
    let mut builder = ClassBuilder::new(name).prop("next", true);
    if !destructor.is_null() {
        builder = builder.destructor(destructor);
    }

    builder.build()
}

/// Whether `object`'s slot still holds it.
fn live(object: *mut Object) -> bool {
    unsafe { slot_state(object as *mut RcHeader) == SlotState::Live }
}

/// One fresh GC-heap object of `class`, carrying its creation reference.
fn object(arena: &mut Arena, class: *const Class) -> *mut Object {
    let mut context = LLContext { arena: &mut *arena };
    unsafe { new_constructed(&mut context, class, MemoryCategory::GcHeap) }
}

/// Run one trace, which the gate of a case's own thread always admits.
fn one_trace() -> Trace {
    unsafe { trace() }.expect("a case's thread is outside every collection")
}

mod the_verdict_against_trial_deletion;
mod what_a_trace_keeps;
mod what_one_trace_frees;
mod what_the_sweep_frees;
mod when_the_poll_traces;
