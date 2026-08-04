//! The `rc-trace` cycle collector.
//!
//! Per `rfc/model/gc/strategies.md` there is no universal GC: the
//! collector is meant to be a strategy behind a fixed four-interface
//! contract, chosen at build time. **That contract lives in the RFC, not
//! in this file.** This crate implements one composition — `rc-trace`:
//! ARC as the primary reclamation path, arenas absorbing the bulk, and
//! stop-the-thread tracing for cycles only.
//!
//! There is **no selection mechanism today**, and the module should not
//! pretend otherwise: `ll_release` calls [`buffer_candidate`] directly,
//! and `ll_object_die` calls [`forget_candidate`] directly. A `nogc` or
//! pure-`rc` build would compile the buffering away, and the tool for
//! that is a cargo feature around those call sites — build-time
//! selection with nothing left behind, which is what the contract asks
//! for. It is not built yet.
//!
//! A `GcStrategy` trait with `NoGc`/`PureRc`/`RcTrace` impls used to sit
//! here. It was removed: nothing constructed or called it, its methods
//! took `&mut self` with no instance to hold, and being dispatch-shaped
//! it could not deliver the one thing it claimed — a build-time choice
//! that compiles the other strategies away. The abstraction is worth
//! introducing when a second real strategy exists and its shape is
//! known; MMTk, when it arrives, is that moment.
//!
//! The `rc-trace` cycle collector is Bacon–Rajan synchronous trial
//! deletion (the Zend architecture done right): candidate roots are
//! buffered on non-zero decrements, `mark_gray` trial-deletes internal
//! edges, `scan` restores externally-reachable subgraphs, white nodes
//! are cyclic garbage. Colors live in the header bits reserved for
//! exactly this (flags 4-5 + buffered bit 6). MMTK, when it arrives,
//! plugs in as just another implementation — never special-cased.
//!
//! **Arm vs fire** (`rfc/model/gc/strategies.md`, "Triggering"): buffering
//! a candidate runs from inside `ll_release`, mid-mutation, so it only
//! *arms* a collection (sets a pending flag); the collector *fires* only at
//! a clean point where refcounts and edges agree — an explicit
//! [`ll_gc_collect_cycles`] or the compiler's [`ll_gc_maybe_collect`] poll.
//! Firing inline would double-count a just-released edge and free a live
//! object. The arming *policy* (which signals, which thresholds) is the
//! compiler's decision; this crate is only the mechanism.
//!
//! `__destruct` of cyclically-dead objects **is** run, before the white
//! set is freed (`run_cyclic_destructors`). Because the counts are
//! trial-mutated when whites are known, they are first restored to a
//! consistent graph; then the destructors run through the ordinary
//! teardown path, and the set is re-collected so a resurrected subgraph
//! survives — the Zend-style discipline, with no new mechanism (no retain
//! hook, no GC-window flag).

use crate::object::Object;
use crate::refcount::{
    CANDIDATE_INDEX_MASK, CANDIDATE_INDEX_MAX, CANDIDATE_INDEX_SHIFT, CYCLE_COLLECTOR_BUFFERED,
    CYCLE_COLLECTOR_COLOR_SHIFT, DESTRUCTOR_PENDING, DESTRUCTOR_RAN, ENTITY_KIND_MASK, EntityKind,
    MemoryCategory, RcHeader, is_object, ll_release,
};
#[cfg(all(test, not(feature = "rc-walk")))]
use crate::value::Value;

// --- rc-trace machinery ----------------------------------------------------

/// Candidate-root buffer fill that *arms* a collection (Zend uses 10K; to
/// be calibrated, PLAN.md). Crossing it never runs the collector inline —
/// it only records that one is due (see `buffer_candidate`).
pub const CANDIDATE_THRESHOLD: usize = 10_000;

/// The fill that arms a collection. In production this folds to the
/// constant above (zero cost); under `cfg(test)` it is lowerable so a test
/// can arm at a precise point.
// In an `rc-walk` build nothing feeds the candidate buffer (`ll_release`
// computes no candidates — the walk does), so the feeding half of the
// rc-trace machinery is expectedly dead there while the module stays
// compiled as the registered alternative strategy. Only
// `buffer_candidate` carries the `expect`: the lint treats everything an
// allowed-dead item references as live, so annotating its callees too
// would leave those expectations unfulfilled.
#[cfg(not(test))]
#[inline(always)]
fn candidate_threshold() -> usize {
    CANDIDATE_THRESHOLD
}

#[cfg(test)]
thread_local! {
    static TEST_THRESHOLD: std::cell::Cell<usize> =
        const { std::cell::Cell::new(CANDIDATE_THRESHOLD) };
}
#[cfg(test)]
fn candidate_threshold() -> usize {
    TEST_THRESHOLD.with(|c| c.get())
}
#[cfg(all(test, not(feature = "rc-walk")))]
pub(crate) fn set_test_threshold(n: usize) {
    TEST_THRESHOLD.with(|c| c.set(n));
}

/// Tests only: make the candidate buffer refuse to grow.
#[cfg(all(test, not(feature = "rc-walk")))]
pub(crate) static FORCE_BUFFER_REFUSAL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

thread_local! {
    /// A raw pointer in a `Cell`, not a `RefCell<Vec<_>>`, and for
    /// soundness rather than speed. A `Vec` has drop glue, so its key is
    /// registered for TLS destruction; this buffer is reached **from** a
    /// TLS destructor, because thread exit runs the static-block
    /// teardown (`static_block.rs`) whose releases cascade into
    /// `buffer_candidate` and `forget_candidate`. TLS destructor order
    /// is unspecified — on glibc it is reverse registration order, which
    /// puts the exit guard last exactly because it registers first — so
    /// the buffer is reliably already destroyed, `with` panics with
    /// `AccessError` inside a destructor, and a panic there cannot
    /// unwind: the process aborts. A `Cell<*mut _>` has no drop glue, is
    /// never registered, and stays readable for the whole life of the
    /// thread; [`dispose`] frees it explicitly.
    static CANDIDATES: std::cell::Cell<*mut Vec<*mut RcHeader>> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
    /// A collection has been armed (the candidate buffer crossed the
    /// threshold) but deferred. It fires only at a clean point, never
    /// inline — see `buffer_candidate` and `ll_gc_maybe_collect`.
    static COLLECT_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// True while a collection is running. The reentrancy guard that makes
    /// any fire point safe even if it is somehow reached from within
    /// teardown: a nested `collect_cycles` becomes a no-op.
    static GC_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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

/// Counted heap children of a heap entity: the tracer's edge set,
/// dispatched on the entity kind through the shared tracer
/// (`walk::trace_entity`) — an object through `traced_runs`, a
/// reference box through its one Value. Gating on `is_object` here used
/// to be vacuously fine; once reference boxes became producible it made
/// trial deletion stop at the box, so a `$a->next = &$a` ring read
/// externally rooted forever (found by review, killed by
/// `a_cycle_through_a_reference_box_is_reclaimed`). Only the general
/// heap is traced — arenas and immortals are invisible to every
/// strategy by contract.
unsafe fn heap_children(e: *mut RcHeader) -> Vec<*mut RcHeader> {
    let mut children = Vec::new();
    unsafe {
        crate::walk::trace_entity(e, |c| {
            if (*c).memory_category() == MemoryCategory::GcHeap {
                children.push(c);
            }
        });
    }
    children
}

/// Called by `ll_release` on a non-zero decrement of a heap object:
/// buffer it once as a possible cycle root, and *arm* (never run) a
/// collection when the buffer fills.
///
/// This runs from **inside `ll_release`, mid-mutation**: the reference
/// that was just decremented is still physically in its slot, so refcounts
/// and edges disagree for this instant. Running the collector here would
/// walk that stale edge and subtract the reference a second time, freeing a
/// live object (`rfc/model/gc/strategies.md`, the arm/fire split). So we
/// only record that a collection is due; it fires at a clean point, chosen
/// by the compiler, via [`ll_gc_maybe_collect`] (or an explicit
/// [`ll_gc_collect_cycles`]).
///
/// ## Buffering is best-effort, and that is what keeps it abort-free
///
/// The buffer is a `Vec`, so growing it can fail — and `push` would
/// resolve that failure by killing the process, an abort nobody chose
/// and the ABI never mentions. There is a better answer here than
/// anywhere else in the runtime: **not buffering a candidate is always
/// safe.** Nothing is corrupted and nothing dangles, because the
/// buffered bit is only set when the entry really went in. So growth is
/// attempted, and a refusal arms a collection instead.
///
/// The price is a leak, not a delay, and the doc should say so:
/// buffering is edge-triggered on a non-zero decrement, so a refused
/// root is never re-offered. If that decrement was the last external
/// release of a garbage cycle, no further decrement comes and no later
/// collection can find it — this buffer is the collector's only root
/// set. Losing one cycle beats losing the process, but it is lost for
/// the life of the thread.
///
/// ## Why this is out of line
///
/// It was not, and the whole of it — the thread-local access, the
/// `RefCell` borrow, a `Vec` push with its growth and panic paths —
/// inlined into `ll_release`, the most frequent operation in the
/// runtime, on a path taken at most once per object per collection.
/// `ll_release` now tests the buffered bit itself, from flags it already
/// holds, and calls this only when something has to be recorded.
///
/// # Safety
/// `entity` must be live.
// Dead under `rc-walk` — see `candidate_threshold`'s note.
#[cfg_attr(feature = "rc-walk", expect(dead_code))]
#[inline(never)]
pub(crate) unsafe fn buffer_candidate(entity: *mut RcHeader) {
    if unsafe { (*entity).flags } & CYCLE_COLLECTOR_BUFFERED != 0 {
        return;
    }
    // An immediately-invoked closure, so the two refusal paths below
    // keep saying "no record" rather than returning from the function.
    let recorded = (|| {
        let c = unsafe { &mut *candidate_buffer() };
        // Fault injection, tests only: a refused `Vec` growth cannot be
        // provoked on demand, and an untested failure path is a guess
        // (same reasoning as `block_pool::FORCE_OOM`).
        #[cfg(all(test, not(feature = "rc-walk")))]
        if FORCE_BUFFER_REFUSAL.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        if c.len() == c.capacity() && c.try_reserve(1).is_err() {
            return None;
        }
        c.push(entity);
        Some((c.len() - 1, c.len() >= candidate_threshold()))
    })();
    let (index, full) = match recorded {
        Some(v) => v,
        None => {
            COLLECT_PENDING.with(|p| p.set(true));
            return;
        }
    };
    // The buffered bit and the position go into the same store.
    unsafe { (*entity).flags |= CYCLE_COLLECTOR_BUFFERED | encode_index(index) };
    if full {
        COLLECT_PENDING.with(|p| p.set(true));
    }
}

/// `index + 1` in the candidate-index field, or zero when it does not
/// fit — see [`CANDIDATE_INDEX_MAX`].
#[inline]
fn encode_index(index: usize) -> u32 {
    if index > CANDIDATE_INDEX_MAX {
        return 0;
    }
    (index as u32 + 1) << CANDIDATE_INDEX_SHIFT
}

/// The buffer position recorded in the header, if it was recorded.
#[inline]
unsafe fn decode_index(entity: *mut RcHeader) -> Option<usize> {
    let raw = (unsafe { (*entity).flags } & CANDIDATE_INDEX_MASK) >> CANDIDATE_INDEX_SHIFT;
    (raw != 0).then(|| raw as usize - 1)
}

/// Called when a buffered entity dies through plain refcounting: its
/// memory is about to be reused, the buffer must not keep a dangling
/// root.
///
/// The position comes from the entity's own header
/// ([`CANDIDATE_INDEX_SHIFT`]), so this is a swap-remove, not a search.
/// It used to scan the buffer — up to 10 000 pointers on an event that
/// happens on every ordinary death of a buffered object. The scan
/// survives only as the fallback for a position that did not fit the
/// field.
///
/// # Safety
/// `entity` must still point at the (dying) entity.
// Dead under `rc-walk` — see `candidate_threshold`'s note; the one
// caller left (`ll_default_dispose`) is cfg'd to the rc-trace arm.
#[cfg_attr(feature = "rc-walk", expect(dead_code))]
pub(crate) unsafe fn forget_candidate(entity: *mut RcHeader) {
    if unsafe { (*entity).flags } & CYCLE_COLLECTOR_BUFFERED == 0 {
        return;
    }
    let at = unsafe { decode_index(entity) };
    unsafe { (*entity).flags &= !(CYCLE_COLLECTOR_BUFFERED | CANDIDATE_INDEX_MASK) };
    {
        let c = unsafe { &mut *candidate_buffer() };
        // A recorded position is trusted only if the slot really holds
        // this entity; anything else falls back to the scan rather than
        // removing an innocent candidate.
        let i = match at {
            Some(i) if c.get(i) == Some(&entity) => Some(i),
            _ => c.iter().position(|&p| p == entity),
        };
        if let Some(i) = i {
            c.swap_remove(i);
            // The tail element moved into `i` and must learn its new
            // position — unless it never had one recorded.
            if let Some(&moved) = c.get(i) {
                if unsafe { decode_index(moved) }.is_some() {
                    unsafe {
                        (*moved).flags = ((*moved).flags & !CANDIDATE_INDEX_MASK) | encode_index(i)
                    };
                }
            }
        }
    }
}

/// This thread's candidate buffer, allocated on first use.
#[cfg_attr(feature = "rc-walk", allow(dead_code))]
fn candidate_buffer() -> *mut Vec<*mut RcHeader> {
    CANDIDATES.with(|cell| {
        let mut buf = cell.get();
        if buf.is_null() {
            buf = Box::into_raw(Box::new(Vec::new()));
            cell.set(buf);
        }
        buf
    })
}

/// Give this thread's candidate buffer back at thread exit.
///
/// Called from `ll_thread_exit`, not from a TLS destructor, which is the
/// point (see the `CANDIDATES` doc).
///
/// **The buffered entities are not touched**, and that is deliberate: a
/// candidate can already be gone. An arena entity dies with its arena
/// without individual teardown, so nothing calls [`forget_candidate`]
/// for it and its buffer entry outlives its memory — Miri caught a
/// write through exactly such an entry when this function tried to
/// clear buffered bits on the way out (2026-08-03). A pointer in this
/// buffer is not evidence that anything is still there.
///
/// **Known limit, and it is the reason the clearing looked worth
/// doing**: an entity that is genuinely alive and still buffered when
/// its thread dies keeps `CYCLE_COLLECTOR_BUFFERED` set while its block
/// goes to the abandoned list. If an adopting thread ever releases it,
/// the stale bit suppresses re-buffering forever and its cycle leaks.
/// Cross-thread entity survival is reserved today, so nothing reaches
/// that case; closing it needs a liveness test this buffer cannot
/// provide, which is a design question and not a cleanup.
///
/// Null-tolerant and idempotent.
#[cfg_attr(feature = "rc-walk", allow(dead_code))]
pub(crate) fn dispose() {
    let buf = CANDIDATES.with(|cell| cell.replace(std::ptr::null_mut()));
    if !buf.is_null() {
        unsafe { drop(Box::from_raw(buf)) };
    }
}

/// Bacon–Rajan synchronous cycle collection over the candidate buffer.
/// Returns the number of entities reclaimed.
///
/// Reentrancy-guarded: a call made while a collection is already running is
/// a no-op, so a fire point reached from inside teardown (a `__destruct`
/// that triggers collection, say) cannot recurse into the marker. Clears
/// the pending flag it may have been armed with.
///
/// # Safety
/// Must run at a **clean point** — where refcounts and physical edges agree
/// (between mutator operations), not mid-store or mid-teardown. That
/// invariant is the whole reason for the arm/fire split
/// (`rfc/model/gc/strategies.md`); `buffer_candidate` arms, this fires.
/// Single mutator thread parked here by construction (`rc-trace`).
pub unsafe fn collect_cycles() -> usize {
    if GC_ACTIVE.with(|a| a.get()) {
        return 0;
    }
    // Reset the guard on every exit path, including a panicking assert in
    // debug builds, so a poisoned collection can't wedge the collector off.
    struct Active;
    impl Drop for Active {
        fn drop(&mut self) {
            GC_ACTIVE.with(|a| a.set(false));
        }
    }
    GC_ACTIVE.with(|a| a.set(true));
    COLLECT_PENDING.with(|p| p.set(false));
    let _active = Active;
    unsafe { collect_cycles_inner() }
}

unsafe fn collect_cycles_inner() -> usize {
    let mut reclaimed = 0;
    loop {
        let roots: Vec<*mut RcHeader> = unsafe { std::mem::take(&mut *candidate_buffer()) };
        if roots.is_empty() {
            return reclaimed;
        }
        for &r in &roots {
            unsafe { (*r).flags &= !(CYCLE_COLLECTOR_BUFFERED | CANDIDATE_INDEX_MASK) };
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

        // Null the whites' weak cells BEFORE any destructor runs — the
        // obligation `rfc/model/weak-references.md` binds on every cycle
        // teardown — and before the raw frees below, which bypass
        // `dispose` and would otherwise leave a cell dangling. Runs every
        // round, so destructor-recreated weak state on a still-garbage
        // white is cleared by the next round's pass.
        unsafe { crate::weak::notify_members(&whites) };

        // Any cyclic garbage still owing a `__destruct` cannot be freed yet:
        // run the destructors (`run_cyclic_destructors`) and re-collect. A
        // resurrected subgraph survives the re-trace; the rest returns as
        // garbage with `DESTRUCTOR_RAN` set, so this loops until nothing owes
        // a destructor, then falls through to the free.
        let owes_destructor = whites.iter().any(|&w| unsafe {
            is_object((*w).flags)
                && (*w).flags & DESTRUCTOR_PENDING != 0
                && (*w).flags & DESTRUCTOR_RAN == 0
        });
        if owes_destructor {
            unsafe { run_cyclic_destructors(&whites) };
            continue; // the survivors re-buffered themselves; re-collect
        }

        // Free the white set. Internal-edge releases already happened
        // count-wise in mark_gray and were deliberately not restored;
        // whites reference only each other or restored-black survivors.
        //
        // The escape hold-counts are a different matter and must be dropped
        // explicitly: arena entities are invisible to the trace (only the
        // heap is traced), so nothing above touched them, and a hold-count
        // left standing makes arena reset believe a dead holder still holds
        // its escapee — which it then promotes and keeps forever.
        // `ll_object_die` does this in its phase 2; the collector never gets
        // there, so it does it here.
        for &w in &whites {
            unsafe {
                debug_assert_eq!((*w).refcount, 0, "white must have no external refs");
                debug_assert_eq!(
                    (*w).flags & crate::refcount::HAS_WEAK_REFERENCES,
                    0,
                    "the weak pass above ran this round, before any user code"
                );
                // By kind, like the trace: a white reference box can hold
                // an arena escapee in its Value just as an object slot can.
                crate::walk::trace_entity(w, |child| {
                    if (*child).memory_category() == MemoryCategory::RequestArena {
                        crate::memory::barrier::escape_lose(child);
                    }
                });
                // A white weak cell has state OUTSIDE the traced graph: a
                // still-live target's weak-table row maps to it, and its
                // target's bit 7 is set. The raw free below would leave
                // both dangling — the next create() on that target would
                // return this freed cell. Its die-arm unregisters and
                // frees; it never touches counts, so trial-deleted
                // siblings are safe.
                if (*w).flags & ENTITY_KIND_MASK == EntityKind::WeakRef.to_flags() {
                    crate::weak::weakref_die(w as *mut crate::weak::LLWeakRef);
                } else {
                    crate::memory::stdapi::ll_free(w as *mut u8);
                }
            }
        }
        reclaimed += whites.len();
        return reclaimed;
    }
}

/// Run `__destruct` for a cyclic-garbage set before it is freed, then leave
/// its survivors re-buffered for the caller to re-collect.
///
/// The white counts are trial-deleted (internal in-edges unreflected), and
/// destructors must not run over that: `ll_release`/`drop_ref` read those
/// counts, so a `$this->x = null` would drive a live sibling to a false zero
/// and double-free it. So, in order:
///
/// 1. **Restore** real counts — re-increment every white's heap children,
///    undoing `mark_gray` for white-sourced edges (`scan_black` already did
///    survivor-sourced ones). The graph is now a consistent garbage cycle.
/// 2. **Guard** every white (`+= 1`): with real counts `rc >= 1` holds, so a
///    released sibling stops at its guard, not at zero.
/// 3. **Run** each pending `__destruct` once (`DESTRUCTOR_RAN`). A store
///    retains normally, so a resurrection is just an ordinary reference.
/// 4. **Un-guard via `ll_release`** (never a raw `-= 1`): a white whose cycle
///    a destructor broke dies here through the proven path (`dispose` skips
///    the already-run `__destruct`); a still-referenced one re-buffers itself
///    for the re-collection.
///
/// No new mechanism — once counts are real, every operation is the ordinary
/// one.
///
/// # Safety
/// `whites` are the just-collected cyclic garbage (trial-deleted counts);
/// `GC_ACTIVE` is held.
unsafe fn run_cyclic_destructors(whites: &[*mut RcHeader]) {
    for &w in whites {
        for child in unsafe { heap_children(w) } {
            unsafe { (*child).refcount += 1 };
        }
    }
    for &w in whites {
        unsafe { (*w).refcount += 1 };
    }
    for &w in whites {
        if is_object(unsafe { (*w).flags }) {
            unsafe { crate::object::run_pre_destructor(w as *mut Object) };
        }
    }
    for &w in whites {
        if unsafe { ll_release(w) } {
            // Ordinary death: the kind switch — a white may be an object,
            // a reference box or a weak cell (only objects buffer, but the
            // trace pulls the other kinds in as their holders' children).
            unsafe { crate::object::ll_entity_die(w) };
        }
    }
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

/// ABI: run a cycle collection now, whether or not one was armed. Returns
/// entities reclaimed.
///
/// # Safety
/// Callable between requests / at a safepoint of the single mutator.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_gc_collect_cycles() -> usize {
    unsafe { collect_cycles() }
}

/// ABI: fire a collection only if one was *armed*, else do nothing. This is
/// the poll the compiler injects at the safepoints it chooses — statement
/// boundary, allocation slow path, request end (`rfc/model/gc/strategies.md`,
/// §2 and the arm/fire split). The arming *policy* (which signals, which
/// thresholds) is the compiler's decision, outside this crate; the runtime
/// only records "due" and collects here, where the graph is clean.
///
/// # Safety
/// Callable at a safepoint of the single mutator (roots enumerable,
/// refcounts and edges consistent).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_gc_maybe_collect() -> usize {
    // The safepoint is also where the barrier's reserve is refilled. It
    // is the only place that can be: drawing on the reserve happens
    // inside `ll_ref_store`, which has no way to report anything, while
    // this poll runs in a frame that can raise. Refilling here is what
    // turns "the barrier would eventually fail" into "the next safepoint
    // raises memory-exhausted", thousands of records earlier
    // (`rfc/runtime/exceptions.md`, "The log reserve protocol").
    if crate::memory::reserve::is_drawn() {
        let _ = crate::memory::reserve::replenish();
    }
    // The compiler's poll is also an rc-walk checkpoint: handshake ack
    // and verdict drain (`crate::epoch`). In that build nothing arms
    // COLLECT_PENDING, so the rc-trace branch below stays cold dead.
    #[cfg(feature = "rc-walk")]
    crate::epoch::checkpoint();
    if COLLECT_PENDING.with(|p| p.get()) {
        unsafe { collect_cycles() }
    } else {
        0
    }
}

/// ABI: serve the rc-walk epoch protocol now — handshake ack, verdict
/// drain, parked-memory flush. The compiler emits it once **after** a
/// run of [`ll_release_batch`](crate::refcount::ll_release_batch)
/// calls (a scope exit), paired with one [`ll_gc_checkpoint_ack`]
/// before the run: the trailing position is load-bearing — a pre-run
/// pickup judges the scope's still-counted transients, the phase-lock
/// shape (`rfc/model/gc/rc-walk.md`, "Batched releases", amendment
/// 2026-07-28; the full argument lives in [`crate::epoch`]'s module
/// doc). A no-op in an rc-trace build, kept exported so lowering is
/// configuration-independent.
///
/// # Safety
/// Callable at a safepoint of the mutator: a drained verdict runs
/// destructors on this thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_gc_checkpoint() {
    #[cfg(feature = "rc-walk")]
    crate::epoch::checkpoint();
}

/// ABI: ack a pending epoch handshake, nothing else — no message
/// pickup, no flush. The compiler emits it once **before** a run of
/// [`ll_release_batch`](crate::refcount::ll_release_batch) calls, so
/// the epoch's activity bit is observed before any free the run
/// performs — the same ordering the death branch of `ll_release` buys
/// with its own ack (`rfc/model/gc/rc-walk.md`, "Batched releases").
/// The full [`ll_gc_checkpoint`] trails the run. A no-op in an
/// rc-trace build.
///
/// # Safety
/// Callable anywhere on a mutator thread: it runs no user code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_gc_checkpoint_ack() {
    #[cfg(feature = "rc-walk")]
    crate::epoch::checkpoint_ack();
}

// The whole module is the rc-trace strategy, and these are its tests:
// they feed the candidate buffer through `ll_release`, which an `rc-walk`
// build compiles without the buffering tail — there the collector is the
// walk, tested in `walk.rs`. Strategy selection is build-time
// (`rfc/model/gc/strategies.md`); each strategy's tests run in the
// configuration where that strategy is wired, and the default
// configuration keeps running these.
#[cfg(all(test, not(feature = "rc-walk")))]
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::memory::arena::Arena;
    use crate::memory::barrier::ref_store;
    use crate::memory::context::LLContext;
    use crate::object::new_constructed;
    use crate::refcount::ll_release;
    use crate::value::Tag;

    /// Real store through the barrier: retain + whole-value slot write.
    unsafe fn link(arena: *mut Arena, from: *mut Object, offset: u32, to: *mut Object) {
        unsafe {
            let slot = Object::prop_at(from, offset);
            ref_store(
                arena,
                from as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, to as *mut RcHeader),
            );
        }
    }

    fn node_class() -> *const crate::class::Class {
        ClassBuilder::new("CycleNode").prop("next", true).build()
    }

    /// A cyclic garbage holder that referenced an arena object still
    /// owes it a `lose`. The trace never sees arena entities — only the
    /// heap is traced — so freeing the white set has to drop those
    /// hold-counts itself. Left standing, the count makes arena reset
    /// believe a dead holder still holds the escapee, and reset promotes
    /// it: a leak for the life of the process, and a live-looking object
    /// nobody can reach.
    #[test]
    fn collecting_a_holder_drops_its_escape_hold_counts() {
        use crate::refcount::IS_ESCAPEE;

        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("EscHolder")
            .prop("peer", true)
            .prop("esc", true)
            .build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        unsafe {
            let a = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
            let b = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
            let escapee = new_constructed(&mut ctx, node_class(), MemoryCategory::RequestArena);

            link(&mut arena, a, 16, b); // a <-> b: a heap cycle
            link(&mut arena, b, 16, a);
            link(&mut arena, a, 32, escapee); // and a holds an arena object
            assert_ne!(
                (*escapee).rc.flags & IS_ESCAPEE,
                0,
                "the store made it an escapee"
            );

            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));
            assert_eq!(ll_gc_collect_cycles(), 2, "the cycle is garbage");

            assert_eq!(
                (*escapee).rc.flags & IS_ESCAPEE,
                0,
                "the dead holder let go of its escapee"
            );
        }
        arena.reset(|_| {});
    }

    /// A ring through a reference box (`$a->next = &$a`): trial deletion
    /// must trace THROUGH the box by kind, or the box's back-edge is
    /// invisible, the object reads externally rooted, and the ring leaks
    /// silently forever.
    #[test]
    fn a_cycle_through_a_reference_box_is_reclaimed() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let r = unsafe { crate::reference::ll_reference_new(&mut ctx, MemoryCategory::GcHeap) };
        unsafe {
            // a.next owns the box's initial ref; the box owns a's second.
            Object::prop_at(a, 16).write(Value::entity(Tag::Reference, r as *mut RcHeader));
            crate::refcount::ll_retain(a as *mut RcHeader);
            (*r).value = Value::entity(Tag::Object, a as *mut RcHeader);
            // The frame's reference dies: a is buffered as a candidate.
            assert!(!ll_release(a as *mut RcHeader));
        }

        let freed = unsafe { collect_cycles() };
        assert_eq!(freed, 2, "object + box are one garbage ring");
        arena.reset(|_| {});
    }

    #[test]
    fn a_two_node_cycle_is_reclaimed() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
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

    /// Cyclic garbage must run `__destruct` before it is freed — the gap
    /// this closes. A two-node cycle of objects each with a destructor,
    /// unreferenced from outside, is collected; both destructors must fire.
    #[test]
    fn cyclic_garbage_runs_its_destructor() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CYCLE_DTORS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn counting(_o: *mut Object) {
            CYCLE_DTORS.fetch_add(1, Ordering::Relaxed);
        }

        let _g = crate::memory::block_pool::test_guard();
        CYCLE_DTORS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("DtorNode")
            .prop("next", true)
            .destructor(counting as *const ())
            .build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        unsafe {
            let a = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
            let b = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
            link(&mut arena, a, 16, b);
            link(&mut arena, b, 16, a);
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));

            assert_eq!(collect_cycles(), 2, "the cycle is garbage");
            assert_eq!(
                CYCLE_DTORS.load(Ordering::Relaxed),
                2,
                "both cyclic objects ran __destruct before being freed"
            );
        }
        arena.reset(|_| {});
    }

    /// A destructor that nulls its own edge (`$this->next = null`) releases
    /// a sibling mid-teardown. The guard must hold that sibling to its own
    /// un-guard; nothing may be freed twice (Miri is the real check).
    #[test]
    fn a_destructor_unsetting_its_own_edge_does_not_double_free() {
        use crate::memory::context::{resolve_arena, set_current_context};
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DTORS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn unset_next(obj: *mut Object) {
            DTORS.fetch_add(1, Ordering::Relaxed);
            unsafe {
                let arena = resolve_arena(std::ptr::null_mut());
                let slot = Object::prop_at(obj, 16);
                let v = slot.read();
                let old = if v.is_refcounted() {
                    v.entity_ptr()
                } else {
                    std::ptr::null_mut()
                };
                ref_store(arena, obj as *mut RcHeader, slot, old, Value::null());
            }
        }

        let _g = crate::memory::block_pool::test_guard();
        DTORS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("Unsetter")
            .prop("next", true)
            .destructor(unset_next as *const ())
            .build();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        unsafe {
            let a = new_constructed(ctx_ptr, cls, MemoryCategory::GcHeap);
            let b = new_constructed(ctx_ptr, cls, MemoryCategory::GcHeap);
            link(arena_ptr, a, 16, b);
            link(arena_ptr, b, 16, a);
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));
            collect_cycles();
            assert_eq!(
                DTORS.load(Ordering::Relaxed),
                2,
                "both ran once, no double free"
            );
        }
        set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// A destructor stores `$this` into a live holder, resurrecting the
    /// cycle. The re-trace must keep the resurrected object *and* its child
    /// (which gained no direct external reference — it survives only because
    /// its parent does), and `__destruct` must not run a second time.
    #[test]
    fn a_destructor_resurrecting_the_cycle_keeps_it_and_its_child() {
        use crate::memory::context::{resolve_arena, set_current_context};
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DTORS: AtomicUsize = AtomicUsize::new(0);
        static LIVE: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn resurrect(obj: *mut Object) {
            DTORS.fetch_add(1, Ordering::Relaxed);
            unsafe {
                let arena = resolve_arena(std::ptr::null_mut());
                let l = LIVE.load(Ordering::Relaxed) as *mut Object;
                let slot = Object::prop_at(l, 16);
                ref_store(
                    arena,
                    l as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Object, obj as *mut RcHeader),
                );
            }
        }

        let _g = crate::memory::block_pool::test_guard();
        DTORS.store(0, Ordering::Relaxed);
        let a_cls = ClassBuilder::new("Resur")
            .prop("next", true)
            .destructor(resurrect as *const ())
            .build();
        let l_cls = ClassBuilder::new("LiveHolder").prop("keep", true).build();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        unsafe {
            let l = new_constructed(ctx_ptr, l_cls, MemoryCategory::GcHeap);
            let a = new_constructed(ctx_ptr, a_cls, MemoryCategory::GcHeap);
            let b = new_constructed(ctx_ptr, node_class(), MemoryCategory::GcHeap);
            LIVE.store(l as usize, Ordering::Relaxed);
            link(arena_ptr, a, 16, b);
            link(arena_ptr, b, 16, a);
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));

            let freed = collect_cycles();
            assert_eq!(DTORS.load(Ordering::Relaxed), 1);
            assert_eq!(freed, 0, "resurrected: nothing freed");
            assert_eq!(
                Object::prop_at(l, 16).read().entity_ptr(),
                a as *mut RcHeader,
                "L keeps A"
            );
            assert_eq!(
                Object::prop_at(a, 16).read().entity_ptr(),
                b as *mut RcHeader,
                "A->B intact"
            );
            assert_eq!((*a).rc.refcount, 2, "A: B->A + L->A");
            assert_eq!((*b).rc.refcount, 1, "B: A->B");

            // Drop the holder; the cycle is garbage again but __destruct
            // already ran, so it must not fire twice.
            ref_store(
                arena_ptr,
                l as *mut RcHeader,
                Object::prop_at(l, 16),
                a as *mut RcHeader,
                Value::null(),
            );
            assert!(ll_release(l as *mut RcHeader));
            crate::object::ll_object_die(l);
            assert_eq!(collect_cycles(), 2, "the un-held cycle is reclaimed");
            assert_eq!(DTORS.load(Ordering::Relaxed), 1, "no second __destruct");
        }
        set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    #[test]
    fn a_self_cycle_is_reclaimed() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
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

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
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

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            crate::refcount::ll_retain(a as *mut RcHeader);
            crate::refcount::ll_retain(a as *mut RcHeader); // rc=3
            assert!(!ll_release(a as *mut RcHeader)); // buffered
            assert!(!ll_release(a as *mut RcHeader)); // deduplicated
        }
        let buffered = CANDIDATES.with(|c| {
            unsafe { &*candidate_buffer() }
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

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            // Plain death: refcount to zero, no non-zero decrement ever.
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_object_die(a);
        }
        assert_eq!(
            unsafe { (*candidate_buffer()).len() },
            0,
            "straight-line deaths never buffer"
        );
    }

    /// The candidate buffer crossing its threshold *arms* a collection but
    /// never runs it inline. Here the arming happens inside `ll_object_die`'s
    /// phase 2 (a child release), the worst possible moment: on the old
    /// fire-inline code that collection ran mid-teardown and freed the
    /// dying object a second time. Now it only sets the pending flag, and
    /// the live child survives.
    #[test]
    fn threshold_crossing_during_teardown_only_arms() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let p = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let c = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };

        unsafe {
            // p.next = c  → c held by p's slot (rc 2) and by us (the creator
            // reference, which must keep c alive past p's death).
            link(&mut arena, p, 16, c);
            assert_eq!((*c).rc.refcount, 2);

            // Buffer p as a cycle-root candidate (a non-zero decrement),
            // still under the default threshold so nothing arms yet.
            crate::refcount::ll_retain(p as *mut RcHeader); // rc 2
            assert!(!ll_release(p as *mut RcHeader)); // rc 1, buffered
            assert!(!COLLECT_PENDING.with(|f| f.get()), "not armed yet");

            // From now the next buffered candidate crosses the threshold.
            set_test_threshold(1);

            // p's last reference dies; teardown releases c during phase 2,
            // which buffers c and crosses the threshold *mid-teardown*.
            assert!(ll_release(p as *mut RcHeader)); // rc 0 → death
            crate::object::ll_object_die(p);
            set_test_threshold(CANDIDATE_THRESHOLD);

            // The collection was armed, not fired: nothing ran inside the
            // teardown, so the still-referenced child is untouched and p was
            // freed exactly once (no crash). On the fire-inline code
            // COLLECT_PENDING is instead false here (a collection ran).
            assert!(COLLECT_PENDING.with(|f| f.get()), "armed, not fired");
            assert_eq!((*c).rc.refcount, 1, "the live child must survive");

            // Firing at a clean point reclaims nothing (c is externally held).
            assert_eq!(ll_gc_maybe_collect(), 0);
            assert!(!COLLECT_PENDING.with(|f| f.get()), "pending cleared");

            assert!(ll_release(c as *mut RcHeader));
            crate::object::ll_object_die(c);
        }
        arena.reset(|_| {});
    }

    /// An armed collection is deferred to a clean fire point: crossing the
    /// threshold from inside `ll_release` must not collect there (that is
    /// the mid-mutation hazard), only arm. The cyclic garbage stays live
    /// until `ll_gc_maybe_collect` runs it at a safe point.
    #[test]
    fn armed_cycle_is_deferred_to_maybe_collect() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            link(&mut arena, a, 16, a); // self-cycle: a.rc = 2
            set_test_threshold(1); // the buffering release will cross it

            // External reference dies: a non-zero decrement (a is still held
            // by its own self-edge), so it buffers a and crosses the
            // threshold from *inside* ll_release. Arm-and-defer must not
            // collect here.
            assert!(!ll_release(a as *mut RcHeader)); // a.rc 1, buffered
            set_test_threshold(CANDIDATE_THRESHOLD);

            assert!(COLLECT_PENDING.with(|f| f.get()), "armed");
            assert_eq!(
                (*a).rc.refcount,
                1,
                "cyclic garbage still live, not collected inline"
            );

            // Fire at a clean point: now the cycle is reclaimed.
            assert_eq!(ll_gc_maybe_collect(), 1);
            assert!(
                !COLLECT_PENDING.with(|f| f.get()),
                "pending cleared after fire"
            );
        }
        arena.reset(|_| {});
    }

    /// A buffer that cannot grow refuses the candidate instead of taking
    /// the process down with it. The entity must come out of that
    /// unmarked — a buffered bit with no entry behind it would make the
    /// object permanently unbufferable and, worse, make `forget_candidate`
    /// hunt for something that was never there.
    #[test]
    fn a_refused_candidate_is_left_unmarked_and_arms_a_collection() {
        // `FORCE_BUFFER_REFUSAL` is process-global, so this has to hold
        // the test lock like any other fault injection here.
        let _g = crate::memory::block_pool::test_guard();
        use std::sync::atomic::Ordering;

        let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
        let p = &mut e as *mut RcHeader;
        COLLECT_PENDING.with(|f| f.set(false));

        FORCE_BUFFER_REFUSAL.store(true, Ordering::Relaxed);
        unsafe { buffer_candidate(p) };
        FORCE_BUFFER_REFUSAL.store(false, Ordering::Relaxed);

        assert!(
            unsafe { (*candidate_buffer()).is_empty() },
            "nothing was recorded"
        );
        assert_eq!(
            e.flags & CYCLE_COLLECTOR_BUFFERED,
            0,
            "and nothing was claimed"
        );
        assert!(COLLECT_PENDING.with(|f| f.get()), "a refusal arms instead");

        // Still bufferable once there is room again.
        unsafe { buffer_candidate(p) };
        assert_eq!(unsafe { (*candidate_buffer()).len() }, 1);
        unsafe { forget_candidate(p) };
        COLLECT_PENDING.with(|f| f.set(false));
    }

    /// `swap_remove` moves the tail candidate, so its recorded position
    /// has to move with it. A stale one cannot corrupt the buffer — the
    /// slot is checked before removal and a mismatch falls back to the
    /// scan — so this asserts the position itself, not just the outcome.
    #[test]
    fn forgetting_a_candidate_keeps_the_moved_one_findable() {
        let mut h: Vec<RcHeader> = (0..4)
            .map(|_| RcHeader::new(MemoryCategory::GcHeap, 0))
            .collect();
        let p: Vec<*mut RcHeader> = h.iter_mut().map(|e| e as *mut RcHeader).collect();
        let buffer = || unsafe { (*candidate_buffer()).clone() };

        unsafe {
            for &e in &p {
                buffer_candidate(e);
            }
            assert_eq!(buffer(), p, "buffered in order");

            // Removes index 1 and moves p[3] into it.
            forget_candidate(p[1]);
            assert_eq!(buffer(), vec![p[0], p[3], p[2]]);
            assert_eq!(
                decode_index(p[3]),
                Some(1),
                "the moved candidate knows where it is"
            );
            assert_eq!(
                decode_index(p[1]),
                None,
                "the removed one no longer claims a slot"
            );

            forget_candidate(p[3]);
            assert_eq!(buffer(), vec![p[0], p[2]], "the moved candidate was found");

            forget_candidate(p[0]);
            forget_candidate(p[2]);
            assert!(buffer().is_empty());
            assert!(h.iter().all(|e| e.flags & CYCLE_COLLECTOR_BUFFERED == 0));
        }
    }
}
