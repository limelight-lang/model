//! The GC ABI and the safepoint.
//!
//! What the compiler emits calls to, and what those calls owe the rest of
//! the runtime. The collector behind them is `rc-cycle`
//! (`rfc/model/gc/rc-cycle.md`), and the order it runs in is
//! `crate::cycle::collect`. The two strategies this module once carried —
//! `rc-trace`'s candidate buffer and trial deletion, and `rc-walk`'s epoch
//! handshake — are on the branch `archive/pre-rc-cycle`, and why they went is
//! `dev/DECISIONS.md`, "what the old collectors left behind is deleted, and
//! what is kept is named".
//!
//! **The four symbols survive the deletion because three of the module's
//! four duties are not the collector's.** The checkpoint pair is the
//! configuration-independent lowering surface: generated code brackets a
//! run of batched releases with them in every build, so the pair is
//! exported whether or not it does anything (`object.rs`,
//! `ll_release_batch`). The poll refills the store barrier's log reserve,
//! and is the only place that can. And the explicit fire is named by the
//! RFC as a symbol rather than as a mechanism
//! (`rfc/model/gc/strategies.md`, "Collection requests and triggers").
//!
//! The two collecting entries run the collection that keeps its rows through
//! its teardown, which is the path of a caller that is not short of memory.
//! The one an allocation failure starts is `crate::cycle::collect`'s other
//! entry and reaches this module through no symbol.

thread_local! {
    /// Whether this thread owes a collection at its next clean point.
    ///
    /// Per thread because what arms it is per thread: the candidate
    /// queue's growth, which is one thread's release path drawing on
    /// one thread's reserve (`crate::cycle::queue`). A process-wide bit
    /// would send every thread through a collection because one of them
    /// ran short, and would leave the thread that needs the memory
    /// waiting behind threads that do not.
    ///
    /// `Cell<bool>` has no drop glue, which is the rule for anything a
    /// thread exit can reach (`memory::heap::ll_thread_exit`).
    static COLLECTION_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Arm this thread for a collection at its next clean point.
///
/// Four callers arm, none of which can collect where it stands. The
/// candidate queue, when a spare cell runs out of room — a reserve draw, or a
/// refusal at both allocation paths — because `ll_release` holds no frame
/// (`crate::cycle::queue`). The pressure collection, at every ending that
/// leaves a prefix of the lane unread or that the gate refused, because what
/// stands behind it is garbage the poll has to read
/// (`crate::cycle::collect::collect_under_pressure`). And the poll itself,
/// when the epoch has turned over and the deferred lane is re-offered, so
/// that the same safepoint traces the re-offered roots, and again when its
/// reading of the verdict ring stopped at a proposed or unwalked root, which
/// the collection it fires reads into its batch
/// (`crate::cycle::queue::verdicts`). The arming is how the
/// poll hears about any of them (`rfc/model/gc/strategies.md`, "Collection
/// requests and triggers").
pub(crate) fn arm() {
    COLLECTION_ARMED.with(|armed| armed.set(true));
}

/// Whether this thread was armed, and disarm it.
#[inline]
fn take_arming() -> bool {
    COLLECTION_ARMED.with(|armed| armed.replace(false))
}

/// Whether this thread is armed, without disarming it.
///
/// A test's only window on the arming: the poll disarms as it fires, so
/// a test that read the flag through the poll would read the same zero
/// whether the arming happened or not.
#[cfg(test)]
pub(crate) fn is_armed() -> bool {
    COLLECTION_ARMED.with(|armed| armed.get())
}

/// Lower the flag, for a case whose subject is an arming.
///
/// A fixture arms this thread as a side effect — the queue's growth draws the
/// reserve, and a draw arms — so a case that means to see its own arming
/// starts from a flag it lowered rather than from one it assumed down.
#[cfg(test)]
pub(crate) fn disarm() {
    take_arming();
}

/// ABI: run a cycle collection now, whether or not one was armed. Returns
/// entities reclaimed.
///
/// The collection keeps its rows through the teardown, which is the path of a
/// caller that is not short of memory
/// (`crate::cycle::collect::collect_off_the_poll`). Zero is every answer short
/// of a teardown, the refusals included.
///
/// **A destructor this runs can ask for the thread's exit**, and the request
/// waits for the thread's top rather than running here: the caller gets its
/// heap back, and `memory::heap::thread_exit_pending` says a request stands.
///
/// # Safety
/// Callable at a safepoint of the calling mutator — refcounts and edges
/// consistent (`rfc/model/gc/strategies.md`, "Collection requests and triggers").
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_gc_collect_cycles() -> usize {
    unsafe { crate::cycle::collect::collect_off_the_poll() }
}

/// Fixture hook for the `benches/` driver, compiled under `bench-loads`
/// alone: re-offer this thread's deferred lane and answer how many records
/// moved. A root read live is deferred at a collection's close and the poll
/// re-offers it at the next turnover only, so a driver that collects the
/// same live ring eight times inside one epoch calls this before each
/// collection, as the exit's rounds do (`PLAN.md` S40.3).
///
/// The body is the count the census reads and the call the exit already
/// makes, so no production function computes anything for the hook and the
/// ordinary library carries no such symbol.
#[cfg(feature = "bench-loads")]
#[unsafe(no_mangle)]
pub extern "C" fn ll_gc_reoffer_deferred() -> usize {
    let records = crate::cycle::queue::deferred_count();
    crate::cycle::queue::reoffer_deferred_candidates();
    records
}

/// ABI: fire a collection only if one was *armed*, else do nothing. This is
/// the poll the compiler injects at the safepoints it chooses — statement
/// boundary, allocation slow path, request end (`rfc/model/gc/strategies.md`,
/// §2 and the arm/fire split). The arming *policy* — which signals, which
/// thresholds — is the compiler's decision, outside this crate; the runtime
/// records the arming and collects here, where the graph is clean. The one
/// threshold the runtime owns is the collector thread's soft threshold,
/// which arms nothing: it is the count of this thread's registrations at
/// which the poll wakes the collector, and the count of an owner's ring at
/// which the collector's round takes a batch (`crate::cycle::worker`,
/// `SOFT_THRESHOLD`).
///
/// The reserve refills and queue maintenance below happen whether or not the
/// fire does, an unarmed poll being the ordinary case and the maintenance
/// being what every poll owes.
///
/// **A destructor the fire runs can ask for the thread's exit**, as under
/// [`ll_gc_collect_cycles`]: the request waits for the thread's top.
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
    // an allocation path that is open (`rfc/model/memory/critical-reserve.md`,
    // "Reserve lifecycle").
    if crate::memory::critical::is_drawn() {
        let _ = crate::memory::critical::replenish();
    }

    // And the candidate queue's spare cells, which is the same protocol
    // one layer up: the growth path may not allocate, so somebody else
    // takes the segment it swaps in, and this is where that somebody
    // stands — a block the ring has to spare behind its tail block fills a
    // short cell first; then the overflow buffer drains into the room the refill
    // made. The exit's collection runs the same three before each of its
    // rounds (`crate::cycle::queue::refill_and_drain`).
    crate::cycle::queue::refill_and_drain();

    // And the returns a foreign trace left this thread withholding, made
    // here so that a thread which frees nothing after the holder let go
    // still gives them back at its next safepoint
    // (`crate::cycle::deferred_slot_reuse`).
    unsafe { crate::cycle::deferred_slot_reuse::make_returns_withheld_under_a_foreign_trace() };

    // A ring whose root sits in the deferred lane can be this thread's only
    // garbage, so it cannot wait for a collection that an empty active queue
    // would never start. The owner alone compares its full-width mirror and
    // moves the records; arming here lets this same safepoint trace the
    // re-offered roots.
    if crate::cycle::queue::reoffer_deferred_if_epoch_moved(crate::cycle::epoch::commits()) {
        arm();
    }

    // The gate before the arming: a poll inside a teardown, a reset or a
    // collection cannot fire, and it leaves the arming standing for the next
    // poll at a clean point rather than spending it on a refusal
    // (`dev/DECISIONS.md`, "a fire point inside a teardown collects nothing,
    // and the runtime enforces it").
    if !crate::cycle::collect::may_collect() {
        return 0;
    }

    // The collector's verdicts, read behind the gate so that a closed-gate
    // poll reads none: P's prefix of completed deaths and roots read live is
    // disposed of here, and the first proposed or unwalked root arms the
    // collection this same poll fires, which reads it into its batch
    // (`crate::cycle::queue::verdicts`).
    let reading =
        crate::cycle::queue::verdicts::dispose_prefix_at_the_poll(crate::cycle::epoch::commits());
    if reading.proposal_stands {
        arm();
    }

    // Armed, so fire. The disarm happens whether or not the fire collects
    // anything, because an arming is an event and not a state: a thread that
    // stayed armed past a fire would fire at every poll for the rest of its
    // life. The gate above is the one refusal that keeps the arming, and it
    // is the one where no fire happened.
    let freed = if take_arming() {
        unsafe { ll_gc_collect_cycles() }
    } else {
        0
    };

    // What the disposition freed — a death retired out of P, or an entity
    // the collection a proposal armed reclaimed — is the collector's timer's
    // to read: a note on this thread's record, and no arming
    // (`crate::cycle::worker`, "The thread, and the round over the records").
    if reading.retired > 0 || (reading.proposal_stands && freed > 0) {
        crate::cycle::queue::verdicts::note_freeing_disposition();
    }

    // The soft signal, last: a fire above read R whole and started the
    // count again, so a signal sent here is for entries still in R, and the
    // round it starts meets no collection of this thread's at the token. A
    // wake, and no arming.
    crate::cycle::queue::signal_the_collector_if_due();
    freed
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
/// exact by construction (`rfc/model/gc/rc-cycle.md`, "Speculative tracing
/// and exact validation") — so
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
