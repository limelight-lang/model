//! Weak references (`rfc/model/weak-references.md`).
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
//! [`HAS_WEAK_REFERENCES`]. The table takes no lock because every
//! notification site runs on the owning thread — teardown, the drain in the
//! mutator's checkpoint, arena reset — and the collector thread never touches
//! it.
//!
//! Where the table's memory comes from, what a row holds and where a refused
//! growth is answered belong to the private `table` submodule.

mod table;

use crate::journal::kinds::journal_event;
use crate::memory::context::{LLContext, resolve_arena};
use crate::refcount::{
    EntityKind, HAS_WEAK_REFERENCES, MemoryCategory, RcHeader, ll_retain, mutator_flags,
    update_header_flags,
};

/// The `WeakReference` entity — [`EntityKind::WeakRef`], a class-less
/// singleton kind, and **the weak cell itself**. 16 bytes: header + the
/// referent, nulled by death notification. `get()` is a load, a null test
/// and a retain.
#[repr(C)]
pub struct LLWeakRef {
    pub rc: RcHeader,
    /// The referent; null once it died. Written only by the owning
    /// thread (creation and notification), so a plain field.
    pub target: *mut RcHeader,
}

/// Which entity kinds PHP admits as the referent of a `WeakReference`.
///
/// It answers for the same kinds as `refcount::carries_a_class_word`
/// today and is a predicate of its own because the two questions are
/// known to diverge: an `FFIBox` is a legal referent
/// (`rfc/model/weak-references.md`, "Death notification") and carries a C
/// payload at `+8` rather than a class, so the day the FFI surface lands,
/// widening this must not widen the other — a trace reading that payload
/// as a `*const Class` is the wild read `cells::trace_cells` dispatches on
/// the kind to avoid.
#[inline]
fn may_be_a_weak_referent(flags: u32) -> bool {
    crate::refcount::carries_a_class_word(flags)
}

/// Give this thread's weak table back at thread exit, after every death that
/// could still need a row.
///
/// No notification: the rows that remain name targets that are dying with this
/// thread's heap, and nothing outlives them to read a cell (cross-thread
/// movement is reserved). The call comes after the static-block teardown — the
/// one step of `ll_thread_exit` that runs user code, so the only one that can
/// still deliver a notification — and before the buffer arena, which a table
/// small enough to be a chunk of it has to reach while it is still mounted
/// (`heap::ll_thread_exit` fixes the order).
///
/// Null-tolerant and idempotent.
pub(crate) fn dispose() {
    table::dispose();
}

/// `WeakReference::create(target)`: return the canonical cell, creating
/// it on first use. The returned reference is retained for the caller.
///
/// **Null when memory refuses**, which is this entry point's out-of-memory
/// answer and costs the caller nothing to act on: the target keeps its flags,
/// the arena's weak log keeps its entries, and the table keeps every row it
/// held. A refusal of the cell after the table has already grown leaves the
/// larger table standing, which changes nothing a caller can read.
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
/// cell (`refcount::publish_header`).
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`]; `target` must
/// be a live entity of an object-bearing kind on this thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_weakref_create(
    ctx: *mut LLContext,
    target: *mut RcHeader,
) -> *mut LLWeakRef {
    let flags = unsafe { mutator_flags(target) };
    debug_assert!(
        may_be_a_weak_referent(flags),
        "a weak referent must be an object"
    );

    if flags & HAS_WEAK_REFERENCES != 0 {
        let cell = unsafe { table::find(table::current(), target as usize) };
        // Bit set ⇔ row exists, on this thread; a miss would mean the
        // invariant broke somewhere else. Recover by rebuilding in
        // release rather than handing out a null.
        debug_assert!(
            !cell.is_null(),
            "HAS_WEAK_REFERENCES set with no weak-table row"
        );
        if !cell.is_null() {
            unsafe { ll_retain(cell as *mut RcHeader) };
            return cell;
        }
    }

    // Every refusal this call can meet is taken here, before it holds
    // anything: the table's own growth first, the cell second. Past this point
    // the row's insert cannot fail.
    if table::ensure_room_for_one_more().is_null() {
        return std::ptr::null_mut();
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

    unsafe { table::insert(target as usize, cell) };
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
/// teardown and inside the drain. The caller has tested
/// `HAS_WEAK_REFERENCES`.
///
/// # Safety
/// `target` must be a live entity on its owning thread, with
/// `HAS_WEAK_REFERENCES` set.
pub(crate) unsafe fn notify_death(target: *mut RcHeader) {
    let cell = unsafe { table::remove(table::current(), target as usize) };
    debug_assert!(
        !cell.is_null(),
        "HAS_WEAK_REFERENCES set with no weak-table row"
    );
    if !cell.is_null() {
        unsafe { (*cell).target = std::ptr::null_mut() };
    }

    unsafe { update_header_flags(target, |f| f & !HAS_WEAK_REFERENCES) };
}

/// A collector's user destructor pass: null every member's cell **before any
/// user code runs** — the binding obligation of `rfc/model/gc/rc-cycle.md`,
/// "Cycle teardown", step 3 (a weak load is the one channel that can hand a
/// destructor a pointer counted references cannot account for). Irrevocable on
/// a later externally-referenced reading, by design. No collector calls it
/// today; S36.3 is where the next one does (`PLAN.md`).
///
/// # Safety
/// Members must be live entities on their owning thread.
#[expect(dead_code, reason = "the weak window of the cycle teardown is S36.3")]
pub(crate) unsafe fn notify_members(members: &[*mut RcHeader]) {
    for &m in members {
        if unsafe { mutator_flags(m) } & HAS_WEAK_REFERENCES != 0 {
            unsafe { notify_death(m) };
        }
    }
}

/// The `WeakRef` arm of the entity death switch: the last `$w` copy died.
/// A still-live target goes back to the cheap death path — its row is
/// removed and `HAS_WEAK_REFERENCES` cleared, so the next `create()`
/// builds a fresh canonical cell (observable via `spl_object_id`, and
/// exactly PHP's behaviour). A nulled target means the row died first;
/// nothing to do.
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
        let removed = unsafe { table::remove(table::current(), target as usize) };
        debug_assert_eq!(removed, cell, "the weak table row must be this cell");
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
/// died and was re-created leaves duplicates, deduplicated by
/// `HAS_WEAK_REFERENCES` going clear on the first notify.
///
/// **The notification runs inside the drain's own walk**, so it may not reach
/// the `Arena` at all — not a log, not a field, not a read. `drain_weak_log`
/// holds `&mut` on the arena for the whole walk and `reset_with` holds a
/// second above it, so a callback that resolved the ambient arena would alias
/// them. What it does touch is the entity's header, which is memory the arena
/// holds rather than the structure that describes it.
///
/// # Safety
/// `arena` must be mid-reset on its owning thread, destructors settled.
pub(crate) unsafe fn drain_arena_weak_log(arena: *mut crate::memory::arena::Arena) {
    unsafe {
        (*arena).drain_weak_log(|target| {
            let flags = mutator_flags(target);
            if MemoryCategory::from_flags(flags) == MemoryCategory::RequestArena
                && flags & HAS_WEAK_REFERENCES != 0
            {
                notify_death(target);
            }
        })
    };
}

#[cfg(test)]
mod tests;
