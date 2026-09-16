//! The collector thread, and what it does for one owner: claim the owner's
//! token by compare-and-swap, take a batch of the owner's candidates from
//! behind its writer, trace them on a copy through `cells::AtomicCells`
//! under a block budget, post one verdict per root into the owner's verdict
//! ring P in R's order, advance R past them, and release
//! (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff";
//! `rfc/dev/DECISIONS.md`, "the candidate queue is read behind its writer,
//! and the collector's verdicts come back by a second ring", "The
//! collector's batch"). What the owner does with the verdicts is
//! `crate::cycle::queue::verdicts`.
//!
//! # The batch
//!
//! Before any claim the collector reads whether the owner has work — an
//! entry standing in R, off the front block's words alone, and P's room —
//! and opens its
//! own workspace: an owner with nothing to take pays no foreign-holder
//! window, under which every one of its deaths is withheld. Then it claims
//! the token and reads the owner's collecting word with acquire: set, the
//! owner is collecting in line and the collector releases and skips — the
//! two orders both resolve, a claim made first being waited out by the
//! owner's take, one made second seeing the word. It peeks up to K entries
//! from R's front through the reader's pair without consuming them, K
//! clamped to P's room and to what R holds, and copies them into its
//! workspace. It marks and scans each root through `cells::AtomicCells` on
//! an arena bounded to [`TRACE_BLOCK_BUDGET`] blocks; a root at count zero
//! is marked by nothing. Then it posts one verdict per entry, in R's order:
//! *proposed* for a row read potentially unreachable, *read live* for one
//! read live and for a live root the trace could not place — an external
//! live reference, by the trace's own rule for an edge it cannot place —
//! *zero-count* for a count read zero, and *unwalked* for every root of a
//! batch whose trace met the budget or a refused allocation: no color of
//! such a trace is a verdict, so the whole batch is handed to the owner's
//! exact trace rather than a prefix of it (`rfc/model/gc/rc-cycle.md`,
//! "Worker-to-owner handoff", amended 2026-09-16 to the whole batch). R's
//! front advances past the batch only after every verdict is posted, by
//! one guard that runs from the unwind as well, so that no entry is
//! consumed without a verdict and none twice. The arena is reset before the
//! token goes, since its rows stand over the owner's blocks.
//!
//! K starts at [`INITIAL_BATCH`], halves after a batch that met the budget
//! — the budget alone, a pool refusal saying nothing about the batch's size
//! — and doubles back after a completed one, up to [`BATCH_BOUND`]: under a
//! block's capacity, so a batch spans at most two blocks of R, and small
//! enough that the copy leaves the workspace to the rows. What an owner
//! waits for when it needs its token is one batch's trace, bounded by the
//! blocks rather than by the roots.
//!
//! # The thread, and the round over the records
//!
//! One collector thread per process, born by [`ensure_thread`] and never at
//! startup (`dev/DECISIONS.md`, "the collector thread is born at the first
//! pressure collection"): the pressure path is the one fire point the
//! runtime owns, and a thread born there costs nothing to a process that
//! never runs short. No production path births it until S49.7 wires the
//! pressure path back to [`ensure_thread`] with the wake channel. It starts
//! as any registered thread does, through `ll_thread_init`, whose base block
//! draw can be refused; a refused base block is a thread that never started,
//! and a call [`BIRTH_RETRY_INTERVAL`] or more after the refusal births
//! again. A round walks every record the registry has carved ([`round`]).
//! Between two rounds the thread sleeps for [`ROUND_INTERVAL`]. Nothing
//! wakes the thread early, and nothing but a test ends it.

// Dead in a build without tests until S49.7 gives the birth its caller; the
// tests are the module's only driver until then.
#![cfg_attr(not(test), allow(dead_code))]

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::cells::AtomicCells;
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::owner_record::{self, OwnerRecord};
use crate::cycle::queue::verdicts::{Verdict, VerdictWriter};
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::scan::{ScanResult, scan};
use crate::cycle::shadow::{self, Color};
use crate::refcount::RcHeader;
use crate::ring::{BLOCK_ENTRIES, Reader};

/// What one round over one owner did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Served {
    /// The owner, or another collector, holds the token.
    TokenHeld,
    /// The token was claimed and released at once: the owner is collecting
    /// in line.
    OwnerCollecting,
    /// Nothing to take, read before any claim: R read empty, P had no room,
    /// or the collector's workspace was refused. No claim was made.
    Idle,
    /// A batch was made: this many roots taken from R, each with a verdict
    /// posted into P, and whether their trace completed.
    Batch { roots: usize, complete: bool },
}

/// Roots a batch takes from an owner the collector has not served before.
/// Not a measured figure: the rfc names no start, and the size adapts from
/// here by the batch's outcome.
const INITIAL_BATCH: usize = 64;

/// The most roots a batch takes: under a block's capacity, so that the peek
/// spans at most two blocks of R, and a copy of at most a quarter of the
/// workspace's bump, so that the rows of the trace do not start by growing.
const BATCH_BOUND: usize = 1024;

const _: () = assert!(BATCH_BOUND < BLOCK_ENTRIES);
const _: () =
    assert!(BATCH_BOUND * size_of::<usize>() * 4 <= crate::cycle::arena::WORKSPACE_BUMP_BYTES);

/// Blocks a batch's trace may draw above the collector's workspace before it
/// ends with its roots unwalked. Not a measured figure: what it bounds is
/// the owner's wait for its token, and the rfc names the bound and not its
/// size.
const TRACE_BLOCK_BUDGET: usize = 8;

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
/// A spawn the operating system refuses, and a base block the pool refuses,
/// each leave the process without a thread until a call after the interval.
/// No production path calls it yet (S49.7 makes the pressure path its
/// caller, after the collection so that the thread's draws compete with no
/// rows of the caller's own).
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
    // here — so that a later birth can happen rather than read a thread
    // that no longer exists.
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
        // allocator-issued"); a call after the interval births again.
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

/// One round over the records: visit every record but the thread's own,
/// which polls nothing, and serve each. What a serve does is [`serve`]'s.
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
        #[cfg(not(test))]
        let _ = served;
    });
}

/// Serve `record`'s owner once: claim its token by compare-and-swap, held
/// being a skip; skip an owner collecting in line; otherwise make one batch
/// (module doc) and release.
///
/// Runs on a collector thread, which holds a base block of its own for the
/// workspace the batch's trace opens (`crate::memory::heap::ll_thread_init`).
///
/// # Safety
/// `record` is a record of the registry's, and the calling thread is not its
/// owner.
pub(crate) unsafe fn serve(record: *mut OwnerRecord) -> Served {
    let owner = unsafe { &*record };
    // Work first, and the collector's own memory, before any claim — by
    // loads alone, since nothing of the owner's may be written under no
    // claim, and off the front block alone, since the owner's pack and its
    // poll's unlink move blocks past the tail block out of the circle under
    // no claim either: whether R has an entry, P's room off its index words,
    // and the workspace this thread's. The figures are an idle test and not
    // the clamp: the clamp is re-read under the token. The blocks read are
    // held for the reading, since an owner exiting meanwhile returns them
    // (`crate::cycle::owner_record`, "The blocks a collector reads before
    // its claim are held"); a record another reading holds is idle to this
    // round.
    if !unsafe { owner_record::take_for_reading(record) } {
        return Served::Idle;
    }

    // Handed back on the unwind too: a hold left standing keeps the record
    // off the registry's free list and its blocks out of the pool for good.
    struct HandBackOnDrop(*mut OwnerRecord);
    impl Drop for HandBackOnDrop {
        fn drop(&mut self) {
            unsafe { owner_record::hand_back_reading(self.0) };
        }
    }
    let (has_work, room) = {
        let _reading = HandBackOnDrop(record);
        #[cfg(test)]
        testing::between_the_take_and_the_reading();
        let has_work = unsafe { Reader::new(owner.candidate_ring()) }.has_unread();
        let room = unsafe { VerdictWriter::open(owner) }.room_by_loads();
        (has_work, room)
    };
    if !has_work || room == 0 {
        return Served::Idle;
    }

    let Some(mut arena) = TraceScratchArena::open() else {
        return Served::Idle;
    };

    if !owner.token.try_take() {
        return Served::TokenHeld;
    }

    // Released on the unwind too: a collector that panicked under the claim
    // would otherwise leave the owner's exit waiting forever.
    struct ReleaseOnDrop<'a>(&'a crate::cycle::token::TraceToken);
    impl Drop for ReleaseOnDrop<'_> {
        fn drop(&mut self) {
            crate::cycle::token::note_traced_owner(std::ptr::null_mut());
            self.0.release();
        }
    }
    let _held = ReleaseOnDrop(&owner.token);
    crate::cycle::token::note_traced_owner(record);
    if owner.is_collecting_as_collector() {
        return Served::OwnerCollecting;
    }

    unsafe { batch(owner, &mut arena) }
}

/// One batch over `owner`, under its token, on `arena` — the collector's own
/// memory, reset before the token goes (module doc).
///
/// # Safety
/// The calling thread holds `owner`'s token and `owner` is not collecting
/// in line.
unsafe fn batch(owner: &OwnerRecord, arena: &mut TraceScratchArena) -> Served {
    let verdicts = unsafe { VerdictWriter::open(owner) };
    // The clamp, under the token: P's room cannot move under it, R's count
    // can only grow.
    let take = verdicts.room().min(match owner.batch_size() {
        0 => INITIAL_BATCH,
        size => size,
    });
    if take == 0 {
        return Served::Idle;
    }
    #[cfg(not(test))]
    let budget = TRACE_BLOCK_BUDGET;
    #[cfg(test)]
    let budget = testing::budget_for_this_batch().unwrap_or(TRACE_BLOCK_BUDGET);
    arena.budget_blocks(budget);
    // Within the workspace by the bound on K, so this draws nothing.
    let copy = arena.alloc(take * size_of::<usize>()) as *mut usize;
    debug_assert!(!copy.is_null(), "the copy fits the workspace");
    if copy.is_null() {
        return Served::Idle;
    }

    // The entries copied out of R, which stay in R until the advance.
    let out = unsafe { std::slice::from_raw_parts_mut(copy, take) };
    let reader = unsafe { Reader::new(owner.candidate_ring()) };
    let peeked = reader.peek(out);
    let roots = &out[..peeked.len()];
    if roots.is_empty() {
        return Served::Idle;
    }

    let complete = unsafe { trace(arena, roots) };
    for &entry in roots {
        let root = crate::cycle::queue::entry_root(entry);
        let verdict = if complete {
            unsafe { verdict_for(root) }
        } else {
            Verdict::Unwalked
        };
        verdicts
            .post(root, verdict)
            .expect("the batch was clamped to P's room");
    }

    // Every verdict is posted: from here the advance is owed, and the guard
    // makes it from the unwind as well.
    struct AdvanceOnDrop<'a>(&'a Reader<'a>, crate::ring::Peeked);
    impl Drop for AdvanceOnDrop<'_> {
        fn drop(&mut self) {
            self.0.commit(self.1);
        }
    }
    let advance = AdvanceOnDrop(&reader, peeked);
    #[cfg(test)]
    testing::between_the_post_and_the_advance();
    drop(advance);

    let met_budget = arena.met_its_budget();
    arena.reset();
    let size = match owner.batch_size() {
        0 => INITIAL_BATCH,
        size => size,
    };
    if complete {
        owner.set_batch_size((size * 2).min(BATCH_BOUND));
    } else if met_budget {
        owner.set_batch_size((size / 2).max(1));
    }

    Served::Batch {
        roots: roots.len(),
        complete,
    }
}

/// Mark every root of `roots`, then scan every one: true when both phases
/// completed, false when either met the budget or a refused allocation — at
/// which point no color is a verdict.
///
/// # Safety
/// As [`mark`] through `AtomicCells`: the calling thread holds the owner's
/// token, and every root is an entry of the owner's R.
unsafe fn trace(arena: &mut TraceScratchArena, roots: &[usize]) -> bool {
    for &entry in roots {
        let root = crate::cycle::queue::entry_root(entry);
        if unsafe { mark::<AtomicCells>(arena, root) } != MarkResult::Complete {
            return false;
        }
    }

    for &entry in roots {
        let root = crate::cycle::queue::entry_root(entry);
        if unsafe { scan::<AtomicCells>(arena, root) } != ScanResult::Complete {
            return false;
        }
    }

    true
}

/// The verdict a completed trace supports for `root`: a count read zero is
/// [`Verdict::ZeroCount`] before any row is read, a row read potentially
/// unreachable is [`Verdict::Proposed`], and a row read live is
/// [`Verdict::ReadLive`] — as is a live root with no met row, one the trace
/// could not place, which the trace's own rule reads as an external live
/// reference and which the owner's trace would place no better; an
/// *unwalked* verdict would send it round P and R at every poll.
///
/// # Safety
/// The trace over `root` completed on this thread and its rows still stand.
unsafe fn verdict_for(root: *mut RcHeader) -> Verdict {
    if unsafe { crate::refcount::slot_state(root) } != crate::refcount::SlotState::Live {
        return Verdict::ZeroCount;
    }

    let EdgeTarget::Tracked(key) = (unsafe { resolve_edge_target(root) }) else {
        return Verdict::ReadLive;
    };

    match unsafe { crate::cycle::arena::find_initialized_row(key) } {
        Some(row) => match shadow::color(unsafe { *row }) {
            Color::PotentiallyUnreachable => Verdict::Proposed,
            _ => Verdict::ReadLive,
        },
        None => Verdict::ReadLive,
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
