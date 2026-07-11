//! GC strategies: the fixed contract, and `rc-trace` — the default.
//!
//! Per `rfc/model/gc/strategies.md` there is no universal GC: the
//! collector is a pluggable strategy behind a fixed four-interface
//! contract, selected at build time. [`GcStrategy`] *is* that contract
//! in Rust-trait form; strategies implement it, nothing else in the
//! runtime knows which one is active. This crate's build corresponds
//! to the `rc-trace` composition (ARC + arenas + stop-the-thread cycle
//! tracing); a NoGC or pure-RC build compiles the candidate buffering
//! away — modeled here by the trivial [`NoGc`] / [`PureRc`] impls.
//!
//! The `rc-trace` cycle collector is Bacon–Rajan synchronous trial
//! deletion (the Zend architecture done right): candidate roots are
//! buffered on non-zero decrements, `mark_gray` trial-deletes internal
//! edges, `scan` restores externally-reachable subgraphs, white nodes
//! are cyclic garbage. Colors live in the header bits reserved for
//! exactly this (flags 4-5 + buffered bit 6). MMTK, when it arrives,
//! plugs in as just another implementation — never special-cased.
//!
//! **Known phase-1 limit**: `__destruct` of cyclically-dead objects is
//! not run (the counts are already trial-mutated when whites are
//! known; running arbitrary PHP there needs the Zend-style re-scan
//! discipline). Logged in PLAN.md; memory safety is unaffected.

use std::cell::RefCell;

use crate::object::Object;
use crate::refcount::{
    CYCLE_COLLECTOR_BUFFERED, CYCLE_COLLECTOR_COLOR_SHIFT, ENTITY_OBJECT, MemoryCategory, RcHeader,
};
use crate::value::Value;

/// The strategy contract (`rfc/model/gc/strategies.md`). Selection is
/// a build-time decision; hot paths of the selected strategy are
/// expected to inline (no dynamic dispatch anywhere in the runtime).
pub trait GcStrategy {
    const NAME: &'static str;
    /// §1: does this strategy contribute a hook to `ll_ref_store`?
    /// (SATB's deletion barrier will; nothing else does.)
    const HAS_STORE_HOOK: bool = false;
    /// §2: does this strategy need poll safepoints? (Concurrent
    /// marking only; `rc-trace` parks the mutator instead.)
    const NEEDS_SAFEPOINTS: bool = false;

    /// §3: the strategy owns the `GcHeap` allocator. Arenas allocate
    /// independently and are invisible to it.
    fn heap_alloc(&mut self, size: usize) -> *mut u8;

    /// § 3, releasing side.
    ///
    /// # Safety
    /// `ptr` must be a live `heap_alloc` allocation of this strategy.
    unsafe fn heap_free(&mut self, ptr: *mut u8);

    /// A non-zero decrement happened on a heap object: a possible
    /// cycle root (§4 consumes object metadata to trace from it).
    ///
    /// # Safety
    /// `entity` must be live.
    unsafe fn buffer_candidate(&mut self, _entity: *mut RcHeader) {}

    /// Run a collection cycle; returns entities reclaimed.
    fn collect(&mut self) -> usize {
        0
    }
}

/// `nogc`: bump allocation, never frees. Benchmark baseline.
pub struct NoGc;
impl GcStrategy for NoGc {
    const NAME: &'static str = "nogc";
    fn heap_alloc(&mut self, size: usize) -> *mut u8 {
        unsafe { crate::memory::stdapi::ll_alloc(size, 16) }
    }
    unsafe fn heap_free(&mut self, _ptr: *mut u8) {} // never frees
}

/// `rc`: ARC + arenas, leaks cycles. Approximately elephc's model.
pub struct PureRc;
impl GcStrategy for PureRc {
    const NAME: &'static str = "rc";
    fn heap_alloc(&mut self, size: usize) -> *mut u8 {
        unsafe { crate::memory::stdapi::ll_alloc(size, 16) }
    }
    unsafe fn heap_free(&mut self, ptr: *mut u8) {
        unsafe { crate::memory::stdapi::ll_free(ptr) }
    }
}

/// `rc-trace` (the default): ARC is the primary reclamation path,
/// arenas absorb the bulk, stop-the-thread tracing collects cycles
/// only. The candidate buffer and collector below are its machinery.
pub struct RcTrace;
impl GcStrategy for RcTrace {
    const NAME: &'static str = "rc-trace";
    fn heap_alloc(&mut self, size: usize) -> *mut u8 {
        unsafe { crate::memory::stdapi::ll_alloc(size, 16) }
    }
    unsafe fn heap_free(&mut self, ptr: *mut u8) {
        unsafe { crate::memory::stdapi::ll_free(ptr) }
    }
    unsafe fn buffer_candidate(&mut self, entity: *mut RcHeader) {
        unsafe { buffer_candidate(entity) }
    }
    fn collect(&mut self) -> usize {
        unsafe { collect_cycles() }
    }
}

// --- rc-trace machinery ----------------------------------------------------

/// Candidate-root buffer fill that triggers a collection (Zend uses
/// 10K; to be calibrated, PLAN.md).
pub const CANDIDATE_THRESHOLD: usize = 10_000;

thread_local! {
    static CANDIDATES: RefCell<Vec<*mut RcHeader>> = const { RefCell::new(Vec::new()) };
}

const COLOR_BLACK: u32 = 0; // in use / definitely live (default)
const COLOR_GRAY: u32 = 1; // trial-deleted
const COLOR_WHITE: u32 = 2; // cyclic garbage
const COLOR_MASK: u32 = 0b11 << CYCLE_COLLECTOR_COLOR_SHIFT;

#[inline]
unsafe fn color(e: *mut RcHeader) -> u32 {
    (unsafe { (*e).flags } & COLOR_MASK) >> CYCLE_COLLECTOR_COLOR_SHIFT
}

#[inline]
unsafe fn set_color(e: *mut RcHeader, c: u32) {
    unsafe { (*e).flags = ((*e).flags & !COLOR_MASK) | (c << CYCLE_COLLECTOR_COLOR_SHIFT) };
}

/// Heap-object children of a heap entity: the tracer's edge set. Only
/// the general heap is traced — arenas and immortals are invisible to
/// every strategy by contract.
unsafe fn heap_children(e: *mut RcHeader) -> Vec<*mut RcHeader> {
    if unsafe { (*e).flags } & ENTITY_OBJECT == 0 {
        return Vec::new();
    }
    unsafe { crate::object::ref_child_values(e as *mut Object) }
        .iter()
        .map(Value::entity_ptr)
        .filter(|&c| unsafe { (*c).memory_category() } == MemoryCategory::GcHeap)
        .collect()
}

/// Called by `ll_release` on a non-zero decrement of a heap object:
/// buffer it once as a possible cycle root.
///
/// # Safety
/// `entity` must be live.
pub(crate) unsafe fn buffer_candidate(entity: *mut RcHeader) {
    if unsafe { (*entity).flags } & CYCLE_COLLECTOR_BUFFERED != 0 {
        return;
    }
    unsafe { (*entity).flags |= CYCLE_COLLECTOR_BUFFERED };
    let full = CANDIDATES.with(|c| {
        let mut c = c.borrow_mut();
        c.push(entity);
        c.len() >= CANDIDATE_THRESHOLD
    });
    if full {
        unsafe { collect_cycles() };
    }
}

/// Called when a buffered entity dies through plain refcounting: its
/// memory is about to be reused, the buffer must not keep a dangling
/// root. Linear removal; rare (most candidates either get collected
/// or stay alive).
///
/// # Safety
/// `entity` must still point at the (dying) entity.
pub(crate) unsafe fn forget_candidate(entity: *mut RcHeader) {
    if unsafe { (*entity).flags } & CYCLE_COLLECTOR_BUFFERED == 0 {
        return;
    }
    unsafe { (*entity).flags &= !CYCLE_COLLECTOR_BUFFERED };
    CANDIDATES.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(i) = c.iter().position(|&p| p == entity) {
            c.swap_remove(i);
        }
    });
}

/// Bacon–Rajan synchronous cycle collection over the candidate
/// buffer. Returns the number of entities reclaimed.
///
/// # Safety
/// Single mutator thread parked here by construction (`rc-trace`).
pub unsafe fn collect_cycles() -> usize {
    let roots: Vec<*mut RcHeader> = CANDIDATES.with(|c| c.borrow_mut().drain(..).collect());
    if roots.is_empty() {
        return 0;
    }
    for &r in &roots {
        unsafe { (*r).flags &= !CYCLE_COLLECTOR_BUFFERED };
    }

    // mark_gray: trial-delete every internal edge reachable from roots.
    for &r in &roots {
        unsafe { mark_gray(r) };
    }
    // scan: subgraphs with external references left get restored to
    // black (re-incrementing), the rest turn white.
    for &r in &roots {
        unsafe { scan(r) };
    }
    // Gather whites (marking black to visit once).
    let mut whites = Vec::new();
    for &r in &roots {
        unsafe { collect_white(r, &mut whites) };
    }

    // Free the white set. Internal-edge releases already happened
    // count-wise in mark_gray and were deliberately not restored;
    // whites reference only each other or restored-black survivors.
    // Phase-1 limit: __destruct is not run here (module doc).
    for &w in &whites {
        unsafe {
            debug_assert_eq!((*w).refcount, 0, "white must have no external refs");
            crate::memory::stdapi::ll_free(w as *mut u8);
        }
    }
    whites.len()
}

unsafe fn mark_gray(root: *mut RcHeader) {
    if unsafe { color(root) } == COLOR_GRAY {
        return;
    }
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if unsafe { color(e) } == COLOR_GRAY {
            continue;
        }
        unsafe { set_color(e, COLOR_GRAY) };
        for child in unsafe { heap_children(e) } {
            unsafe {
                debug_assert!((*child).refcount > 0, "trial delete underflow");
                (*child).refcount -= 1;
            }
            stack.push(child);
        }
    }
}

unsafe fn scan(root: *mut RcHeader) {
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if unsafe { color(e) } != COLOR_GRAY {
            continue;
        }
        if unsafe { (*e).refcount } > 0 {
            unsafe { scan_black(e) };
        } else {
            unsafe { set_color(e, COLOR_WHITE) };
            stack.extend(unsafe { heap_children(e) });
        }
    }
}

unsafe fn scan_black(root: *mut RcHeader) {
    let mut stack = vec![root];
    unsafe { set_color(root, COLOR_BLACK) };
    while let Some(e) = stack.pop() {
        for child in unsafe { heap_children(e) } {
            unsafe { (*child).refcount += 1 };
            if unsafe { color(child) } != COLOR_BLACK {
                unsafe { set_color(child, COLOR_BLACK) };
                stack.push(child);
            }
        }
    }
}

unsafe fn collect_white(root: *mut RcHeader, whites: &mut Vec<*mut RcHeader>) {
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if unsafe { color(e) } != COLOR_WHITE {
            continue;
        }
        unsafe { set_color(e, COLOR_BLACK) }; // visit once
        whites.push(e);
        stack.extend(unsafe { heap_children(e) });
    }
}

/// ABI: run a cycle collection now (threshold-triggered runs use the
/// same path). Returns entities reclaimed.
///
/// # Safety
/// Callable between requests / at a safepoint of the single mutator.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_gc_collect_cycles() -> usize {
    unsafe { collect_cycles() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::memory::arena::Arena;
    use crate::memory::barrier::ref_store;
    use crate::memory::context::LLContext;
    use crate::object::ll_object_new;
    use crate::refcount::ll_release;
    use crate::value::Tag;

    /// Real store through the barrier: retain + slot write + Box stamp.
    unsafe fn link(arena: &mut Arena, from: *mut Object, offset: u32, to: *mut Object) {
        unsafe {
            let slot = (*from).prop_at(offset);
            ref_store(
                arena,
                from as *mut RcHeader,
                (slot as *mut u8) as *mut *mut RcHeader,
                std::ptr::null_mut(),
                to as *mut RcHeader,
            );
            slot.write(Value::entity(Tag::Object, to as *mut RcHeader));
        }
    }

    fn node_class() -> *const crate::class::Class {
        ClassBuilder::new("CycleNode").prop("next", true).build()
    }

    #[test]
    fn a_two_node_cycle_is_reclaimed() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            link(&mut arena, a, 16, b); // a→b: b rc=2
            link(&mut arena, b, 16, a); // b→a: a rc=2
            // External references die: counts drop to 1, both buffered.
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));
        }

        let freed = unsafe { collect_cycles() };
        assert_eq!(freed, 2, "the cycle is garbage and must be reclaimed");
        arena.reset(|_| {});
    }

    #[test]
    fn a_self_cycle_is_reclaimed() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            link(&mut arena, a, 16, a); // a→a: rc=2
            assert!(!ll_release(a as *mut RcHeader));
        }
        assert_eq!(unsafe { collect_cycles() }, 1);
        arena.reset(|_| {});
    }

    #[test]
    fn an_externally_referenced_cycle_survives_with_counts_restored() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            link(&mut arena, a, 16, b);
            link(&mut arena, b, 16, a);
            // Only b's external reference dies; a is still held (by us).
            assert!(!ll_release(b as *mut RcHeader));
        }

        assert_eq!(unsafe { collect_cycles() }, 0, "externally reachable");
        unsafe {
            assert_eq!((*a).rc.refcount, 2, "trial deletion fully restored");
            assert_eq!((*b).rc.refcount, 1);
        }

        // Now the external reference dies too: the cycle is garbage.
        unsafe { assert!(!ll_release(a as *mut RcHeader)) };
        assert_eq!(unsafe { collect_cycles() }, 2);
        arena.reset(|_| {});
    }

    #[test]
    fn buffering_is_deduplicated_and_death_forgets_the_candidate() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            crate::refcount::ll_retain(a as *mut RcHeader);
            crate::refcount::ll_retain(a as *mut RcHeader); // rc=3
            assert!(!ll_release(a as *mut RcHeader)); // buffered
            assert!(!ll_release(a as *mut RcHeader)); // deduplicated
        }
        let buffered = CANDIDATES.with(|c| {
            c.borrow()
                .iter()
                .filter(|&&p| p == a as *mut RcHeader)
                .count()
        });
        assert_eq!(buffered, 1, "one buffer entry per object");

        // The last reference dies through plain RC: the candidate must
        // be forgotten, and a later collection must not touch freed
        // memory.
        unsafe {
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_object_die(a);
        }
        assert_eq!(unsafe { collect_cycles() }, 0);
        arena.reset(|_| {});
    }

    #[test]
    fn acyclic_garbage_never_reaches_the_collector() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            // Plain death: refcount to zero, no non-zero decrement ever.
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_object_die(a);
        }
        assert_eq!(
            CANDIDATES.with(|c| c.borrow().len()),
            0,
            "straight-line deaths never buffer"
        );
    }
}
