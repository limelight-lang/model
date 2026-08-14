//! Weak references — rc-walk build step 4
//! (`rfc/model/weak-references.md`).
//!
//! The canonical `WeakReference` instance doubles as the shared weak
//! cell: PHP guarantees at most one instance per target, so every `$w`
//! copy points at the same 16-byte entity and death notification is one
//! store into its `target` field. The dying target finds its cell
//! through the per-thread **weak table** (target address → cell) — the
//! object itself carries only the [`HAS_WEAK_REFERENCES`] gate bit.
//!
//! Knowledge split: this module owns the cell, the table, and every
//! notification rule; teardown paths (`object::ll_default_dispose`, the
//! cycle collectors, arena reset) own only *when* to call in, gated by
//! bit 7. A plain `HashMap` under no lock is sound because every
//! notification site runs on the owning thread — teardown, the drain in
//! the mutator's checkpoint, arena reset — and the collector thread never
//! touches it.
//!
//! The row is a single canonical-cell pointer today; it widens to the
//! design's tagged subscriber list when `WeakMap` lands and maps start
//! subscribing (`rfc/model/weak-references.md`, "The weak table: address
//! → subscriber row").

use std::collections::HashMap;

use crate::journal::kinds::journal_event;
use crate::memory::context::{LLContext, resolve_arena};
use crate::refcount::{
    ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind, HAS_WEAK_REFERENCES, MemoryCategory, RcHeader,
    header_flags, ll_retain, update_header_flags,
};

/// The `WeakReference` entity — kind 5, class-less singleton kind, and
/// **the weak cell itself**. 16 bytes: header + the referent, nulled by
/// death notification. `get()` is a load, a null test and a retain.
#[repr(C)]
pub struct LLWeakRef {
    pub rc: RcHeader,
    /// The referent; null once it died. Written only by the owning
    /// thread (creation and notification), so a plain field.
    pub target: *mut RcHeader,
}

thread_local! {
    /// The weak table: target address → its canonical cell. Row exists
    /// iff the target's bit 7 is set iff a cell is live for it.
    ///
    /// Discarded at thread exit without notification — the thread's
    /// entities die with its heap, and nothing outlives them to read a
    /// cell (cross-thread movement is reserved). The disposal comes after
    /// the static-block teardown — the one step of `ll_thread_exit` that
    /// runs user code, so the only one that can still deliver a
    /// notification — and before the buffer arena and the heaps
    /// (`heap::ll_thread_exit` fixes the order).
    ///
    /// A `Cell<*mut _>` with no drop glue, freed by [`dispose`]
    /// (`dev/DECISIONS.md`, "thread exit owns the order its per-thread
    /// state dies in"). Its initializer is `const`, so a lazily
    /// initialized key cannot be first-initialized mid-destruction.
    static WEAK_TABLE: std::cell::Cell<*mut HashMap<usize, *mut LLWeakRef>> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// This thread's weak table, allocated on first use.
fn weak_table() -> *mut HashMap<usize, *mut LLWeakRef> {
    WEAK_TABLE.with(|cell| {
        let mut table = cell.get();
        if table.is_null() {
            table = Box::into_raw(Box::new(HashMap::new()));
            cell.set(table);
        }

        table
    })
}

/// Give this thread's weak table back at thread exit, after every death
/// that could still need a row (see the `WEAK_TABLE` doc).
///
/// No notification: the rows that remain name targets that are dying
/// with this thread's heap, and nothing outlives them to read a cell.
///
/// Null-tolerant and idempotent.
pub(crate) fn dispose() {
    let table = WEAK_TABLE.with(|cell| cell.replace(std::ptr::null_mut()));
    if !table.is_null() {
        unsafe { drop(Box::from_raw(table)) };
    }
}

/// `WeakReference::create(target)`: return the canonical cell, creating
/// it on first use. The returned reference is retained for the caller.
///
/// The cell is **always GC-heap memory**, wherever the target lives and
/// whichever arena is ambient: its refcount only counts in that
/// category, and an arena-allocated cell would die at reset under the
/// `$w` copies holding it (`rfc/model/weak-references.md`). A weak
/// reference to an arena-resident target additionally records the
/// target on the arena's weak log, so reset can null the cell before
/// the pages are reused.
///
/// Commissioning follows the factory contract: body first, header
/// published last, so an entity-block slot never reads a half-built
/// cell (`rfc/model/gc/rc-walk.md`, Phase 1).
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`]; `target` must
/// be a live entity of an object-bearing kind on this thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_weakref_create(
    ctx: *mut LLContext,
    target: *mut RcHeader,
) -> *mut LLWeakRef {
    let flags = unsafe { header_flags(target) };
    // PHP permits only objects as referents; in entity terms that is the
    // class-pointer-bearing kinds today (Box joins them with FFI).
    debug_assert!(
        matches!(
            (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT,
            k if k == EntityKind::Object as u32 || k == EntityKind::Lazy as u32
        ),
        "a weak referent must be an object"
    );

    if flags & HAS_WEAK_REFERENCES != 0 {
        let cell = unsafe { (*weak_table()).get(&(target as usize)).copied() };
        // Bit set ⇔ row exists, on this thread; a miss would mean the
        // invariant broke somewhere else. Recover by rebuilding in
        // release rather than handing out a null.
        debug_assert!(
            cell.is_some(),
            "HAS_WEAK_REFERENCES set with no weak-table row"
        );
        if let Some(cell) = cell {
            unsafe { ll_retain(cell as *mut RcHeader) };
            return cell;
        }
    }

    let mem = unsafe { crate::memory::heap::entity_alloc(size_of::<LLWeakRef>()) };
    if mem.is_null() {
        return std::ptr::null_mut();
    }

    let cell = mem as *mut LLWeakRef;
    unsafe {
        (*cell).target = target;
        crate::refcount::publish_header(
            cell as *mut RcHeader,
            RcHeader::new(MemoryCategory::GcHeap, EntityKind::WeakRef.to_flags()),
        );
    }

    unsafe { (*weak_table()).insert(target as usize, cell) };
    unsafe { update_header_flags(target, |f| f | HAS_WEAK_REFERENCES) };
    if MemoryCategory::from_flags(flags) == MemoryCategory::RequestArena {
        unsafe { (*resolve_arena(ctx)).log_weak(target) };
    }

    cell
}

/// `$w->get()`: the referent retained, or null once it died.
///
/// # Safety
/// `cell` must be a live weak cell on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_weakref_get(cell: *mut LLWeakRef) -> *mut RcHeader {
    let target = unsafe { (*cell).target };
    if !target.is_null() {
        // The caller receives a strong reference; without the retain the
        // target could die while the result is in the caller's hands.
        unsafe { ll_retain(target) };
    }

    target
}

/// Death notification (`rfc/model/weak-references.md`): null the cell,
/// drop the row, clear the gate bit. Runs no user code — safe inside
/// teardown and inside the drain. The caller has tested bit 7.
///
/// # Safety
/// `target` must be a live entity on its owning thread, bit 7 set.
pub(crate) unsafe fn notify_death(target: *mut RcHeader) {
    let cell = unsafe { (*weak_table()).remove(&(target as usize)) };
    debug_assert!(
        cell.is_some(),
        "HAS_WEAK_REFERENCES set with no weak-table row"
    );
    if let Some(cell) = cell {
        unsafe { (*cell).target = std::ptr::null_mut() };
    }

    unsafe { update_header_flags(target, |f| f & !HAS_WEAK_REFERENCES) };
}

/// The collectors' pre-destructor pass: null every member's cell
/// **before any user code runs** — the binding obligation of
/// `rfc/model/gc/rc-walk.md` (a weak load is the one channel that can
/// hand a destructor a pointer counted references cannot account for).
/// Irrevocable on a later acquittal, by design.
///
/// # Safety
/// Members must be live entities on their owning thread.
pub(crate) unsafe fn notify_members(members: &[*mut RcHeader]) {
    for &m in members {
        if unsafe { header_flags(m) } & HAS_WEAK_REFERENCES != 0 {
            unsafe { notify_death(m) };
        }
    }
}

/// Kind-5 arm of the entity death switch: the last `$w` copy died. A
/// still-live target goes back to the cheap death path — its row is
/// removed and bit 7 cleared, so the next `create()` builds a fresh
/// canonical cell (observable via `spl_object_id`, and exactly PHP's
/// behaviour). A nulled target means the row died first; nothing to do.
///
/// # Safety
/// `cell` must be a weak cell whose count just reached zero (or that a
/// collector owns).
pub(crate) unsafe fn weakref_die(cell: *mut LLWeakRef) {
    // A death a reset in flight must not walk past (`memory::reset_window`).
    crate::memory::reset_window::record_death(cell as *mut crate::refcount::RcHeader);
    journal_event!(
        crate::journal::kinds::KIND_ENTITY_DEATH,
        cell as u64,
        EntityKind::WeakRef as u64,
        0
    );
    let target = unsafe { (*cell).target };
    if !target.is_null() {
        let removed = unsafe { (*weak_table()).remove(&(target as usize)) };
        debug_assert_eq!(removed, Some(cell), "the weak table row must be this cell");
        unsafe { update_header_flags(target, |f| f & !HAS_WEAK_REFERENCES) };
    }

    // Always GcHeap by construction; the category read keeps the
    // teardown shape uniform with `reference_die`.
    if unsafe { crate::object::header_category(cell as *const RcHeader) } == MemoryCategory::GcHeap
    {
        unsafe { crate::memory::stdapi::ll_free(cell as *mut u8) };
    }
}

/// Arena reset's weak walk: drain the arena's weak log and null the
/// cells of entries that are actually dying with the pages. Ordered
/// **after** the destructor fixpoint (a tracked destructor's `get()` on
/// a fellow arena object must still see it alive) and **before** the
/// pages are reused. Runs no user code, so it cannot grow the logs.
///
/// Two kinds of stale entry are tolerated by the two tests: a promoted
/// survivor has its category rewritten off the arena (its cell stays
/// live — a weak reference to it must keep resolving), and a row that
/// died and was re-created leaves duplicates, deduplicated by bit 7
/// going clear on the first notify.
///
/// # Safety
/// `arena` must be mid-reset on its owning thread, destructors settled.
pub(crate) unsafe fn drain_arena_weak_log(arena: *mut crate::memory::arena::Arena) {
    let mut entries = Vec::new();
    unsafe { (*arena).drain_weak_log(|e| entries.push(e)) };
    for target in entries {
        let flags = unsafe { header_flags(target) };
        if MemoryCategory::from_flags(flags) == MemoryCategory::RequestArena
            && flags & HAS_WEAK_REFERENCES != 0
        {
            unsafe { notify_death(target) };
        }
    }
}

#[cfg(test)]
mod tests;
