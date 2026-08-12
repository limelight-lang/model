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

static SEEN_BY_OWN_DESTRUCTOR: AtomicUsize = AtomicUsize::new(0);
static SEEN_BY_CHILD_DESTRUCTOR: AtomicUsize = AtomicUsize::new(usize::MAX);
static PROBE_CELL: AtomicUsize = AtomicUsize::new(0);

/// The dying object's own `__destruct`: `get()` must still produce it.
unsafe extern "C" fn probing_own_destructor(_obj: *mut Object) {
    let cell = PROBE_CELL.load(Ordering::Relaxed) as *mut LLWeakRef;
    let got = unsafe { ll_weakref_get(cell) };
    SEEN_BY_OWN_DESTRUCTOR.store(got as usize, Ordering::Relaxed);
    if !got.is_null() {
        assert!(
            !unsafe { ll_release(got) },
            "the object is alive mid-destructor"
        );
    }
}

/// A child's `__destruct`, running inside the parent's phase 2:
/// `get()` on the parent must already read null (the wrong order is
/// a use-after-free — `rfc/runtime/object-lifecycle.md`, phase 2).
unsafe extern "C" fn probing_child_destructor(_obj: *mut Object) {
    let cell = PROBE_CELL.load(Ordering::Relaxed) as *mut LLWeakRef;
    SEEN_BY_CHILD_DESTRUCTOR.store(unsafe { ll_weakref_get(cell) } as usize, Ordering::Relaxed);
}

static RESURRECTED_INTO: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
    unsafe { crate::refcount::ll_retain(obj as *mut RcHeader) };
    RESURRECTED_INTO.store(obj as usize, Ordering::Relaxed);
}

// --- Cycle collection (walk::collect_cycles, both configurations) ----

static CYCLE_DESTRUCTOR_SAW: AtomicUsize = AtomicUsize::new(usize::MAX);

unsafe extern "C" fn cycle_probing_destructor(_obj: *mut Object) {
    let cell = PROBE_CELL.load(Ordering::Relaxed) as *mut LLWeakRef;
    CYCLE_DESTRUCTOR_SAW.store(unsafe { ll_weakref_get(cell) } as usize, Ordering::Relaxed);
}

mod a_cell_that_dies_before_its_target;
mod across_the_arena_reset;
mod the_cell_and_the_table_behind_it;
mod when_the_notification_arrives;
