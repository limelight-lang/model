use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::refcount::{ll_release, ll_retain};
use crate::value::Tag;
use std::sync::atomic::{AtomicUsize, Ordering};

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
static RESURRECT_INTO: AtomicUsize = AtomicUsize::new(0);
static TRANSIENT_DEATHS: AtomicUsize = AtomicUsize::new(0);
static DISPOSE_DISPATCHED: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// A stand-in for a compiler-generated specialized `dispose`: it marks
/// that the descriptor's pointer was dispatched to, then delegates the
/// real teardown to the default so the effects are unchanged.
unsafe extern "C" fn counting_dispose(obj: *mut Object) -> bool {
    DISPOSE_DISPATCHED.fetch_add(1, Ordering::Relaxed);
    unsafe { ll_default_dispose(obj) }
}

unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    unsafe { ll_retain(obj as *mut RcHeader) };
    RESURRECT_INTO.store(obj as usize, Ordering::Relaxed);
}

/// `$x = $this;` then `$x` leaves scope: a transient retain + release.
/// Under the destructor guard the release must NOT report death — a
/// reported death here re-enters teardown and double-frees `obj`.
unsafe extern "C" fn transient_this_destructor(obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    unsafe { ll_retain(obj as *mut RcHeader) };
    if unsafe { ll_release(obj as *mut RcHeader) } {
        TRANSIENT_DEATHS.fetch_add(1, Ordering::Relaxed);
    }
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
