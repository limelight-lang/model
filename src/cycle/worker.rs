//! The collector thread, and what it does for one owner: take the chain the
//! owner's poll offered, trace it through the collector's reader, mark the
//! roots whose components read potentially unreachable, and post the chain
//! back for the owner's exact reading (`rfc/model/gc/rc-cycle.md`,
//! "Worker-to-owner handoff"; `rfc/dev/DECISIONS.md`, "the owner detaches at
//! its poll, and the worker takes the chain from a one-word outbox").
//!
//! # The round over one owner
//!
//! The outbox word is read first, under no claim, and null is a skip. Set,
//! the owner's token is claimed by compare-and-swap — held is a skip — and
//! only then is anything of the owner's touched: an unpicked proposal in the
//! inbox is a skip, since the inbox holds one chain; the outbox is exchanged
//! with null, and a null answer is the owner having reclaimed the offer for a
//! collection of its own, another skip. The chain taken is traced through
//! `cells::AtomicCells` over a workspace of the collector thread's own, and
//! **it is always posted, walked or not** — a trace the pool refuses posts the
//! chain unmarked, a trace that panics posts it from the unwind, and the
//! owner's close puts every unmarked root back in its lane — before the
//! token's release store, so that an owner reading its token free finds the
//! chain in its inbox. A trace that did not complete marks nothing: its
//! colours are no verdict. The marks are the only thing written into the
//! owner's memory here, and they are written over entries of a chain nobody
//! else holds.
//!
//! # The thread, and the round over the records
//!
//! One collector thread per process, born at the end of the first collection
//! an allocation failure started and never at startup (`dev/DECISIONS.md`,
//! "the collector thread is born at the first pressure collection"): the
//! pressure path is the one fire point the runtime owns, and a thread born
//! there costs nothing to a process that never runs short. It starts as any
//! registered thread does, through `ll_thread_init`, whose base block draw
//! can be refused; a refused base block is a thread that never started, and
//! a pressure collection [`BIRTH_RETRY_INTERVAL`] or more after the refusal
//! births again. A round walks every record the registry has carved, serves
//! the owners whose outboxes are set, and asks an owner whose outbox is
//! empty for an offer only where that owner noted a shortage since the last
//! ask — the request is a relay of the owner's own pressure collection, so
//! the thread originates no trace of its own and a thread that never runs
//! short is never traced (`dev/DECISIONS.md`, "the worker relays the owner's
//! shortage into its request"); the owner's next unarmed poll makes the
//! offer ([`round`]). Between two rounds the thread sleeps for
//! [`ROUND_INTERVAL`], which bounds how long an offer or a note waits and
//! nothing else. Nothing wakes the thread early, and nothing but a test ends
//! it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::cells::AtomicCells;
use crate::cycle::arena::{TraceScratchArena, find_initialized_row};
use crate::cycle::owner_record::{self, OwnerRecord};
use crate::cycle::queue::InFlightBatch;
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::shadow::{self, Color};
use crate::cycle::trace::{ALL_ROOTS, TraceOutcome, trace_batch};

/// What one round over one owner did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Served {
    /// The outbox read empty.
    NothingOffered,
    /// The owner, or another collector, holds the token.
    TokenHeld,
    /// A chain posted earlier is not yet picked up.
    ProposalUnpicked,
    /// The owner reclaimed the offer between the read and the take.
    Reclaimed,
    /// The chain was traced and posted, `proposed` of its `roots` marked.
    Posted { roots: usize, proposed: usize },
    /// The chain was posted unmarked: the collector thread's workspace or
    /// the trace's rows could not be had.
    PostedUntraced,
}

/// The pause between two rounds over the records.
const ROUND_INTERVAL: Duration = Duration::from_millis(10);

/// How long after a refused birth the pressure path waits before it spawns
/// again: a process that stays short of memory collects at every refused
/// allocation, and without the wait it would spawn a thread per refusal.
const BIRTH_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// When the last birth was refused — a spawn the operating system refused,
/// or a base block the pool refused — or `None`.
static REFUSED_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// The name the collector thread is spawned under.
const THREAD_NAME: &str = "ll-collector";

/// Where the process's collector thread stands: [`UNBORN`], [`STARTING`]
/// from the spawn until its `ll_thread_init` answered, [`ALIVE`] from a
/// started init until the thread ends.
static THREAD: AtomicU8 = AtomicU8::new(UNBORN);
const UNBORN: u8 = 0;
const STARTING: u8 = 1;
const ALIVE: u8 = 2;

/// Start the collector thread unless the process has one already, one is
/// starting, or a birth was refused less than [`BIRTH_RETRY_INTERVAL`] ago.
/// The pressure path's call, made after its collection so that the thread's
/// draws compete with no rows of this thread's own. A spawn the operating
/// system refuses, and a base block the pool refuses, each leave the process
/// without a thread until a call after the interval.
///
/// The spawn allocates through the global allocator — the thread's name and
/// the handle's shared state — on a path the ruling that no runtime path may
/// end the process on an allocation the manager could have refused forbids
/// it; `PLAN.md`'s backlog carries the debt.
pub(crate) fn ensure_thread() {
    #[cfg(test)]
    if !testing::births_permitted() {
        return;
    }

    // The load before the exchange keeps every pressure collection after the
    // birth off a read-modify-write of the word.
    if THREAD.load(Ordering::Relaxed) != UNBORN
        || birth_refused_recently()
        || THREAD
            .compare_exchange(UNBORN, STARTING, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
    {
        return;
    }

    match std::thread::Builder::new()
        .name(THREAD_NAME.into())
        .spawn(thread_body)
    {
        Ok(handle) => {
            #[cfg(test)]
            testing::keep_handle(handle);
            #[cfg(not(test))]
            drop(handle);
        }
        Err(_) => {
            note_refused_birth();
            THREAD.store(UNBORN, Ordering::Release);
        }
    }
}

/// Whether a birth was refused less than [`BIRTH_RETRY_INTERVAL`] ago.
fn birth_refused_recently() -> bool {
    let refused_at = REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    refused_at.is_some_and(|at| at.elapsed() < BIRTH_RETRY_INTERVAL)
}

fn note_refused_birth() {
    let mut refused_at = REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *refused_at = Some(Instant::now());
}

/// The collector thread's life: its registration, its rounds, its exit.
fn thread_body() {
    // The word goes back to unborn however this thread ends — a refused
    // base block, a test's retire, or a panic in a round that unwinds out of
    // here — so that a later pressure collection can birth again rather than
    // read a thread that no longer exists.
    struct UnbornOnDrop;
    impl Drop for UnbornOnDrop {
        fn drop(&mut self) {
            THREAD.store(UNBORN, Ordering::Release);
        }
    }
    let _unborn = UnbornOnDrop;

    let started = {
        #[cfg(test)]
        let _budget = testing::base_block_budget_for_this_birth();
        crate::memory::heap::ll_thread_init()
    };
    if !started {
        // The base block was refused, so this is a thread that never started
        // (`rfc/dev/DECISIONS.md`, "the baseline overflow segment is
        // allocator-issued"); a pressure collection after the interval
        // births again.
        note_refused_birth();
        return;
    }

    THREAD.store(ALIVE, Ordering::Release);
    while !retiring() {
        round();
        std::thread::park_timeout(ROUND_INTERVAL);
    }

    crate::memory::heap::ll_thread_exit();
}

/// Clear the last refusal, so that a case's birth is not held by the
/// refusal of the case before it.
#[cfg(test)]
fn forget_refused_birth() {
    let mut refused_at = REFUSED_AT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *refused_at = None;
}

/// Whether a test asked the thread to end; false in every other build.
#[cfg(not(test))]
fn retiring() -> bool {
    false
}

#[cfg(test)]
use testing::retiring;

/// One round over the records: serve every owner whose outbox is set, and
/// ask every owner whose outbox is empty and whose shortage note is set for
/// an offer, taking the note. A round that serves an offer leaves the note
/// for the next round. The thread's own record is skipped, since it polls
/// nothing; a record on the free list carries no note, its taker having
/// cleared it.
fn round() {
    let own = owner_record::this_thread_record();
    owner_record::for_each_record(|record| {
        if record == own {
            return;
        }

        #[cfg(test)]
        if !testing::in_round(record) {
            return;
        }

        let served = unsafe { serve(record) };
        #[cfg(test)]
        testing::note_served(served);
        if served == Served::NothingOffered && unsafe { owner_record::take_shortage(record) } {
            unsafe { owner_record::request(record) };
        }
    });
}

/// Serve `record`'s owner once: take its offer under its token, trace it,
/// mark, post, release.
///
/// Runs on a collector thread, which holds a base block of its own for the
/// workspace the trace opens (`crate::memory::heap::ll_thread_init`).
///
/// # Safety
/// `record` is a record of the registry's, and the calling thread is not its
/// owner.
pub(crate) unsafe fn serve(record: *mut OwnerRecord) -> Served {
    if !unsafe { owner_record::offer_stands(record) } {
        return Served::NothingOffered;
    }

    let token = unsafe { &(*record).token };
    if !token.try_take() {
        return Served::TokenHeld;
    }

    // Released on the unwind too: a collector that panicked mid-trace would
    // otherwise leave the owner's exit waiting forever.
    struct ReleaseOnDrop<'a>(&'a crate::cycle::token::TraceToken);
    impl Drop for ReleaseOnDrop<'_> {
        fn drop(&mut self) {
            crate::cycle::token::note_traced_owner(std::ptr::null_mut());
            self.0.release();
        }
    }
    let _held = ReleaseOnDrop(token);
    crate::cycle::token::note_traced_owner(record);

    if unsafe { owner_record::proposal_stands(record) } {
        return Served::ProposalUnpicked;
    }

    let Some(mut taken) = (unsafe { TakenChain::take(record) }) else {
        return Served::Reclaimed;
    };

    match TraceScratchArena::open() {
        Some(mut arena) => {
            let served = unsafe { trace_and_mark(&mut arena, taken.batch()) };
            // The rows go back and every shadow pointer the trace stamped on
            // the owner's blocks is nulled before the chain is posted: the
            // owner's own next trace stamps them afresh.
            arena.reset();
            served
        }
        None => Served::PostedUntraced,
    }
    // `taken` posts as it drops, after the arena's reset and before `_held`
    // releases the token: the declaration order is the post's order.
}

/// A chain taken out of an owner's outbox, posted to the owner's inbox when
/// this drops — on the return and on the unwind alike, so a trace that
/// panics still hands every root back. Declared after the token guard so
/// that it drops first and the post precedes the release.
struct TakenChain {
    record: *mut OwnerRecord,
    batch: Option<InFlightBatch>,
}

impl TakenChain {
    /// Exchange `record`'s outbox with null and hold what it named, or `None`
    /// when the owner reclaimed the offer meanwhile.
    ///
    /// # Safety
    /// The caller holds `record`'s token.
    unsafe fn take(record: *mut OwnerRecord) -> Option<Self> {
        let word = unsafe { owner_record::take_offer(record) };
        if word == 0 {
            return None;
        }

        Some(Self {
            record,
            batch: Some(InFlightBatch::from_word(word, false)),
        })
    }

    fn batch(&mut self) -> &mut InFlightBatch {
        self.batch
            .as_mut()
            .expect("the chain is posted only at the drop")
    }
}

impl Drop for TakenChain {
    fn drop(&mut self) {
        let batch = self.batch.take().expect("the chain is posted once");
        unsafe { owner_record::post(self.record, batch.into_word()) };
    }
}

/// Trace `batch` through the collector's reader and mark the roots the scan
/// read potentially unreachable.
///
/// # Safety
/// The calling thread holds the owner's token, and `batch` is the chain it
/// took from the owner's outbox.
unsafe fn trace_and_mark(arena: &mut TraceScratchArena, batch: &mut InFlightBatch) -> Served {
    let (outcome, roots) = unsafe { trace_batch::<AtomicCells>(arena, batch, ALL_ROOTS) };
    if outcome != TraceOutcome::Complete {
        return Served::PostedUntraced;
    }

    // A root at count zero took no row and is not proposed; a root whose row
    // the scan raised to live is not either; the rest are the proposal
    // (`rfc/model/gc/rc-cycle.md`, "Speculative tracing and exact
    // validation").
    let proposed = batch.mark_proposed(|root| {
        let EdgeTarget::Tracked(key) = (unsafe { resolve_edge_target(root) }) else {
            return false;
        };

        match unsafe { find_initialized_row(key) } {
            Some(row) => shadow::color(unsafe { *row }) == Color::PotentiallyUnreachable,
            None => false,
        }
    });
    Served::Posted { roots, proposed }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
