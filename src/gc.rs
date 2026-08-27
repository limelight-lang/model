//! The GC ABI and the safepoint.
//!
//! What the compiler emits calls to, and what those calls owe the rest of
//! the runtime. The collector behind them is `rc-cycle`
//! (`rfc/model/gc/rc-cycle.md`), which is not built yet: the two strategies
//! that used to live here — `rc-trace`'s candidate buffer and trial
//! deletion, and `rc-walk`'s epoch handshake — were deleted on 2026-08-26
//! (`dev/DECISIONS.md`), and the code is on the branch
//! `archive/pre-rc-cycle`.
//!
//! **The four symbols survive the deletion because three of the module's
//! four duties are not the collector's.** The checkpoint pair is the
//! configuration-independent lowering surface: generated code brackets a
//! run of batched releases with them in every build, so the pair is
//! exported whether or not it does anything (`object.rs`,
//! `ll_release_batch`). The poll refills the store barrier's log reserve,
//! and is the only place that can. And the explicit fire is named by the
//! RFC as a symbol rather than as a mechanism
//! (`rfc/model/gc/strategies.md`, "Triggering: arm vs fire").
//!
//! Until stage S36.7 wires `rc-cycle` in, the two collecting entries
//! collect nothing and report zero. Cyclic garbage is retained; acyclic
//! garbage dies by counting as it always did.

/// ABI: run a cycle collection now, whether or not one was armed. Returns
/// entities reclaimed.
///
/// Reports zero: no collector is wired. The body arrives with stage S36.7.
///
/// # Safety
/// Callable at a safepoint of the calling mutator — refcounts and edges
/// consistent (`rfc/model/gc/strategies.md`, "Triggering: arm vs fire").
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_gc_collect_cycles() -> usize {
    0
}

/// ABI: fire a collection only if one was *armed*, else do nothing. This is
/// the poll the compiler injects at the safepoints it chooses — statement
/// boundary, allocation slow path, request end (`rfc/model/gc/strategies.md`,
/// §2 and the arm/fire split). The arming *policy* — which signals, which
/// thresholds — is the compiler's decision, outside this crate; the runtime
/// records "due" and collects here, where the graph is clean.
///
/// Collects nothing today, and refills the reserve regardless: the refill
/// is not conditional on there being a collector.
///
/// # Safety
/// Callable at a safepoint of the calling mutator.
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

    // The critical reserve refills here too, and for a reason of its own:
    // a collection returns what it drew at its own end, so the reserve is
    // usually full by the time this runs, and what it catches is the
    // collection that ended by refusing — the retry after an abort wants
    // a door that is open (`rfc/model/memory/critical-reserve.md`,
    // "Filling, refilling, and leaving reserve mode").
    if crate::memory::critical::is_drawn() {
        let _ = crate::memory::critical::replenish();
    }

    0
}

/// ABI: serve the collector's checkpoint now. The compiler emits it once
/// **after** a run of [`ll_release_batch`](crate::refcount::ll_release_batch)
/// calls (a scope exit), paired with one [`ll_gc_checkpoint_ack`] before the
/// run.
///
/// A no-op, and exported anyway so lowering is configuration-independent:
/// generated code carries the bracket in every build, and deleting one half
/// of an emitted pair would rewrite the calling convention to save nothing.
/// `rc-cycle` has no handshake to serve here — the in-line collection is
/// exact by construction (`rfc/model/gc/rc-cycle.md`, "Who judges") — so
/// whether this stays empty forever is settled when the collector-thread
/// accelerator is built, not before.
///
/// # Safety
/// Callable at a safepoint of the mutator.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_gc_checkpoint() {}

/// ABI: the opening half of the bracket [`ll_gc_checkpoint`] closes. The
/// compiler emits it once **before** a run of
/// [`ll_release_batch`](crate::refcount::ll_release_batch) calls.
///
/// A no-op, exported for the same reason as its twin.
///
/// # Safety
/// Callable anywhere on a mutator thread: it runs no user code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_gc_checkpoint_ack() {}
