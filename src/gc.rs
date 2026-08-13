//! The `rc-trace` cycle collector: ARC as the primary reclamation path,
//! arenas absorbing the bulk, and stop-the-thread tracing for cycles only.
//! The four-interface strategy contract this composes with lives in
//! `rfc/model/gc/strategies.md` rather than here.
//!
//! There is **no selection mechanism today**, and the module does not
//! pretend otherwise: `ll_release` calls [`buffer_candidate`] directly,
//! and the three teardown doors call [`forget_candidate`] directly —
//! `ll_entity_die`, `ll_object_die`, and the drain inside
//! `array::entity::array_die`, which a nested array reaches instead of the
//! first of those. A `nogc` or pure-`rc` build would compile the buffering
//! away through a cargo feature around those call sites — build-time
//! selection with nothing left behind, and neither feature exists yet
//! (`rfc/model/gc/strategies.md`, "Strategy Registry"). A `GcStrategy`
//! trait is not the answer and was removed once: dispatch cannot deliver a
//! build-time choice that compiles the other strategies away. It is worth
//! reintroducing when a second real strategy exists and its shape is
//! known.
//!
//! The algorithm is Bacon-Rajan synchronous trial deletion: candidate
//! roots are buffered on non-zero decrements, `mark_gray` trial-deletes
//! internal edges, `scan` restores externally-reachable subgraphs, and
//! white nodes are cyclic garbage. Colours live in the header bits
//! reserved for them, flags 4-5 with the buffered bit 6.
//!
//! **Arm against fire** (`rfc/model/gc/strategies.md`, "Triggering: arm vs
//! fire"): buffering a candidate only *arms* a collection, and the
//! collector *fires* at a clean point where refcounts and edges agree.
//! Which signals and thresholds arm is the compiler's decision; this crate
//! is the mechanism.
//!
//! `__destruct` of cyclically-dead objects **is** run, before the white
//! set is freed (`run_cyclic_destructors`). The counts are trial-mutated
//! when whites are known, so they are first restored to a consistent
//! graph; then the destructors run through the ordinary teardown path, and
//! the set is re-collected so a resurrected subgraph survives.

use crate::object::Object;
use crate::refcount::{
    CANDIDATE_INDEX_MASK, CANDIDATE_INDEX_MAX, CANDIDATE_INDEX_SHIFT, CYCLE_COLLECTOR_BUFFERED,
    CYCLE_COLLECTOR_COLOR_SHIFT, DESTRUCTOR_PENDING, DESTRUCTOR_RAN, ENTITY_KIND_MASK, EntityKind,
    MemoryCategory, RcHeader, is_object, ll_release,
};
#[cfg(all(test, not(feature = "rc-walk")))]
use crate::value::Value;

// --- rc-trace machinery ----------------------------------------------------

/// Candidate-root buffer fill that *arms* a collection (Zend uses 10K;
/// uncalibrated here). Crossing it never runs the collector inline —
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

/// Tests only: arm a collection at `n` buffered candidates instead of
/// [`CANDIDATE_THRESHOLD`]. Thread-local and sticky — restore the
/// constant before returning, or every later test on this thread arms at
/// `n`.
#[cfg(all(test, not(feature = "rc-walk")))]
pub(crate) fn set_test_threshold(n: usize) {
    TEST_THRESHOLD.with(|c| c.set(n));
}

#[cfg(all(test, not(feature = "rc-walk")))]
thread_local! {
    /// Tests only: make the candidate buffer refuse to grow, on this
    /// thread alone.
    ///
    /// **Thread-local because the buffer it forces is thread-local.** A
    /// process-global flag is read by every test running beside the one
    /// that raised it, so the refusal lands in buffers the raiser never
    /// looks at — and a test that asserts its own buffer's contents then
    /// fails for a reason nothing in it names. `block_pool::test_guard`
    /// serializes only the tests that take it, so a lock cannot close
    /// this (`dev/POSTMORTEM.md`, 2026-08-12).
    pub(crate) static FORCE_BUFFER_REFUSAL: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

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
    /// Teardowns in flight on this thread, counted because teardown
    /// cascades: a child release inside a dispose is another teardown.
    /// While it is non-zero every fire point returns without collecting
    /// (`dev/DECISIONS.md`, "a fire point inside a teardown collects
    /// nothing, and the runtime enforces it"), which is what makes the
    /// arm/fire split hold for user code as well as for the runtime — a
    /// destructor may hold the compiler's poll, and the poll must do
    /// nothing there. The rc-walk twin of this counter is
    /// `epoch::TEARDOWN_DEPTH`, which brackets the same two doors for a
    /// different reason (message pickup).
    static TEARDOWN_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Enter a teardown: fire points stop collecting until the matching
/// [`teardown_exit`]. Called by the two doors into teardown
/// (`ll_entity_die`, `ll_object_die`), which nest.
#[cfg_attr(feature = "rc-walk", allow(dead_code))]
#[inline]
pub(crate) fn teardown_enter() {
    TEARDOWN_DEPTH.with(|d| d.set(d.get() + 1));
}

/// See [`teardown_enter`]. Nothing is fired on the way out: an armed
/// collection waits for the next poll, where the graph is clean.
#[cfg_attr(feature = "rc-walk", allow(dead_code))]
#[inline]
pub(crate) fn teardown_exit() {
    TEARDOWN_DEPTH.with(|d| d.set(d.get() - 1));
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
/// (`walk::trace_entity`) — an object through its trace map, a
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

/// Called by `ll_release` on a non-zero decrement of a heap entity whose
/// kind can close a cycle ([`crate::refcount::CANDIDATE_KINDS`]): buffer
/// it once as a possible cycle root, and *arm* (never run) a collection
/// when the buffer fills.
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
/// **Buffering is best-effort, which is what keeps it abort-free.** The
/// buffer is a `Vec`, and `push` would resolve a failed growth by killing
/// the process. Not buffering a candidate is always safe instead: nothing
/// is corrupted and nothing dangles, the buffered bit being set only when
/// the entry really went in. So growth is attempted and a refusal arms a
/// collection.
///
/// The price is a leak rather than a delay. Buffering is edge-triggered on
/// a non-zero decrement, so a refused root is never re-offered, and if
/// that decrement was the last external release of a garbage cycle no
/// later collection can find it, this buffer being the collector's only
/// root set.
///
/// Out of line, because inlined it lands in `ll_release` for work needed
/// at most once per object per collection. `ll_release` tests the buffered
/// bit itself, from flags it already holds, and calls this only when there
/// is something to record (`dev/BENCHMARKS.md`).
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
        if FORCE_BUFFER_REFUSAL.with(|f| f.get()) {
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
/// ([`CANDIDATE_INDEX_SHIFT`]), so this is a swap-remove rather than a
/// scan of up to 10 000 pointers on an event that happens at every
/// ordinary death of a buffered object. The scan survives only as the
/// fallback for a position that did not fit the field.
///
/// **Every door into teardown calls this**, and none delegates the duty
/// to `dispose`, which is class code: `ll_entity_die` before its kind
/// switch, `ll_object_die` after `dispose` returns and never before, and
/// `array::entity::array_die`'s drain for a nested array, which reaches
/// teardown without passing `ll_entity_die` at all (`dev/DECISIONS.md`,
/// "the candidate buffer admits arrays, and leaving it belongs to the
/// runtime"; the drain's own share of the duty is under the entry of
/// 2026-08-08, whose heading names the pinned block instead).
///
/// # Safety
/// `entity` must still point at the (dying) entity.
// Dead under `rc-walk` — see `candidate_threshold`'s note; the callers
// left are cfg'd to the rc-trace arm.
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
/// **The buffered entities are not touched**: a candidate can already be
/// gone. An arena entity dies with its arena without individual
/// teardown, so nothing calls [`forget_candidate`] for it and its buffer
/// entry outlives its memory. A pointer in this buffer is not evidence
/// that anything is still there.
///
/// **Known limit**: an entity genuinely alive and still buffered when
/// its thread dies keeps `CYCLE_COLLECTOR_BUFFERED` set while its block
/// goes to the abandoned list, so an adopting thread's release can never
/// re-buffer it and its cycle leaks. Cross-thread entity survival is
/// reserved today, so nothing reaches that case. Both, with the Miri
/// finding that refused the obvious repair, in `dev/DECISIONS.md`,
/// "thread exit owns the order its per-thread state dies in".
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
/// Refuses in two states, returning zero and leaving the arming alone: a
/// collection already running, which would recurse into the marker, and
/// a teardown in flight, where the dying entity is still a buffered root
/// at refcount zero and a collection would free it under the teardown
/// that then frees it again (`dev/DECISIONS.md`, "a fire point inside a
/// teardown collects nothing, and the runtime enforces it"). Neither
/// clears `COLLECT_PENDING`, so the next poll at a clean point collects.
///
/// # Safety
/// Must run at a **clean point** — where refcounts and physical edges agree
/// (between mutator operations), not mid-store or mid-teardown. That
/// invariant is the whole reason for the arm/fire split
/// (`rfc/model/gc/strategies.md`); `buffer_candidate` arms, this fires.
/// Single mutator thread parked here by construction (`rc-trace`).
pub unsafe fn collect_cycles() -> usize {
    if GC_ACTIVE.with(|a| a.get()) || TEARDOWN_DEPTH.with(|d| d.get()) != 0 {
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
                // The raw free below reclaims the entity's own slot and
                // nothing else, so every kind that owns memory *outside*
                // that slot needs an arm here or its body is lost with no
                // pointer left to it. Two do: a dynamic string's payload
                // and an array's table storage are separate allocations,
                // each a buffer chunk holding its block's live count
                // above zero for the life of the process, or an
                // OS-direct run.
                //
                // A string goes through its own teardown, which touches no
                // counts. An array cannot: `array_die` releases the
                // children first, and these children were already
                // trial-deleted, so it would decrement them twice. Only
                // the storage half of it applies.
                match (*w).flags & ENTITY_KIND_MASK {
                    k if k == EntityKind::WeakRef.to_flags() => {
                        crate::weak::weakref_die(w as *mut crate::weak::LLWeakRef)
                    }
                    k if k == EntityKind::String.to_flags() => {
                        crate::string::string_die(w as *mut crate::string::LLString)
                    }
                    k if k == EntityKind::Array.to_flags() => {
                        let a = w as *mut crate::array::entity::LLArray;
                        crate::array::entity::dispose_storage(
                            a,
                            crate::array::entity::category_of(a),
                        );
                        crate::memory::stdapi::ll_free(w as *mut u8);
                    }
                    // A class whose cells lie outside its body owns that
                    // storage the way an array owns its chunk, and this
                    // collector calls no `dispose`, so the group's own
                    // free is the only thing that reaches it.
                    k if k == EntityKind::Object.to_flags() => {
                        let cls = (*(w as *mut crate::object::Object)).class;
                        if let Some(group) = crate::class::Class::outside_cells(cls) {
                            (group.free)(w);
                        }
                        crate::memory::stdapi::ll_free(w as *mut u8);
                    }
                    _ => crate::memory::stdapi::ll_free(w as *mut u8),
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
            // Ordinary death through the kind switch: a white can be any
            // kind the trace reaches — an object, an array, a reference
            // box, a string, a weak cell. Only the candidate kinds ever
            // buffer, and the rest enter the component as their holders'
            // children.
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
/// before the run, and the trailing position is what makes it work: a pre-run
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
mod tests;
