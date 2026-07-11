//! Arena death with promotion: the reset-time consumer of the
//! remembered set (`rfc/model/memory/arena-reset.md`).
//!
//! Phase 1 implements **retention only** — the safe default and,
//! per the RFC, the whole of the first implementation: no copying, no
//! identity machinery, no reference fixup. Sparse-block evacuation is
//! purely additive and lands later.
//!
//! The algorithm:
//!
//! 1. **Fixpoint** over the destructor and remembered-set logs: mark
//!    the escaped subgraph (conservatively — a slot may later be
//!    overwritten; over-approximation is safe, the cost is floating
//!    garbage, never a dangling pointer), then run pre-destructors of
//!    dying, unescaped objects. Destructors run PHP code: they may
//!    create new escapes and track new destructors, which start fresh
//!    log chains — hence the loop.
//! 2. **Count**: external references from the final state of every
//!    logged slot (deduplicated), internal edges between survivors,
//!    and one compensating retain per heap entity a survivor holds
//!    (its release-at-reset record assumed the holder would die; the
//!    survivor now owes its own release at its real death).
//! 3. **Retain blocks** carrying survivors: rewrite each survivor's
//!    category to GcHeap in place (the pointer-tag alternative was
//!    rejected exactly because this rewrite must be possible), stamp
//!    the blocks `BLOCK_KIND_RETAINED`, keep them out of the pool.
//! 4. Release-at-reset log: one release per record, with real teardown
//!    dispatch for entities that die of it.

use std::collections::HashSet;

use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_KIND_RETAINED, BlockHeader};
use crate::object::Object;
use crate::refcount::{
    ENTITY_OBJECT, ESCAPED, MEMORY_CATEGORY_MASK, MemoryCategory, RcHeader, ll_release, ll_retain,
};

/// Full arena death: fixpoint, promotion by retention, deferred
/// releases, blocks home. Replaces bare `Arena::reset` wherever the
/// object model is in play.
///
/// # Safety
/// The arena must not be reachable by running PHP code anymore (no
/// live stack); destructors invoked here may still allocate into it.
pub unsafe fn arena_reset_full(arena: &mut Arena) {
    let mut survivors: Vec<*mut RcHeader> = Vec::new();
    let mut slots: Vec<*mut *mut RcHeader> = Vec::new();
    let mut seen_slots: HashSet<usize> = HashSet::new();

    // --- 1. Fixpoint: mark escapes, destruct the dying ------------------
    loop {
        let mut progress = false;

        let mut round_slots = Vec::new();
        arena.drain_escapes(|s| round_slots.push(s));
        for slot in round_slots {
            progress = true;
            if !seen_slots.insert(slot as usize) {
                continue;
            }
            slots.push(slot);
            let target = unsafe { *slot };
            unsafe { mark_subgraph(target, &mut survivors) };
        }

        let mut round_dtors = Vec::new();
        arena.drain_destructors(|o| round_dtors.push(o));
        for obj in round_dtors {
            progress = true;
            if unsafe { (*obj).flags } & ESCAPED != 0 {
                continue; // escaped objects are not dying and are skipped
            }
            unsafe { crate::object::run_pre_destructor(obj as *mut Object) };
        }

        if !progress {
            break;
        }
    }

    // --- 2. Count: external slots, internal edges, compensations --------
    // Counts start from zero (mark_subgraph zeroed them): survivors
    // enter the counted world with exact reference counts.
    for &slot in &slots {
        let target = unsafe { *slot };
        if unsafe { is_arena_entity(target) } {
            debug_assert_ne!(unsafe { (*target).flags } & ESCAPED, 0);
            unsafe { (*target).refcount += 1 };
        }
    }
    for &surv in &survivors {
        unsafe { count_children(surv) };
    }

    // --- 3. Retention: rewrite categories, keep survivor blocks ---------
    let mut retained: HashSet<usize> = HashSet::new();
    for &surv in &survivors {
        unsafe {
            (*surv).flags &= !(MEMORY_CATEGORY_MASK | ESCAPED); // 00 = GcHeap
        }
        retained.insert(BlockHeader::of_ptr(surv as *const u8) as usize);
    }
    for &block in &retained {
        unsafe { (*(block as *mut BlockHeader)).kind = BLOCK_KIND_RETAINED };
    }

    // --- 4. Deferred releases, then memory ------------------------------
    arena.drain_release_log(|entity| unsafe {
        if ll_release(entity) {
            die(entity);
        }
    });
    arena.finish_reset(|block| retained.contains(&(block as usize)));
}

/// Entity teardown dispatch from a bare header (the Box tag is not
/// available here). Strings/arrays claim their own kind bits later.
unsafe fn die(entity: *mut RcHeader) {
    if unsafe { (*entity).flags } & ENTITY_OBJECT != 0 {
        unsafe { crate::object::ll_object_die(entity as *mut Object) };
    }
    // Non-object entities have no teardown yet.
}

#[inline]
unsafe fn is_arena_entity(p: *mut RcHeader) -> bool {
    !p.is_null() && unsafe { (*p).memory_category() } == MemoryCategory::RequestArena
}

/// Mark the escaped subgraph from one external reference: the target
/// and everything it references transitively inside the arena. Counts
/// are zeroed at first mark; counting happens in a later single pass.
unsafe fn mark_subgraph(root: *mut RcHeader, survivors: &mut Vec<*mut RcHeader>) {
    if !unsafe { is_arena_entity(root) } {
        return; // stale entry: overwritten or never an arena value
    }
    let mut stack = Vec::new();
    unsafe { mark_one(root, survivors, &mut stack) };

    while let Some(obj) = stack.pop() {
        for v in unsafe { crate::object::ref_child_values(obj) } {
            let child = v.entity_ptr();
            if unsafe { is_arena_entity(child) } {
                unsafe { mark_one(child, survivors, &mut stack) };
            }
        }
    }
}

unsafe fn mark_one(
    e: *mut RcHeader,
    survivors: &mut Vec<*mut RcHeader>,
    stack: &mut Vec<*mut Object>,
) {
    if unsafe { (*e).flags } & ESCAPED != 0 {
        return;
    }
    unsafe {
        (*e).flags |= ESCAPED;
        (*e).refcount = 0; // exact count rebuilt in the counting pass
    }
    survivors.push(e);
    if unsafe { (*e).flags } & ENTITY_OBJECT != 0 {
        stack.push(e as *mut Object);
    }
}

/// One counting pass over a survivor's reference slots: +1 to arena
/// children (internal edges), a compensating retain to heap entities
/// (their release-at-reset record no longer matches a dying holder).
unsafe fn count_children(surv: *mut RcHeader) {
    if unsafe { (*surv).flags } & ENTITY_OBJECT == 0 {
        return; // leaf entity: no reference slots
    }
    for v in unsafe { crate::object::ref_child_values(surv as *mut Object) } {
        let child = v.entity_ptr();
        if child.is_null() {
            continue;
        }
        match unsafe { (*child).memory_category() } {
            MemoryCategory::RequestArena => unsafe { (*child).refcount += 1 },
            MemoryCategory::GcHeap => unsafe { ll_retain(child) },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::memory::barrier::ref_store;
    use crate::memory::block_pool::BLOCK_KIND_ARENA;
    use crate::memory::context::{LLContext, set_current_context};
    use crate::object::{ll_object_die, ll_object_new};
    use crate::refcount::{DESTRUCTED, HAS_DESTRUCTOR};
    use crate::value::{Tag, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Entity pointer behind a Box slot, or null for scalar/null Boxes.
    fn entity_checked(v: &Value) -> *mut RcHeader {
        if v.is_refcounted() {
            v.entity_ptr()
        } else {
            std::ptr::null_mut()
        }
    }

    /// Store `value` into `holder`'s slot at `offset` through the real
    /// barrier, as generated code would.
    unsafe fn store_prop(arena: &mut Arena, holder: *mut Object, offset: u32, value: *mut Object) {
        unsafe {
            let slot = (*holder).prop_at(offset);
            let old = entity_checked(&*slot);
            ref_store(
                arena,
                holder as *mut RcHeader,
                (slot as *mut u8) as *mut *mut RcHeader,
                old,
                value as *mut RcHeader,
            );
            // The barrier wrote the payload; stamp the Box tag/flags.
            if !value.is_null() {
                slot.write(Value::entity(Tag::Object, value as *mut RcHeader));
            } else {
                slot.write(Value::null());
            }
        }
    }

    #[test]
    fn no_escapes_returns_every_block() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Temp").prop("x", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };
        let block = BlockHeader::of_ptr(obj as *const u8);

        unsafe { arena_reset_full(&mut arena) };

        // The block went home: a fresh arena must get it back.
        let mut second = Arena::new();
        let p = second.alloc(8);
        assert_eq!(BlockHeader::of_ptr(p), block);
        second.reset(|_| {});
    }

    #[test]
    fn escaped_object_survives_with_exact_count_and_retained_block() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Session").prop("x", true).build();
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { ll_object_new(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let obj = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe { store_prop(&mut arena, holder, 16, obj) };
        let block = BlockHeader::of_ptr(obj as *const u8);
        assert_eq!(unsafe { (*block).kind }, BLOCK_KIND_ARENA);

        unsafe { arena_reset_full(&mut arena) };

        let o = unsafe { &*obj };
        assert_eq!(
            o.rc.memory_category(),
            MemoryCategory::GcHeap,
            "recategorized in place"
        );
        assert_eq!(o.rc.refcount, 1, "exactly the one external reference");
        assert_eq!(o.rc.flags & ESCAPED, 0, "transient mark cleared");
        assert_eq!(unsafe { (*block).kind }, BLOCK_KIND_RETAINED);

        // The survivor is an ordinary counted object now: its one
        // reference is the holder's slot, so the holder's death
        // releases it and cascades into the survivor's own teardown.
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    #[test]
    fn internal_edges_survive_and_are_counted() {
        let _g = crate::memory::block_pool::test_guard();
        let node = ClassBuilder::new("Node").prop("next", true).build();
        let holder_cls = ClassBuilder::new("Root").prop("head", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { ll_object_new(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { ll_object_new(&mut ctx, node, MemoryCategory::RequestArena) };
        let b = unsafe { ll_object_new(&mut ctx, node, MemoryCategory::RequestArena) };

        unsafe {
            store_prop(&mut arena, a, 16, b); // arena→arena: no logs
            store_prop(&mut arena, holder, 16, a); // escape of a (and b transitively)
            arena_reset_full(&mut arena);
        }

        unsafe {
            assert_eq!((*a).rc.memory_category(), MemoryCategory::GcHeap);
            assert_eq!((*b).rc.memory_category(), MemoryCategory::GcHeap);
            assert_eq!((*a).rc.refcount, 1, "one external reference");
            assert_eq!((*b).rc.refcount, 1, "one internal edge from a");
        }
    }

    #[test]
    fn survivor_holding_heap_entity_compensates_the_release_log() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Keeper").prop("cfg", true).build();
        let holder_cls = ClassBuilder::new("Slot").prop("v", true).build();
        let cfg_cls = ClassBuilder::new("Config").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { ll_object_new(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let cfg = unsafe { ll_object_new(&mut ctx, cfg_cls, MemoryCategory::GcHeap) };
        let keeper = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe {
            // Heap entity into an arena container: retain + release log.
            store_prop(&mut arena, keeper, 16, cfg);
            assert_eq!((*cfg).rc.refcount, 2);
            // The keeper escapes.
            store_prop(&mut arena, holder, 16, keeper);
            arena_reset_full(&mut arena);
        }

        // Log's -1 and the survivor compensation +1 cancel out: the
        // keeper legitimately holds cfg.
        assert_eq!(unsafe { (*cfg).rc.refcount }, 2);

        // Keeper dies for real: phase 2 releases cfg.
        unsafe {
            assert!(crate::refcount::ll_release(keeper as *mut RcHeader));
            ll_object_die(keeper);
        }
        assert_eq!(
            unsafe { (*cfg).rc.refcount },
            1,
            "exactly one release at real death"
        );
    }

    #[test]
    fn heap_entity_of_a_dying_holder_dies_with_teardown() {
        let _g = crate::memory::block_pool::test_guard();
        static CFG_DTORS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn cfg_dtor(_o: *mut Object) {
            CFG_DTORS.fetch_add(1, Ordering::Relaxed);
        }

        let cfg_cls = ClassBuilder::new("DoomedCfg")
            .destructor(cfg_dtor as *const ())
            .build();
        let tmp_cls = ClassBuilder::new("Tmp").prop("cfg", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let cfg = unsafe { ll_object_new(&mut ctx, cfg_cls, MemoryCategory::GcHeap) };
        let tmp = unsafe { ll_object_new(&mut ctx, tmp_cls, MemoryCategory::RequestArena) };

        unsafe {
            store_prop(&mut arena, tmp, 16, cfg);
            // The test's own reference goes away: the arena holds the last one.
            assert!(!crate::refcount::ll_release(cfg as *mut RcHeader));
            arena_reset_full(&mut arena);
        }

        assert_eq!(
            CFG_DTORS.load(Ordering::Relaxed),
            1,
            "release log's last release must run real teardown"
        );
    }

    #[test]
    fn destructor_created_escape_survives_already_destructed() {
        let _g = crate::memory::block_pool::test_guard();
        static HOLDER_SLOT: AtomicUsize = AtomicUsize::new(0);
        static DTORS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn escaping_dtor(obj: *mut Object) {
            DTORS.fetch_add(1, Ordering::Relaxed);
            // `$GLOBALS['x'] = $this;` — through the real barrier, with
            // the TLS context (as generated destructor code would).
            let holder = HOLDER_SLOT.load(Ordering::Relaxed) as *mut Object;
            unsafe {
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                let slot = (*holder).prop_at(16);
                ref_store(
                    arena,
                    holder as *mut RcHeader,
                    (slot as *mut u8) as *mut *mut RcHeader,
                    std::ptr::null_mut(),
                    obj as *mut RcHeader,
                );
                slot.write(Value::entity(Tag::Object, obj as *mut RcHeader));
            }
        }

        let holder_cls = ClassBuilder::new("Globals").prop("x", true).build();
        let cls = ClassBuilder::new("LastWill")
            .destructor(escaping_dtor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        set_current_context(&mut ctx);

        let holder = unsafe { ll_object_new(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        HOLDER_SLOT.store(holder as usize, Ordering::Relaxed);
        let obj = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe { arena_reset_full(&mut arena) };
        set_current_context(std::ptr::null_mut());

        assert_eq!(DTORS.load(Ordering::Relaxed), 1);
        unsafe {
            assert_eq!(
                (*obj).rc.memory_category(),
                MemoryCategory::GcHeap,
                "the destructor-created escape was caught by the fixpoint"
            );
            assert_eq!((*obj).rc.refcount, 1);
            assert_ne!(
                (*obj).rc.flags & DESTRUCTED,
                0,
                "survives already-destructed"
            );
            assert_ne!((*obj).rc.flags & HAS_DESTRUCTOR, 0);
        }
    }

    #[test]
    fn overwritten_slot_is_stale_and_only_the_final_target_survives() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Val").build();
        let holder_cls = ClassBuilder::new("One").prop("v", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { ll_object_new(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };
        let b = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe {
            store_prop(&mut arena, holder, 16, a); // logged
            store_prop(&mut arena, holder, 16, b); // same slot, logged again
            arena_reset_full(&mut arena);
        }

        unsafe {
            assert_eq!((*b).rc.memory_category(), MemoryCategory::GcHeap);
            assert_eq!((*b).rc.refcount, 1, "deduplicated: one slot, one count");
            // `a` was conservatively marked but is unreferenced: floating
            // garbage of this reset, never a dangling pointer.
        }
    }
}
