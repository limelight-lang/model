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
//! bit 7. The table is per-thread and lock-free by construction: every
//! notification site runs on the owning thread — ordinary teardown, the
//! drain in the mutator's checkpoint, arena reset — and the collector
//! thread never touches it.
//!
//! The row is a single canonical-cell pointer today; it widens to the
//! design's tagged subscriber list when `WeakMap` lands and maps start
//! subscribing (`rfc/model/weak-references.md`, "The weak table").

use std::cell::RefCell;
use std::collections::HashMap;

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
    /// cell (cross-thread movement is reserved; the design doc pins the
    /// table's disposal last once static-block teardown exists).
    static WEAK_TABLE: RefCell<HashMap<usize, *mut LLWeakRef>> =
        RefCell::new(HashMap::new());
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
        let cell = WEAK_TABLE.with(|t| t.borrow().get(&(target as usize)).copied());
        // Bit set ⇔ row exists, on this thread; a miss would mean the
        // invariant broke somewhere else. Recover by rebuilding in
        // release rather than handing out a null.
        debug_assert!(cell.is_some(), "HAS_WEAK_REFERENCES set with no weak-table row");
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
    WEAK_TABLE.with(|t| t.borrow_mut().insert(target as usize, cell));
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
    let cell = WEAK_TABLE.with(|t| t.borrow_mut().remove(&(target as usize)));
    debug_assert!(cell.is_some(), "HAS_WEAK_REFERENCES set with no weak-table row");
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
    let target = unsafe { (*cell).target };
    if !target.is_null() {
        let removed = WEAK_TABLE.with(|t| t.borrow_mut().remove(&(target as usize)));
        debug_assert_eq!(removed, Some(cell), "the weak table row must be this cell");
        unsafe { update_header_flags(target, |f| f & !HAS_WEAK_REFERENCES) };
    }
    // Always GcHeap by construction; the category read keeps the
    // teardown shape uniform with `reference_die`.
    if unsafe { crate::object::header_category(cell as *const RcHeader) } == MemoryCategory::GcHeap {
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
mod tests {
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

    #[test]
    fn a_weak_cell_is_a_16_byte_kind_5_gc_heap_entity() {
        let _g = crate::memory::block_pool::test_guard();
        assert_eq!(size_of::<LLWeakRef>(), 16);
        assert_eq!(core::mem::offset_of!(LLWeakRef, rc), 0);
        assert_eq!(core::mem::offset_of!(LLWeakRef, target), 8);

        let cls = ClassBuilder::new("WeakTarget").build();
        with_ctx(|ctx| {
            let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
            let w = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };
            let rc = unsafe { &(*w).rc };
            assert_eq!(rc.refcount, 1);
            assert_eq!(rc.flags & ENTITY_KIND_MASK, EntityKind::WeakRef.to_flags());
            assert_eq!(
                rc.memory_category(),
                MemoryCategory::GcHeap,
                "the cell is GC-heap even when an arena is ambient"
            );
            assert_ne!(unsafe { (*obj).rc.flags } & HAS_WEAK_REFERENCES, 0);

            unsafe {
                assert!(ll_release(obj as *mut RcHeader));
                crate::object::ll_object_die(obj);
                assert!(ll_release(w as *mut RcHeader));
                crate::object::ll_entity_die(w as *mut RcHeader);
            }
        });
    }

    #[test]
    fn get_returns_the_target_retained_until_death_nulls_it() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("GetTarget").build();
        with_ctx(|ctx| {
            let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
            let w = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };

            let got = unsafe { ll_weakref_get(w) };
            assert_eq!(got, obj as *mut RcHeader);
            assert_eq!(unsafe { (*obj).rc.refcount }, 2, "get returned a strong reference");
            assert!(!unsafe { ll_release(got) });

            // Death notifies: the cell reads null, the flag is gone.
            assert!(unsafe { ll_release(obj as *mut RcHeader) });
            unsafe { crate::object::ll_object_die(obj) };
            assert!(unsafe { ll_weakref_get(w) }.is_null());

            unsafe {
                assert!(ll_release(w as *mut RcHeader));
                crate::object::ll_entity_die(w as *mut RcHeader);
            }
        });
    }

    #[test]
    fn create_is_canonical_while_the_cell_lives_and_fresh_after_it_dies() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Canonical").build();
        with_ctx(|ctx| {
            let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
            let w1 = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };
            let w2 = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };
            assert_eq!(w1, w2, "one canonical cell per live target");
            assert_eq!(unsafe { (*w1).rc.refcount }, 2);
            assert!(!unsafe { ll_release(w2 as *mut RcHeader) });

            // The last copy dies first: the target returns to the cheap
            // path and the next create() builds a fresh cell (PHP's
            // observable behaviour via spl_object_id).
            unsafe {
                assert!(ll_release(w1 as *mut RcHeader));
                crate::object::ll_entity_die(w1 as *mut RcHeader);
            }
            assert_eq!(unsafe { (*obj).rc.flags } & HAS_WEAK_REFERENCES, 0);
            let w3 = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };
            assert!(!w3.is_null());
            assert_eq!(unsafe { (*w3).target }, obj as *mut RcHeader);

            unsafe {
                assert!(ll_release(w3 as *mut RcHeader));
                crate::object::ll_entity_die(w3 as *mut RcHeader);
                assert!(ll_release(obj as *mut RcHeader));
                crate::object::ll_object_die(obj);
            }
        });
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
            assert!(!unsafe { ll_release(got) }, "the object is alive mid-destructor");
        }
    }

    /// A child's `__destruct`, running inside the parent's phase 2:
    /// `get()` on the parent must already read null (the wrong order is
    /// a use-after-free — `rfc/runtime/object-lifecycle.md`, phase 2).
    unsafe extern "C" fn probing_child_destructor(_obj: *mut Object) {
        let cell = PROBE_CELL.load(Ordering::Relaxed) as *mut LLWeakRef;
        SEEN_BY_CHILD_DESTRUCTOR.store(unsafe { ll_weakref_get(cell) } as usize, Ordering::Relaxed);
    }

    #[test]
    fn own_destructor_still_sees_the_object_but_a_child_destructor_sees_null() {
        let _g = crate::memory::block_pool::test_guard();
        let child_cls = ClassBuilder::new("WeakProbeChild")
            .destructor(probing_child_destructor as *const ())
            .build();
        let parent_cls = ClassBuilder::new("WeakProbeParent")
            .prop("child", true)
            .destructor(probing_own_destructor as *const ())
            .build();

        with_ctx(|ctx| {
            let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
            let parent = unsafe { new_constructed(ctx, parent_cls, MemoryCategory::GcHeap) };
            unsafe {
                Object::prop_at(parent, 16)
                    .write(Value::entity(Tag::Object, child as *mut RcHeader));
            }
            let w = unsafe { ll_weakref_create(ctx, parent as *mut RcHeader) };
            PROBE_CELL.store(w as usize, Ordering::Relaxed);
            SEEN_BY_OWN_DESTRUCTOR.store(0, Ordering::Relaxed);
            SEEN_BY_CHILD_DESTRUCTOR.store(usize::MAX, Ordering::Relaxed);

            assert!(unsafe { ll_release(parent as *mut RcHeader) });
            unsafe { crate::object::ll_object_die(parent) };

            assert_eq!(
                SEEN_BY_OWN_DESTRUCTOR.load(Ordering::Relaxed),
                parent as usize,
                "phase 1: the object's own __destruct still resolves itself"
            );
            assert_eq!(
                SEEN_BY_CHILD_DESTRUCTOR.load(Ordering::Relaxed),
                0,
                "phase 2: a cascading child __destruct must read null — \
                 anything else is a strong reference to a freed object"
            );

            unsafe {
                assert!(ll_release(w as *mut RcHeader));
                crate::object::ll_entity_die(w as *mut RcHeader);
            }
        });
    }

    static RESURRECTED_INTO: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
        unsafe { crate::refcount::ll_retain(obj as *mut RcHeader) };
        RESURRECTED_INTO.store(obj as usize, Ordering::Relaxed);
    }

    #[test]
    fn a_resurrected_object_keeps_its_weak_state() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("WeakLazarus")
            .destructor(resurrecting_destructor as *const ())
            .build();
        with_ctx(|ctx| {
            let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
            let w = unsafe { ll_weakref_create(ctx, obj as *mut RcHeader) };

            assert!(unsafe { ll_release(obj as *mut RcHeader) });
            unsafe { crate::object::ll_object_die(obj) };
            assert_ne!(unsafe { (*obj).rc.flags } & DESTRUCTOR_RAN, 0);
            assert_eq!(
                unsafe { ll_weakref_get(w) },
                obj as *mut RcHeader,
                "invalidation only after teardown commits: a resurrected \
                 object keeps resolving"
            );
            assert!(!unsafe { ll_release(obj as *mut RcHeader) }, "the get retained");

            // The second, final death nulls it.
            assert!(unsafe { ll_release(obj as *mut RcHeader) });
            unsafe { crate::object::ll_object_die(obj) };
            assert!(unsafe { ll_weakref_get(w) }.is_null());

            unsafe {
                assert!(ll_release(w as *mut RcHeader));
                crate::object::ll_entity_die(w as *mut RcHeader);
            }
        });
    }

    // --- Cycle collection (walk::collect_cycles, both configurations) ----

    static CYCLE_DESTRUCTOR_SAW: AtomicUsize = AtomicUsize::new(usize::MAX);

    unsafe extern "C" fn cycle_probing_destructor(_obj: *mut Object) {
        let cell = PROBE_CELL.load(Ordering::Relaxed) as *mut LLWeakRef;
        CYCLE_DESTRUCTOR_SAW.store(unsafe { ll_weakref_get(cell) } as usize, Ordering::Relaxed);
    }

    #[test]
    fn cyclic_death_nulls_the_cell_before_any_destructor_runs() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("WeakRing")
            .prop("next", true)
            .destructor(cycle_probing_destructor as *const ())
            .build();

        with_ctx(|ctx| {
            let a = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
            let b = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
            // Raw slot writes: each slot takes over the object's initial
            // reference, so the ring is pure garbage held only by itself.
            unsafe {
                Object::prop_at(a, 16).write(Value::entity(Tag::Object, b as *mut RcHeader));
                Object::prop_at(b, 16).write(Value::entity(Tag::Object, a as *mut RcHeader));
            }
            let w = unsafe { ll_weakref_create(ctx, a as *mut RcHeader) };
            PROBE_CELL.store(w as usize, Ordering::Relaxed);
            CYCLE_DESTRUCTOR_SAW.store(usize::MAX, Ordering::Relaxed);

            let stats = unsafe { crate::walk::collect_cycles() };
            assert_eq!(stats.collected, 2, "the ring was collected");
            assert_eq!(
                CYCLE_DESTRUCTOR_SAW.load(Ordering::Relaxed),
                0,
                "a member's __destruct must not be able to fish a condemned \
                 sibling out of a weak cell (the PEP 442 obligation)"
            );
            assert!(unsafe { ll_weakref_get(w) }.is_null());

            unsafe {
                assert!(ll_release(w as *mut RcHeader));
                crate::object::ll_entity_die(w as *mut RcHeader);
            }
        });
    }

    /// rc-trace frees its white set raw (no dispose), so its weak pass is
    /// a separate site from the walk drain and needs its own proof.
    #[cfg(not(feature = "rc-walk"))]
    #[test]
    fn rc_trace_cycle_collection_nulls_the_cell() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("WeakTraceRing").prop("next", true).build();

        /// Real store through the barrier: retain + whole-value write.
        unsafe fn link(arena: *mut Arena, from: *mut Object, to: *mut Object) {
            unsafe {
                crate::memory::barrier::ref_store(
                    arena,
                    from as *mut RcHeader,
                    Object::prop_at(from, 16),
                    std::ptr::null_mut(),
                    Value::entity(Tag::Object, to as *mut RcHeader),
                );
            }
        }

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        // Barrier stores (retain + slot write), then the variable
        // references die: the decrement-to-nonzero buffers the
        // candidates, exactly as generated code would.
        unsafe {
            let a = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
            let b = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
            link(&mut arena, a, b);
            link(&mut arena, b, a);
            let w = ll_weakref_create(&mut ctx, a as *mut RcHeader);

            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));
            assert_eq!(crate::gc::collect_cycles(), 2, "the ring was collected");
            assert!(ll_weakref_get(w).is_null());

            assert!(ll_release(w as *mut RcHeader));
            crate::object::ll_entity_die(w as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// The cell itself as cyclic garbage, its target alive outside: the
    /// dying cell must unregister (row removed, bit 7 cleared), or the
    /// live target's row keeps mapping to freed memory and the next
    /// `create()` returns a freed cell.
    #[test]
    fn a_cell_dying_inside_cyclic_garbage_unregisters_from_its_live_target() {
        let _g = crate::memory::block_pool::test_guard();
        let target_cls = ClassBuilder::new("WeakCellTarget").build();
        // Two bare-pointer slots: the pointer run starts at +16, so
        // "next" is at 16 and "w" at 24 — raw writes below rely on it.
        let holder_cls = ClassBuilder::new("WeakCellHolder")
            .prop_pointer("next")
            .prop_pointer("w")
            .build();

        with_ctx(|ctx| {
            let c = unsafe { new_constructed(ctx, target_cls, MemoryCategory::GcHeap) };
            let a = unsafe { new_constructed(ctx, holder_cls, MemoryCategory::GcHeap) };
            let b = unsafe { new_constructed(ctx, holder_cls, MemoryCategory::GcHeap) };
            // The ring, raw writes (slots take over the initial refs)...
            unsafe {
                ((a as *mut u8).add(16) as *mut *mut RcHeader).write(b as *mut RcHeader);
                ((b as *mut u8).add(16) as *mut *mut RcHeader).write(a as *mut RcHeader);
            }
            // ...and the cell, held ONLY by the ring: a's second slot
            // takes over the cell's initial reference.
            let w = unsafe { ll_weakref_create(ctx, c as *mut RcHeader) };
            unsafe {
                ((a as *mut u8).add(24) as *mut *mut RcHeader).write(w as *mut RcHeader);
            }

            let stats = unsafe { crate::walk::collect_cycles() };
            assert_eq!(stats.collected, 3, "ring + cell were collected");
            assert_eq!(
                unsafe { (*c).rc.flags } & HAS_WEAK_REFERENCES,
                0,
                "the dying cell unregistered from its live target"
            );
            // A fresh create must build a fresh, valid cell.
            let w2 = unsafe { ll_weakref_create(ctx, c as *mut RcHeader) };
            assert_eq!(unsafe { (*w2).target }, c as *mut RcHeader);
            assert_eq!(unsafe { (*w2).rc.refcount }, 1);

            unsafe {
                assert!(ll_release(w2 as *mut RcHeader));
                crate::object::ll_entity_die(w2 as *mut RcHeader);
                assert!(ll_release(c as *mut RcHeader));
                crate::object::ll_object_die(c);
            }
        });
    }

    /// The same shape through rc-trace, whose white-set free is a raw
    /// `ll_free` and needs its own kind-5 arm (a bypassed unregister is
    /// a use-after-free on the next `create()`).
    #[cfg(not(feature = "rc-walk"))]
    #[test]
    fn rc_trace_frees_a_white_cell_through_its_unregister_arm() {
        let _g = crate::memory::block_pool::test_guard();
        let target_cls = ClassBuilder::new("TraceCellTarget").build();
        let holder_cls = ClassBuilder::new("TraceCellHolder")
            .prop_pointer("next")
            .prop_pointer("w")
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        unsafe {
            let c = new_constructed(&mut ctx, target_cls, MemoryCategory::GcHeap);
            let a = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
            let b = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
            // Raw writes: each slot takes over an initial reference, so
            // the ring plus the cell are garbage the moment the buffered
            // release below hands them to the collector.
            ((a as *mut u8).add(16) as *mut *mut RcHeader).write(b as *mut RcHeader);
            ((b as *mut u8).add(16) as *mut *mut RcHeader).write(a as *mut RcHeader);
            let w = ll_weakref_create(&mut ctx, c as *mut RcHeader);
            ((a as *mut u8).add(24) as *mut *mut RcHeader).write(w as *mut RcHeader);

            // A transient reference buffers `a` as a candidate root, as
            // any decrement-to-nonzero from generated code would.
            crate::refcount::ll_retain(a as *mut RcHeader);
            assert!(!ll_release(a as *mut RcHeader));
            assert_eq!(crate::gc::collect_cycles(), 3, "ring + cell collected");
            assert_eq!(
                (*c).rc.flags & HAS_WEAK_REFERENCES,
                0,
                "the white cell must unregister before its memory is freed"
            );
            let w2 = ll_weakref_create(&mut ctx, c as *mut RcHeader);
            assert_eq!((*w2).target, c as *mut RcHeader);
            assert_eq!((*w2).rc.refcount, 1, "a fresh cell, not the freed one");

            assert!(ll_release(w2 as *mut RcHeader));
            crate::object::ll_entity_die(w2 as *mut RcHeader);
            assert!(ll_release(c as *mut RcHeader));
            crate::object::ll_object_die(c);
        }
        arena.reset(|_| {});
    }

    // --- Arena reset ------------------------------------------------------

    #[test]
    fn arena_reset_nulls_cells_of_dying_arena_targets() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("WeakArenaTarget").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        let w = unsafe { ll_weakref_create(&mut ctx, obj as *mut RcHeader) };
        assert_eq!(unsafe { ll_weakref_get(w) }, obj as *mut RcHeader);
        assert!(!unsafe { ll_release(obj as *mut RcHeader) }, "arena objects are not counted");

        arena.reset(|_| {});
        assert!(
            unsafe { ll_weakref_get(w) }.is_null(),
            "the pages are gone; a stale cell would be a dangling read"
        );

        unsafe {
            assert!(ll_release(w as *mut RcHeader));
            crate::object::ll_entity_die(w as *mut RcHeader);
        }
    }

    #[test]
    fn a_promoted_survivor_keeps_its_weak_state_across_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("WeakEscapee").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        let w = unsafe { ll_weakref_create(&mut ctx, obj as *mut RcHeader) };

        // A longer-lived holder takes the object: hold-count 1, logged —
        // the store barrier's escape protocol, done by hand here.
        unsafe {
            (*obj).rc.flags |= crate::refcount::IS_ESCAPEE;
            (*obj).rc.refcount = 1;
            arena.log_escapee(obj as *mut RcHeader);
        }

        unsafe { crate::promote::arena_reset_full(&mut arena) };

        assert_eq!(
            unsafe { (*obj).rc.memory_category() },
            MemoryCategory::GcHeap,
            "the escapee was promoted in place"
        );
        assert_eq!(
            unsafe { ll_weakref_get(w) },
            obj as *mut RcHeader,
            "a promoted survivor is alive — reset must not null its cell"
        );
        assert!(!unsafe { ll_release(obj as *mut RcHeader) }, "the get retained");

        // The promoted object now dies the ordinary counted death.
        assert!(unsafe { ll_release(obj as *mut RcHeader) });
        unsafe { crate::object::ll_object_die(obj) };
        assert!(unsafe { ll_weakref_get(w) }.is_null());

        unsafe {
            assert!(ll_release(w as *mut RcHeader));
            crate::object::ll_entity_die(w as *mut RcHeader);
        }
    }
}
