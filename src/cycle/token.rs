//! The trace token: the per-mutator byte whose state says who may trace that
//! mutator's candidates and the entities the trace reaches — the arena, the
//! block triples, the touched list — and read its candidate ring (`rfc/model/gc/rc-cycle.md`, "Concurrency";
//! `rfc/dev/design/trace-token-handshake.md`, the ruled form).
//!
//! One byte per mutator thread, in the thread's record, whose storage
//! outlives the thread so that a collector may reach it before holding
//! anything (`crate::cycle::mutator_record`). Five states in the low three
//! bits, and the slot of the collector a request or a claim names in the
//! three above them ([`STATE_MASK`], [`SLOT_SHIFT`]):
//!
//! | state | who writes it | meaning |
//! |---|---|---|
//! | [`FREE`] | either | nobody traces this thread; the mutator returns memory at once |
//! | [`MUTATOR`] | the mutator | the mutator's own claim: a collection through its close, the exit's final claim, an initialisation not yet complete |
//! | [`REQUESTED`]`\|s` | collector s | collector s asks to trace; the mutator has not consented |
//! | [`COLLECTOR`]`\|s` | collector s, or the consenting mutator | collector s traces; the mutator withholds every return |
//! | [`POSTED`] | collector s | no collector holds anything; the last batch's verdicts stand in P undisposed of, and the mutator owes a collection over P |
//!
//! `FREE`, `MUTATOR` and `POSTED` carry slot zero, so a collector's request
//! expects exactly zero. Every transition is a compare-and-swap that names
//! the byte it expects, and a failed swap is acted on by the value it read
//! back, never inferred; the two exceptions are the releases, stores over a
//! value only their writer can change. The collector never writes
//! `COLLECTOR` itself: it requests ([`TraceToken::request`]) and the
//! mutator consents ([`read_and_act_on_this_thread`]) with a release swap,
//! which is what orders the mutator's stores before its reading against the
//! collector's loads after its grant, with no fence on either side
//! (`rfc/dev/design/trace-token-handshake.md`, E3; the loom model
//! `token/free_path_model.rs` exhibits the fenced form a claim without a
//! consent needed).
//!
//! **The mutator holds `MUTATOR` from its take through its close**, on the
//! path off the poll and under pressure alike: exact validation, the
//! destructors, the sever, the frees and the ring's compaction all run under
//! it, and the close's last store releases it. A collector's claim therefore
//! fails on a collecting mutator in one swap, and the record's collecting
//! word is the mutator's own gate and nobody else's. The one holder that
//! never releases is the exit, whose final claim stays on the record until
//! the next thread's initialisation ends
//! (`crate::cycle::mutator_record`, "The token is what says whether a record
//! is anyone's").
//!
//! **A waiter blocks rather than spins.** The mutator that finds its byte at
//! `COLLECTOR` waits on the mutex and is woken by the collector's release; a
//! trace runs no user code and takes no user lock (Edmond, 2026-08-29,
//! `rfc/dev/DECISIONS.md`, "a trace stays inside the blocks of the thread it
//! claimed"), and the waiter recalls the token before it waits, so the wait is
//! bounded by one stride of the trace, the batch's posts and one reset of the
//! collector's arena ([`TraceToken::take_unless`]) — when the batch traced is
//! the waiter's own; behind another mutator's batch it waits that batch out
//! (`crate::cycle::worker`, "The recall of the token"). Nobody waits on any other
//! state: a collector that meets `MUTATOR`, `REQUESTED` or `POSTED` skips.
//! Eligibility is checked before the wait: a thread the gate refuses — one
//! already collecting, inside a teardown, or inside a reset — opens no window
//! (`crate::cycle::collect::may_collect`), and of the three only the teardown
//! refusal touches the byte afterwards, for the retirement pass that rewrites
//! the ring (`crate::cycle::collect::collect_under_pressure`); the exit's own
//! collection runs with the gate open and waits through the same take
//! (`crate::cycle::collect::collect_before_exit`).
//!
//! **Why per thread.** No thread names an entity in another thread's blocks —
//! `thread_move` and `thread_clone` require the graph arriving in a thread to
//! hold no reference to what stays behind — so two traces of two threads never
//! meet in a block, a triple or a row, and the exclusion narrows to the thread
//! (`rfc/dev/DECISIONS.md`, "a trace stays inside the blocks of the thread it
//! claimed").

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};

/// Nobody traces this thread; the mutator returns memory at once.
pub(crate) const FREE: u8 = 0;
/// The mutator's own claim.
pub(crate) const MUTATOR: u8 = 1;
/// A collector asks to trace; the mutator has not consented.
pub(crate) const REQUESTED: u8 = 2;
/// A collector traces; the mutator withholds every return.
pub(crate) const COLLECTOR: u8 = 3;
/// The last batch posted verdicts the mutator has not disposed of.
pub(crate) const POSTED: u8 = 4;

/// The bits the state takes.
pub(crate) const STATE_MASK: u8 = 0b111;
/// The bit the slot starts at; three bits hold `MAX_COLLECTORS` of 8.
pub(crate) const SLOT_SHIFT: u32 = 3;

/// The state of a byte.
#[inline]
pub(crate) const fn state(word: u8) -> u8 {
    word & STATE_MASK
}

/// The collector slot a byte names: meaningful for `REQUESTED` and
/// `COLLECTOR`, zero for the rest.
#[inline]
pub(crate) const fn slot(word: u8) -> usize {
    (word >> SLOT_SHIFT) as usize
}

/// The byte of `state` naming collector `slot`.
#[inline]
pub(crate) const fn word(state: u8, slot: usize) -> u8 {
    state | ((slot as u8) << SLOT_SHIFT)
}

/// What a collector's withdrawal of its request found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Withdrawn {
    /// The request stood and is taken back; the collector holds nothing.
    Withdrawn,
    /// The mutator consented meanwhile: the collector holds `COLLECTOR|s`
    /// and serves the grant.
    Granted,
    /// The mutator took `MUTATOR` over the request: a refusal, and the
    /// collector holds nothing.
    TakenByTheMutator,
    /// `FREE`, or a value naming another slot: the record moved on — a life
    /// ended, a slot handed over — and the collector holds nothing.
    MovedOn,
}

/// What the mutator's reading of its own byte found, after acting on it.
///
/// **Production branches on [`Reading::Collector`] alone**, in
/// `gc::ll_gc_maybe_collect` and in
/// `deferred_slot_reuse::withhold_under_a_trace_or_make_returns`: withholding
/// is the one behaviour that differs, and every other state returns memory at
/// once. The four beside it are the byte's states under their own names, so a
/// case asserts which state the reading met instead of asserting over the raw
/// byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reading {
    /// This thread has no record: no collector can reach it.
    NoRecord,
    /// `FREE`: return memory at once.
    Free,
    /// `POSTED`: the collector's last batch stands in P; this thread is
    /// armed for the collection over it, and returns memory at once.
    Posted,
    /// `COLLECTOR|s`, found or just consented to: withhold every return.
    Collector,
    /// `MUTATOR`: this thread's own claim; its window decides.
    Mutator,
}

/// Where a mutator's take found the byte. Production acts the same on
/// both — every ending of a collection disposes of P whether or not the
/// take consumed `POSTED` — so the answer is read by tests alone, which
/// assert which state a take consumed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TookFrom {
    /// `FREE`, or a request refused: nothing stands in P.
    Free,
    /// `POSTED`: the collector's last batch stands in P, and this collection
    /// disposes of it.
    Posted,
}

/// The token of one mutator thread.
///
/// Three fields: the byte the compare-and-swaps act on, and the mutex and
/// condition variable a waiter blocks on. None of the three may carry drop
/// glue, because the token stands in a record that is written in place and
/// never dropped (`crate::cycle::mutator_record`); the assertion below holds
/// that on every target, and a target whose mutex is not futex-backed fails
/// there.
pub(crate) struct TraceToken {
    word: AtomicU8,
    /// Whether the mutator stands in this token's wait: set by its take
    /// before it waits out a collector's claim and cleared once the take
    /// returns, so that the collector's trace, which reads it every
    /// `crate::cycle::arena::RECALL_STRIDE` positions, stops and releases
    /// (`rfc/model/gc/rc-cycle.md`, "The recall of the token"). A hint and
    /// not a claim: a reading that missed the store costs one stride more,
    /// and nothing but the byte above decides who holds the token. Relaxed
    /// on both sides, since nothing is published beside it.
    waiting: AtomicBool,
    wait: Mutex<()>,
    released: Condvar,
    /// How many times a taker has gone to wait on this token. A case reads
    /// it because a take that never waited is indistinguishable, from
    /// outside, from one whose wait is a no-op. Counted per wait on the
    /// condition variable rather than per take: a spurious wakeup re-tests
    /// the byte and waits again, so a case asserts that the count moved and
    /// never what it reached.
    #[cfg(test)]
    waits: std::sync::atomic::AtomicUsize,
    /// Requests this mutator consented to, and requests it refused by a
    /// take of its own: the mutator's side of the ledger the stress probe
    /// balances against the collector's grants served and refusals read
    /// (`crate::cycle::worker::tests::under_stress`).
    #[cfg(test)]
    consents: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    refusals: std::sync::atomic::AtomicUsize,
}

const _: () = assert!(
    !std::mem::needs_drop::<TraceToken>(),
    "a record written in place and never dropped may carry no drop glue"
);

impl TraceToken {
    /// A token at `MUTATOR`, the state a record leaves the registry in
    /// (`crate::cycle::mutator_record`): the taker releases it when its
    /// initialisation is complete, and no claim succeeds before that.
    pub(crate) const fn new_held() -> Self {
        Self {
            word: AtomicU8::new(MUTATOR),
            waiting: AtomicBool::new(false),
            wait: Mutex::new(()),
            released: Condvar::new(),
            #[cfg(test)]
            waits: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            consents: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            refusals: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The byte now: a reading, not a claim, and stale by the time it is read
    /// unless the reader is the holder.
    ///
    /// An acquire, paired with every release store: what a reader does after
    /// reading `FREE` or `POSTED` — return a slot, read P — happens after
    /// every load and store of the trace or the batch that wrote the value.
    #[inline]
    pub(crate) fn read(&self) -> u8 {
        self.word.load(Ordering::Acquire)
    }

    /// Whether the mutator stands in this token's wait, asking a collector
    /// that traces for it to stop: the collector's reading of the recall.
    #[inline]
    pub(crate) fn mutator_waits(&self) -> bool {
        self.waiting.load(Ordering::Relaxed)
    }

    /// Whether a collector traces this thread now: the byte at `COLLECTOR`.
    ///
    /// The readers that never consent read it — the chunk gate, the block
    /// gate, the remote reclaim, the drains' per-pop tests
    /// (`crate::cycle::deferred_slot_reuse`) — and both stale directions
    /// are safe there: a holder that let go just after the read costs one
    /// return withheld until the mutator's next pop, and a request that
    /// landed just after it is granted by nobody but this thread's own
    /// consent, which comes after every store this thread made before it.
    #[inline]
    pub(crate) fn collector_is_tracing(&self) -> bool {
        state(self.read()) == COLLECTOR
    }

    /// Ask, as collector `slot`, to trace: one swap `FREE → REQUESTED|slot`,
    /// a release as well as an acquire on success, so that the link the
    /// collector wrote into its standing list before the request is seen by
    /// the registry through the exit's take of the request
    /// (`crate::cycle::worker::Standing`; `crate::cycle::mutator_record`,
    /// [`first_free_record`](crate::cycle::mutator_record)).
    /// The byte the swap read back on a refusal, which the caller acts on:
    /// `POSTED` is a mutator that has not disposed of the last batch, and
    /// every other value a holder or another collector's request. The failure
    /// is acquire too: a standing request answered on the free path is read
    /// through this failure as `COLLECTOR`, and the grant it reads must carry
    /// the stores the mutator's consent released.
    pub(crate) fn request(&self, slot: usize) -> Result<(), u8> {
        self.word
            .compare_exchange(
                FREE,
                word(REQUESTED, slot),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
    }

    /// Take back collector `slot`'s request: one swap `REQUESTED|slot →
    /// FREE`, and on its failure the read-back decides
    /// ([`Withdrawn`]). Relaxed on success: nothing was granted, so nothing
    /// of the mutator's is read after it; acquire on failure, since a grant
    /// read back here is served.
    pub(crate) fn withdraw(&self, slot: usize) -> Withdrawn {
        match self.word.compare_exchange(
            word(REQUESTED, slot),
            FREE,
            Ordering::Relaxed,
            Ordering::Acquire,
        ) {
            Ok(_) => Withdrawn::Withdrawn,
            Err(seen) if seen == word(COLLECTOR, slot) => Withdrawn::Granted,
            Err(seen) if state(seen) == MUTATOR => Withdrawn::TakenByTheMutator,
            Err(_) => Withdrawn::MovedOn,
        }
    }

    /// Consent, as the mutator, to the request `seen` reads: one swap
    /// `REQUESTED|s → COLLECTOR|s`, a release, so that every store this
    /// thread made before its reading is ordered before the collector's
    /// loads after its grant; then the wake of s. `Err` is the byte the
    /// swap read back instead, which the caller acts on.
    pub(crate) fn consent(&self, seen: u8) -> Result<(), u8> {
        debug_assert_eq!(state(seen), REQUESTED);
        let granted = word(COLLECTOR, slot(seen));
        self.word
            .compare_exchange(seen, granted, Ordering::Release, Ordering::Acquire)
            .map(|_| {
                #[cfg(test)]
                self.consents.fetch_add(1, Ordering::Relaxed);
                crate::cycle::worker::wake_for_the_byte(slot(seen));
            })
    }

    /// Release collector `slot`'s claim: one store — `POSTED` when the batch
    /// posted verdicts into P, `FREE` when it posted nothing — then the wake
    /// of the mutator, if it waits.
    ///
    /// The notify is made under the mutex so that a waiter which read the
    /// byte before this store and is about to wait cannot miss it.
    pub(crate) fn release_claim(&self, slot: usize, posted: bool) {
        debug_assert_eq!(
            self.word.load(Ordering::Relaxed),
            word(COLLECTOR, slot),
            "a release of a claim this collector does not hold"
        );
        self.word
            .store(if posted { POSTED } else { FREE }, Ordering::Release);
        let _guard = self
            .wait
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.released.notify_all();
    }

    /// [`take_unless`](Self::take_unless) with `hold_at_posted` off, expecting
    /// a take, for tests.
    #[cfg(test)]
    pub(crate) fn take(&self) -> TookFrom {
        self.take_unless(false)
            .expect("a take that holds at POSTED was not asked for")
    }

    /// Wait out a collector's claim once and answer the byte read after the
    /// wait.
    ///
    /// The lock is taken before the byte is re-read, and a claim gone by then
    /// is answered at once with the lock kept in `guard` for the next round:
    /// the release writes the byte under the same lock, so a claim still
    /// standing under it cannot end between this reading and the wait.
    fn wait_out_a_claim<'a>(&'a self, guard: &mut Option<MutexGuard<'a, ()>>) -> u8 {
        let held = guard.take().unwrap_or_else(|| {
            self.wait
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        });
        let seen = self.read();
        if state(seen) != COLLECTOR {
            *guard = Some(held);
            return seen;
        }

        #[cfg(test)]
        self.waits.fetch_add(1, Ordering::SeqCst);
        *guard = Some(
            self.released
                .wait(held)
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        self.read()
    }

    /// Take the token as the mutator, waiting while a collector holds it,
    /// and say where the take found the byte; the take behind
    /// [`HeldToken::take`].
    ///
    /// From `FREE` and `POSTED` the swap to `MUTATOR` is the take. From
    /// `REQUESTED|s` it is a refusal: the swap lands and collector `s` is
    /// woken to read it. Under `COLLECTOR` the mutator blocks on the mutex
    /// and is woken by [`release_claim`](Self::release_claim); the byte is
    /// re-read under the mutex, so a release between the read and the wait
    /// is not lost. `MUTATOR` is the caller's own claim, and the caller tells
    /// a nested take apart before calling ([`HeldToken::take`]). With
    /// `hold_at_posted` a byte read as `POSTED` — at the first read or after
    /// a wait — is left as it is and `None` is answered, which is the
    /// retirement pass's form, decided on the same read a swap would act on.
    ///
    /// **A take that meets `COLLECTOR` recalls the token first**: it sets
    /// [`mutator_waits`](Self::mutator_waits) before its first wait and clears
    /// it when it returns, so that the collector stops its trace within a
    /// stride of positions, posts and releases (`rfc/model/gc/rc-cycle.md`,
    /// "The recall of the token").
    pub(crate) fn take_unless(&self, hold_at_posted: bool) -> Option<TookFrom> {
        // Cleared on the unwind too: a recall left standing would stop every
        // later grant's trace at its first reading.
        struct ClearTheRecall<'a> {
            waiting: &'a AtomicBool,
            recalled: bool,
        }
        impl Drop for ClearTheRecall<'_> {
            fn drop(&mut self) {
                if self.recalled {
                    self.waiting.store(false, Ordering::Relaxed);
                }
            }
        }
        let mut clear = ClearTheRecall {
            waiting: &self.waiting,
            recalled: false,
        };
        self.take_recalling(hold_at_posted, &mut clear.recalled)
    }

    /// [`take_unless`](Self::take_unless) up to the clearing of the recall,
    /// which `recalled` says is owed.
    fn take_recalling(&self, hold_at_posted: bool, recalled: &mut bool) -> Option<TookFrom> {
        let mut guard = None;
        let mut seen = self.read();
        loop {
            let took = match state(seen) {
                FREE | REQUESTED => TookFrom::Free,
                POSTED if hold_at_posted => return None,
                POSTED => TookFrom::Posted,
                COLLECTOR => {
                    if !*recalled {
                        self.waiting.store(true, Ordering::Relaxed);
                        *recalled = true;
                    }

                    seen = self.wait_out_a_claim(&mut guard);
                    continue;
                }
                _ => unreachable!("a mutator's take under its own claim"),
            };
            match self
                .word
                .compare_exchange(seen, MUTATOR, Ordering::Acquire, Ordering::Acquire)
            {
                // A take over a standing request is this mutator's refusal,
                // and the collector is woken rather than left on its wait.
                Ok(_) if state(seen) == REQUESTED => {
                    #[cfg(test)]
                    self.refusals.fetch_add(1, Ordering::Relaxed);
                    crate::cycle::worker::wake_for_the_byte(slot(seen));
                    return Some(took);
                }
                Ok(_) => return Some(took),
                Err(actual) => seen = actual,
            }
        }
    }

    /// Release the mutator's own claim: the close's last store. Nobody waits
    /// on `MUTATOR`, so no wake follows.
    pub(crate) fn release(&self) {
        debug_assert_eq!(
            self.word.load(Ordering::Relaxed),
            MUTATOR,
            "a release of a claim the mutator does not hold"
        );
        self.word.store(FREE, Ordering::Release);
    }

    /// How many times a taker has gone to wait on this token so far.
    #[cfg(test)]
    pub(crate) fn waits(&self) -> usize {
        self.waits.load(Ordering::SeqCst)
    }

    /// Requests the mutator consented to so far.
    #[cfg(test)]
    pub(crate) fn consents(&self) -> usize {
        self.consents.load(Ordering::Relaxed)
    }

    /// Requests the mutator refused by a take of its own so far.
    #[cfg(test)]
    pub(crate) fn refusals(&self) -> usize {
        self.refusals.load(Ordering::Relaxed)
    }

    /// Whether somebody holds the token: the byte at `MUTATOR` or
    /// `COLLECTOR`. A case's reading; production reads the state it acts on.
    #[cfg(test)]
    pub(crate) fn is_held(&self) -> bool {
        matches!(state(self.read()), MUTATOR | COLLECTOR)
    }

    /// Set or clear the recall as the mutator's take does, for a case that
    /// traces on the calling thread against a recall standing.
    #[cfg(test)]
    pub(crate) fn recall_for_test(&self, waiting: bool) {
        self.waiting.store(waiting, Ordering::Relaxed);
    }

    /// Write `requested`, a `REQUESTED|s` byte, over `FREE`: a case standing
    /// in for a collector's request, which the collector thread makes with
    /// its own swap.
    #[cfg(test)]
    pub(crate) fn request_for_test(&self, requested: u8) {
        assert_eq!(state(requested), REQUESTED);
        self.word
            .compare_exchange(FREE, requested, Ordering::Acquire, Ordering::Relaxed)
            .expect("a request lands on a free byte");
    }

    /// Write `FREE` over `POSTED`: a case that batches again without a
    /// collection between, standing in for the disposition it makes by
    /// hand afterwards (`discard_standing_verdicts`). Nothing else writes
    /// `FREE` over `POSTED`.
    #[cfg(test)]
    pub(crate) fn clear_posted_for_test(&self) {
        let _ = self
            .word
            .compare_exchange(POSTED, FREE, Ordering::AcqRel, Ordering::Relaxed);
    }

    /// Claim `COLLECTOR|slot` over `FREE` in one swap, without a request or
    /// a consent, and say whether it landed: a case standing in for a
    /// collector on a mutator that is blocked in the case's own join, so
    /// that no store of the mutator's races the stand-in's loads.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn claim_for_test(&self, slot: usize) -> bool {
        self.word
            .compare_exchange(
                FREE,
                word(COLLECTOR, slot),
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
    }
}

/// The token of the calling thread, as a pointer a case standing in for a
/// collector holds from another thread. The collector thread itself reaches a token through the
/// record a round hands it (`crate::cycle::worker`), and no production path
/// takes the pointer.
///
/// The pointee is a line of the mutator's record, and the record's storage
/// outlives the thread (`crate::cycle::mutator_record`), so the pointer stays
/// valid after this thread exits; what a holder finds there after the exit's
/// final claim is a token at `MUTATOR` until the record's next thread
/// completes its initialisation and releases it. Null for a thread with no
/// record.
#[cfg(test)]
pub(crate) fn this_thread_token() -> *const TraceToken {
    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() {
        return std::ptr::null();
    }

    unsafe { &raw const (*record).token }
}

/// Whether a collector is tracing this thread's heap now
/// ([`TraceToken::collector_is_tracing`]). False for a thread with no record:
/// no collector can reach a token that does not exist.
#[inline]
pub(crate) fn collector_is_tracing_this_thread() -> bool {
    let record = crate::cycle::mutator_record::this_thread_record();
    !record.is_null() && unsafe { (*record).token.collector_is_tracing() }
}

/// The mutator's one reading of its byte, made by the slot free entry and
/// by the poll and by nothing else: one acquire load, and the act the value
/// asks for — at `REQUESTED|s` the consent and the wake, at `POSTED` the
/// arming for the collection over P — then the answer the caller decides
/// its return by ([`Reading`]). A swap that fails is acted on by the value
/// it read back.
///
/// It arms and does not collect because its first caller has no frame:
/// `ll_release` cannot raise and cannot run a destructor
/// (`crate::gc::arm`). The byte is not written at `POSTED`, so every
/// reading in the window arms again, an idempotent store into a
/// thread-local, until the collection's take consumes the state.
#[inline]
pub(crate) fn read_and_act_on_this_thread() -> Reading {
    let record = crate::cycle::mutator_record::this_thread_record();
    if record.is_null() {
        return Reading::NoRecord;
    }

    let token = unsafe { &(*record).token };
    let mut seen = token.read();
    loop {
        match state(seen) {
            FREE => return Reading::Free,
            POSTED => {
                crate::gc::arm_for_the_verdicts();
                return Reading::Posted;
            }
            COLLECTOR => return Reading::Collector,
            MUTATOR => return Reading::Mutator,
            _ => match token.consent(seen) {
                Ok(()) => return Reading::Collector,
                Err(actual) => seen = actual,
            },
        }
    }
}

/// The token of the calling thread, held from the call to the guard's drop:
/// the mutator's own claim.
///
/// **A take inside the mutator's own claim is nested and releases nothing**:
/// the byte reads `MUTATOR`, which only this thread writes, so the take is
/// under a claim of this thread's — a collection's, from its take through its
/// close, or the exit's, which claims its token once for good and runs its
/// collection rounds under that claim
/// (`crate::cycle::collect::collect_before_exit`) — and each inner take must
/// neither wait on the thread's own byte nor let go of it.
///
/// **A thread with no record holds nothing.** Its collection is refused
/// before any window opens (`crate::cycle::collect::CollectingThread`), and
/// the one work it takes the guard around — the teardown refusal's
/// retirement pass — runs untokened, which excludes no one: a collector
/// reaches a thread through its record, and this thread has none.
///
/// The drop releases on the unwind as well as on the return, so a panic
/// inside a collection leaves no claim standing for a collector to skip
/// forever. Not `Send`: the drop releases the byte of the thread it runs on,
/// and a guard moved to another thread would free that thread's token
/// instead.
#[must_use = "the token is released when this guard drops"]
pub(crate) struct HeldToken {
    /// The record whose token this guard releases on drop, or null for a
    /// nested take, a hold at `POSTED` and a thread without a record.
    releases: *mut crate::cycle::mutator_record::MutatorRecord,
    thread_bound: std::marker::PhantomData<*const ()>,
}

impl HeldToken {
    /// Take this thread's token, waiting while a collector holds it.
    pub(crate) fn take() -> Self {
        Self::take_unless(false)
    }

    /// Take this thread's token as [`take`](Self::take) does, except that a
    /// byte at `POSTED` is left as it is and nothing is held: the retirement
    /// pass's form. Under `POSTED` no collector holds anything and a claim
    /// fails, so the pass may rewrite the rings under it; and the byte left
    /// standing is what makes the next reading arm the collection over P
    /// that the pass, run with the gate closed, cannot be. The byte is
    /// decided on the read the swap acts on, after any wait, so a collector
    /// that releases `POSTED` into this take is held at `POSTED` too.
    pub(crate) fn take_or_hold_posted() -> Self {
        Self::take_unless(true)
    }

    fn take_unless(hold_at_posted: bool) -> Self {
        let record = crate::cycle::mutator_record::this_thread_record();
        if record.is_null() || state(unsafe { (*record).token.read() }) == MUTATOR {
            return Self::holding_nothing();
        }

        match unsafe { (*record).token.take_unless(hold_at_posted) } {
            Some(_) => Self {
                releases: record,
                thread_bound: std::marker::PhantomData,
            },
            None => Self::holding_nothing(),
        }
    }

    fn holding_nothing() -> Self {
        Self {
            releases: std::ptr::null_mut(),
            thread_bound: std::marker::PhantomData,
        }
    }

    /// Keep the claim past the guard: the token stays at `MUTATOR`, and
    /// nothing releases it. The exit's final claim
    /// (`crate::cycle::mutator_record::release_thread_record`).
    pub(crate) fn keep(self) {
        std::mem::forget(self);
    }
}

#[cfg(test)]
thread_local! {
    /// Whether the traced mutator's token was held at the trace's last row
    /// read — the scan's end, and the harvest sweep under pressure — since a
    /// case last asked. The upper edge of what the token covers, which no
    /// destructor can observe.
    static HELD_AT_LAST_ROW_READ: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };

    /// The record of the mutator whose candidates, and the entities the trace
    /// reaches, this thread is tracing as a collector, null while it traces
    /// as a mutator: the token the probe
    /// above reads is that mutator's rather than this thread's own.
    static TRACED_MUTATOR: std::cell::Cell<*mut crate::cycle::mutator_record::MutatorRecord> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// Name the mutator whose candidates, and the entities the trace reaches,
/// the calling collector thread traces under a foreign claim, or null once its trace is over, and do nothing at all
/// without `cfg(test)`.
///
/// Called by `cycle::worker` around its trace, and by the verdict ring's
/// test collector (`cycle::queue::verdicts::testing`).
#[inline]
pub(crate) fn note_traced_mutator(record: *mut crate::cycle::mutator_record::MutatorRecord) {
    #[cfg(test)]
    TRACED_MUTATOR.with(|cell| cell.set(record));
    #[cfg(not(test))]
    let _ = record;
}

/// Record whether the token is held — at `MUTATOR` or `COLLECTOR` — at the
/// reading that ends a trace's row reads, and do nothing at all without
/// `cfg(test)`. The token is the traced mutator's: this thread's own unless
/// [`note_traced_mutator`] named another.
///
/// Called by `cycle::trace` at the scan's end and by the arena's harvest
/// sweep, and by nothing else.
#[inline]
pub(crate) fn note_last_row_read() {
    #[cfg(test)]
    {
        let mut record = TRACED_MUTATOR.with(std::cell::Cell::get);
        if record.is_null() {
            record = crate::cycle::mutator_record::this_thread_record();
        }

        let held = !record.is_null()
            && matches!(
                state(unsafe { (*record).token.read() }),
                MUTATOR | COLLECTOR
            );
        HELD_AT_LAST_ROW_READ.with(|cell| cell.set(Some(held)));
    }
}

/// The last reading [`note_last_row_read`] made, and clear it.
#[cfg(test)]
pub(crate) fn take_held_at_last_row_read() -> Option<bool> {
    HELD_AT_LAST_ROW_READ.with(|held| held.take())
}

impl Drop for HeldToken {
    fn drop(&mut self) {
        if self.releases.is_null() {
            return;
        }

        unsafe { (*self.releases).token.release() };
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;

// The loom model of the free path's reading against a claim is not run by
// the suite: it exists only under `--cfg loom`, where the dev-dependency
// exists too. How to run it, and what it demonstrated, are in the file.
#[cfg(loom)]
mod free_path_model;
