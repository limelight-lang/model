//! What the collector's tests read out of a shadow row.
//!
//! Two test trees ask the same question — the mark's and the scan's —
//! and a second copy of the row lookup would be a second opinion about
//! where a row is. Test builds only.
//!
//! The row readers own nothing, allocate nothing and order nothing: each reads
//! a row the caller's arena holds, through
//! [`arena::find_initialized_row`](crate::cycle::arena::find_initialized_row),
//! and answers a value. A row read after its arena reset is the one thing a
//! caller can do wrong, and it is the caller's to avoid
//! (`rfc/model/gc/rc-cycle.md`, "Concurrency"; the row layout it reads is
//! `crate::cycle::shadow`). Beside them stands [`open_arena`], which hands the
//! caller an arena to own.

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::row::{EdgeTarget, RowKey, resolve_edge_target};
use crate::cycle::shadow::{self, Color, RowArray};
use crate::refcount::RcHeader;

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
