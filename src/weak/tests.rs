use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::object::{Object, new_constructed};
use crate::refcount::{DESTRUCTOR_RAN, ll_release};
use crate::value::{Tag, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

fn with_ctx<R>(f: impl FnOnce(*mut LLContext) -> R) -> R {
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let r = f(&mut ctx);
    arena.reset(|_| {});
    r
}

// --- Notification ordering (rfc/model/weak-references.md) ------------

// --- Cycle collection (walk::collect_cycles, both configurations) ----

mod a_cell_that_dies_before_its_target;
mod across_the_arena_reset;
mod the_cell_and_the_table_behind_it;
mod when_the_notification_arrives;
