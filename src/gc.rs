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
//! **Four of the five symbols survive the deletion because three of the
//! module's four duties are not the collector's**; the fifth, the collector
//! cap, is the embedder's dial over the collector threads. The checkpoint pair is the
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
    /// The collection this thread owes at its next clean point
    /// ([`Arming`]), as its value.
    ///
    /// Per thread because what arms it is per thread: the candidate
    /// queue's growth, which is one thread's release path drawing on
    /// one thread's reserve (`crate::cycle::queue`), and the byte the
    /// collector wrote into this thread's record. A process-wide word
    /// would send every thread through a collection because one of them
    /// ran short, and would leave the thread that needs the memory
    /// waiting behind threads that do not.
    ///
    /// `Cell<u8>` has no drop glue, which is the rule for anything a
    /// thread exit can reach (`memory::heap::ll_thread_exit`).
    static COLLECTION_ARMED: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// The collection a thread is armed for, ordered so that two armings merge
/// by the larger: a thread armed for R whole and for P collects once, over
/// R whole, P's roots ahead in the batch (`crate::cycle::queue::Batch`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub(crate) enum Arming {
    /// No collection owed.
    None = 0,
    /// No collection, and a retirement pass over R: completed deaths the
    /// free path counted to [`crate::cycle::queue::DEATHS_TO_RETIRE`] stand
    /// there with their slots withheld. Every collection retires them too,
    /// so every other arming outranks this one.
    Retire = 1,
    /// The collection over P: the collector's batch stands there, its
    /// release having written `POSTED` into this thread's byte
    /// (`crate::cycle::token::read_and_act_on_this_thread`).
    Verdicts = 2,
    /// The collection over R whole, with P disposed of in it.
    AllRoots = 3,
}

impl Arming {
    fn from_word(word: u8) -> Self {
        match word {
            0 => Self::None,
            1 => Self::Retire,
            2 => Self::Verdicts,
            _ => Self::AllRoots,
        }
    }
}

/// Arm this thread for a collection over R whole at its next clean point.
///
/// Two callers arm, neither of which can collect where it stands. The
/// pressure collection, at every ending that leaves a prefix of the lane
/// unread or that the gate refused, because what stands behind it is
/// garbage the poll has to read
/// (`crate::cycle::collect::collect_under_pressure`). And the poll itself,
/// when the collector has advanced the epoch and the deferred lane is
/// re-offered, so that the same safepoint traces the re-offered roots. The candidate
/// queue's growth arms nothing: a block the manager refused raises the
/// collector's signal (`crate::cycle::queue`). The third arming, for P
/// alone, is the byte's ([`arm_for_the_verdicts`]), and the lowest, a
/// retirement pass with no collection, is the free path's count's
/// ([`arm_to_retire`]). The arming is how the
/// poll hears about any of them (`rfc/model/gc/strategies.md`, "Collection
/// requests and triggers").
pub(crate) fn arm() {
    COLLECTION_ARMED.with(|armed| armed.set(Arming::AllRoots as u8));
}

/// Arm this thread for the collection over P, unless it is armed for more:
/// the reading of `POSTED` on the free path and at the poll
/// (`crate::cycle::token::read_and_act_on_this_thread`).
pub(crate) fn arm_for_the_verdicts() {
    COLLECTION_ARMED.with(|armed| armed.set(armed.get().max(Arming::Verdicts as u8)));
}

/// Arm this thread for the retirement pass, unless it is armed for more:
/// the free path's count of completed deaths
/// (`crate::cycle::queue::note_a_candidate_death`).
pub(crate) fn arm_to_retire() {
    COLLECTION_ARMED.with(|armed| armed.set(armed.get().max(Arming::Retire as u8)));
}

/// Lower an arming for P alone or for the retirement pass, keeping one for R
/// whole: a collection just disposed of P whole and retired R's completed
/// deaths, so a collection over P alone would open an empty window
/// (`crate::cycle::collect::CollectingThread`), and a pass would read what
/// the close has just read.
pub(crate) fn spend_an_arming_for_the_verdicts() {
    COLLECTION_ARMED.with(|armed| {
        if armed.get() <= Arming::Verdicts as u8 {
            armed.set(Arming::None as u8);
        }
    });
}

/// What this thread was armed for, and disarm it.
#[inline]
fn take_arming() -> Arming {
    Arming::from_word(COLLECTION_ARMED.with(|armed| armed.replace(0)))
}

/// Whether this thread is armed, without disarming it.
///
/// A test's only window on the arming: the poll disarms as it fires, so
/// a test that read the flag through the poll would read the same zero
/// whether the arming happened or not.
#[cfg(test)]
pub(crate) fn is_armed() -> bool {
    COLLECTION_ARMED.with(|armed| armed.get()) != 0
}

/// What this thread is armed for, without disarming it.
#[cfg(test)]
pub(crate) fn arming() -> Arming {
    Arming::from_word(COLLECTION_ARMED.with(|armed| armed.get()))
}

#[cfg(test)]
thread_local! {
    /// Collections over P this thread's polls fired, for the stress probe's
    /// count of `POSTED` skips against batches minus collections.
    static VERDICT_COLLECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Collections over P this thread's polls have fired so far.
#[cfg(test)]
pub(crate) fn verdict_collections_on_this_thread() -> usize {
    VERDICT_COLLECTIONS.with(|count| count.get())
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
    // A standing arming is spent where the fire can happen: the collection
    // reads R whole and disposes of P whole, which is everything either
    // arming asks for; a fire the gate refuses keeps it for the next poll.
    if crate::cycle::collect::may_collect() {
        let _ = take_arming();
    }

    unsafe { crate::cycle::collect::collect_off_the_poll() }
}

/// Fixture hook for the `benches/` driver, compiled under `bench-loads`
/// alone: re-offer this thread's deferred lane and answer how many records
/// moved. A root read live is deferred at a collection's close and the poll
/// re-offers it at the next turnover only, so a driver that collects the
/// same live ring eight times inside one epoch calls this before each
/// collection, as the exit's rounds do (`dev/BENCHMARKS.md`, 2026-09-12).
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
/// which arms nothing: the count of an owner's ring at which the
/// collector's round takes a batch (`crate::cycle::worker`,
/// `SOFT_THRESHOLD`); the poll's wake to the collector is sent on a block of
/// R filled, and decides nothing.
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
    // stands; then the overflow buffer drains into the room the refill
    // made. The exit's collection runs the same before each of its rounds
    // (`crate::cycle::queue::refill_and_drain`).
    crate::cycle::queue::refill_and_drain();

    // The gate, read once: a poll inside a teardown, a reset or a collection
    // fires nothing, and an arming made under it stands for the next clean
    // poll (`dev/DECISIONS.md`, "a fire point inside a teardown collects
    // nothing, and the runtime enforces it").
    let open = crate::cycle::collect::may_collect();

    // The deferred lane's turnover, under an open gate and before the token
    // is read. The collector keeps the clock and stores its low byte beside
    // the token at every advance; the owner alone moves its records, so the
    // poll compares that byte against the lane's mirror and splices the lane
    // where it moved, and arming here lets this same safepoint trace the
    // re-offered roots (`crate::cycle::worker`, "The epoch clock").
    if open
        && crate::cycle::queue::deferred_lane_is_occupied()
        && crate::cycle::queue::reoffer_deferred_if_epoch_moved()
    {
        arm();
    }

    // The returns a foreign trace left this thread withholding, made here so
    // that a thread which frees nothing after the holder let go still gives
    // them back at its next safepoint (`crate::cycle::deferred_slot_reuse`).
    // Before the byte is read: a return re-enters the free path, whose own
    // reading consents to a request that landed meanwhile, and a reading
    // taken before it would fire a take into that grant and wait its batch.
    unsafe { crate::cycle::deferred_slot_reuse::make_returns_withheld_under_a_foreign_trace() };

    // The byte, read by the one reading the slot free entry makes too, and
    // whatever the gate: a request is consented to and `POSTED` arms whether
    // or not this poll may fire (`crate::cycle::token`).
    let reading = crate::cycle::token::read_and_act_on_this_thread();

    if !open {
        return 0;
    }

    // An armed poll that read a collector tracing defers: the batch is the
    // trace taken off this thread, and the collection its verdicts owe is
    // fired one poll later, by `POSTED` at the next reading or by the arming
    // kept here — the second refusal that keeps an arming
    // (`rfc/dev/design/trace-token-handshake.md`, E9).
    if reading == crate::cycle::token::Reading::Collector {
        crate::cycle::queue::signal_the_collector_if_due();
        return 0;
    }

    // Armed, so fire, over what the arming names. The disarm happens whether
    // or not the fire collects anything, because an arming is an event and
    // not a state: a thread that stayed armed past a fire would fire at
    // every poll for the rest of its life. The gate above is the one refusal
    // that keeps the arming, and it is the one where no fire happened.
    crate::cycle::queue::take_retired_by_the_close();
    let freed = match take_arming() {
        Arming::None => 0,
        Arming::Retire => {
            unsafe { crate::cycle::queue::retire_at_the_poll() };
            0
        }
        Arming::Verdicts => {
            #[cfg(test)]
            VERDICT_COLLECTIONS.with(|count| count.set(count.get() + 1));
            unsafe { crate::cycle::collect::collect_over_the_verdicts() }
        }
        Arming::AllRoots => unsafe { ll_gc_collect_cycles() },
    };

    // What the collection freed or retired — a death retired out of P, or an
    // entity the collection a proposal armed reclaimed — is the collector's
    // timer's to read: a note on this thread's record, and no arming
    // (`crate::cycle::worker`, "The thread, and the round over the records").
    if freed > 0 || crate::cycle::queue::take_retired_by_the_close() > 0 {
        crate::cycle::queue::verdicts::note_freeing_disposition();
    }

    // The soft signal, last: a fire over R whole above read R and started
    // the count again, so a signal sent here is for entries still in R; a
    // fire over P alone read nothing of R and lowers no flag, the
    // block-filled wake being for an R it did not read. Either way the
    // round it starts meets no collection of this thread's at the token. A
    // wake, and no arming.
    crate::cycle::queue::signal_the_collector_if_due();
    freed
}

/// ABI: cap the collector threads the process may hold, from one to
/// `cycle::worker::MAX_COLLECTORS`, `cap` clamped into that range. The
/// embedder's one dial over the collectors: the runtime births siblings up
/// to it on its own reading of the backlog and ends them when idle
/// (`crate::cycle::worker`, "Siblings"). Without a call the cap is the
/// crate's default. Callable at any time from any thread; a lowered cap
/// ends the siblings above it at the elder's next round.
#[unsafe(no_mangle)]
pub extern "C" fn ll_gc_set_collector_cap(cap: usize) {
    crate::cycle::worker::set_collector_cap(cap);
}

/// ABI: set the longest a mutator's epoch stands before its collector
/// advances it, and with it re-offers the roots the mutator deferred, in
/// milliseconds; zero restores the crate's default (`crate::cycle::worker`,
/// "The epoch clock"). The embedder's dial over how long garbage behind a
/// deferred root may wait on a thread whose batches are few. Callable at any
/// time from any thread; the next round reads it.
#[unsafe(no_mangle)]
pub extern "C" fn ll_gc_set_epoch_interval(millis: u64) {
    crate::cycle::worker::set_epoch_interval(std::time::Duration::from_millis(millis));
}

/// ABI: set how long a mutator's candidate ring may stand non-empty below
/// the collector's serve threshold before the round takes it as an ordinary
/// batch, in milliseconds; zero restores the crate's default
/// (`dev/design/a-standing-r-is-taken-after-n-rounds.md`). The embedder's
/// dial over how long the garbage a thread registers at a rate below the
/// threshold waits, against one batch's window per interval on a thread
/// that keeps registering. Callable at any time from any thread: the
/// figure a round compares a standing ring's instant against is this one
/// from the call on.
#[unsafe(no_mangle)]
pub extern "C" fn ll_gc_set_standing_interval(millis: u64) {
    crate::cycle::worker::set_standing_interval(std::time::Duration::from_millis(millis));
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
/// and exact validation") — and the collector thread (`crate::cycle::worker`)
/// keeps it so: its batch is traced on its own arena under the mutator's
/// token, and nothing of the bracket's is asked of the mutator between the
/// two calls.
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
