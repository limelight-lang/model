use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::refcount::{ll_release, ll_retain};
use crate::value::Tag;
use std::sync::atomic::{AtomicUsize, Ordering};

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

fn with_ctx<R>(f: impl FnOnce(*mut LLContext) -> R) -> R {
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let r = f(&mut ctx);
    arena.reset(|_| {});
    r
}

mod the_three_phases_of_a_death;
mod the_type_test;
mod what_the_factory_stamps;
mod who_owes_the_destructor;
