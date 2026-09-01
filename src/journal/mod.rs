//! The event journal: a fixed-width record of what the runtime did,
//! written into a per-thread ring and read back afterwards. It answers one
//! question exactly — **what was recorded inside this window** — which
//! neither a counter nor a census of what exists now can answer. The
//! design is `dev/design/debug-modes.md` §9, and the decisions behind the
//! shape are in `dev/DECISIONS.md` under "the event journal is one ring
//! per thread" and the three entries beside it.
//!
//! **One ring per thread, and no global sequence number.** A window is
//! marked by reading every registered ring's cursor before and after the
//! interval, and membership follows from the two readings. Two records in
//! different rings therefore cannot be ordered against each other.
//!
//! **An overflowed window answers `unknown`, never `none`**, and so does a
//! window that lost a whole ring: a [`Mark`] carries the registry's
//! eviction count ([`Window::Evicted`]).
//!
//! Where a record site may sit is §9.7, "Rules the record path obeys";
//! where a ring lives and when it is retired, §9.4, "Where a ring lives,
//! and what happens at thread exit"; why a ring is named by identity,
//! §9.3, "The ring, and how a window is marked". [`ALLOCATING`] is raised
//! once the first record has already decided to allocate, so a site the
//! first record's own path reaches is unguarded by it and fails on that
//! first record.
//!
//! This module is compiled into every build. What §9.6 promises to compile
//! away without the `debug-journal` feature is the **record sites**, not
//! the ring, and keeping the module in the ordinary build is what keeps
//! its tests inside the gate.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::sync::{Mutex, MutexGuard};

// A model of the ring's read-back bracket, checked by `loom` rather than
// by the suite: it exists only under `--cfg loom`, where the
// dev-dependency exists too. How to run it, and what it demonstrated, are
// in the file.
#[cfg(loom)]
mod ring_model;

/// What a record's `kind` means, and the two gates a site passes: the
/// `debug-journal` feature and the enabled mask.
pub mod kinds;

/// Records per ring. A guess until a hunt runs against it
/// (`dev/design/debug-modes.md` §9.8): 1024 records is 32 KiB a thread,
/// which is the census hunt's hand-made ring rounded up to a power of
/// two. Must stay a power of two — the slot for a cursor value is
/// `cursor & (CAPACITY - 1)`.
pub const CAPACITY: usize = 1024;

/// Retired rings the registry keeps. The oldest beyond this are freed,
/// because a program that creates a thread per request would otherwise
/// accumulate rings for the life of the process. A guess, like
/// [`CAPACITY`].
pub const RETIRED_KEPT: usize = 64;

/// A ring is one pooled block, and [`CAPACITY`] is what decides that.
///
/// `ll_malloc` splits above one block payload: a request that fits takes
/// a block from the process-global pool, and a larger one is an OS-direct
/// run — a `mmap` and a `munmap` per thread that journals, on a path that
/// runs while the allocator under investigation is loaded. The split is a
/// cost rather than a correctness rule, which is exactly why raising the
/// capacity must fail the build rather than change the regime quietly.
const _: () = assert!(
    size_of::<Ring>() <= crate::memory::block_pool::BLOCK_PAYLOAD,
    "a ring larger than a block payload is an OS-direct run per thread"
);

/// What happened. `0` is unset, so a ring that was allocated and never
/// written reads as empty rather than as a run of some real kind.
pub type Kind = u32;

/// The unset kind, and the reason a fresh ring reads as empty.
pub const KIND_UNSET: Kind = 0;

/// One event, 32 bytes and fixed width — the ring is an array and the
/// cursor is an index into it, and a reader walking backwards from the
/// cursor cannot walk a variable-width record from that end.
///
/// The fields are atomics because the writer is the owning thread and the
/// reader is another one: a plain store racing a plain load is a data
/// race, which is undefined behaviour rather than the torn value a reader
/// could cope with. Both sides use relaxed ordering; the cursor's
/// release/acquire pair is what orders a record against its publication.
#[repr(C)]
pub struct Record {
    kind: AtomicU32,
    /// `LLAllocSite` id, `0` when unknown. Nothing stamps it until the
    /// debug ABI exists (`dev/design/debug-modes.md` §4.1).
    site: AtomicU32,
    /// The address the event is about.
    subject: AtomicU64,
    /// Kind-specific. An event needing more state writes two records
    /// sharing a `subject`.
    a: AtomicU64,
    b: AtomicU64,
}

/// One event as a reader sees it: plain values, copied out of a ring that
/// its owner may still be writing. `kind`, `site`, `subject`, `a` and `b`
/// carry what [`Record`]'s fields of those names carry; what each `kind`
/// puts in `subject`, `a` and `b` is [`kinds`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    pub site: u32,
    pub subject: u64,
    pub a: u64,
    pub b: u64,
    /// Which thread wrote it, from the ring header — the ordering
    /// guarantee is per thread and a reader that mixes rings needs to
    /// know which is which. A number the registry hands out, so it names
    /// one thread over the whole life of the process rather than one live
    /// thread at a time.
    pub thread: u64,
}

/// A thread's ring: the header, then [`CAPACITY`] records inline.
///
/// One allocation, so a reader holds one pointer and the records cannot
/// move. `#[repr(C)]` because the layout is read by address arithmetic
/// rather than by the type.
#[repr(C)]
pub struct Ring {
    /// Records ever written. It counts and never wraps, so the slot is
    /// `cursor & (CAPACITY - 1)` and a window's arithmetic is subtraction
    /// rather than a comparison of wrapped positions.
    cursor: AtomicU64,
    /// The writing thread's identity, stable for the ring's life —
    /// including after that thread has exited. Written once, under the
    /// registry's lock and before the ring is reachable from it, so a
    /// reader that found the ring found the identity too.
    thread: u64,
    records: [Record; CAPACITY],
}

impl Ring {
    /// Write one event and publish it. Relaxed stores fill the record,
    /// then a release store publishes `cursor + 1`, so a reader that
    /// acquires the cursor sees every record below it fully written.
    ///
    /// Only the owning thread calls this, which is what makes the plain
    /// read-modify-write of the cursor sound.
    ///
    /// [`KIND_UNSET`] is written as it is: §9.7 forbids this path to
    /// judge its caller, and a kind-0 record is still inside the window
    /// its position puts it in — the unset kind is what an *unwritten*
    /// slot reads as, and no reader walks below a cursor.
    fn write(&self, kind: Kind, site: u32, subject: u64, a: u64, b: u64) {
        let at = self.cursor.load(Ordering::Relaxed);
        let slot = &self.records[(at as usize) & (CAPACITY - 1)];
        // The other half of [`Self::read_at`]'s bracket, and the reader's
        // fence is worth nothing without it: a reader whose relaxed loads
        // picked up the stores below has to end up ordered after the
        // cursor store that closed the previous record, or its validating
        // re-read may still return a position this record has already
        // lapped. A release fence pairs with the reader's acquire fence
        // exactly there — a release *store* would not, because what the
        // reader reads from are these relaxed payload stores. Free on
        // x86-64, where it emits no instruction.
        fence(Ordering::Release);
        slot.kind.store(kind, Ordering::Relaxed);
        slot.site.store(site, Ordering::Relaxed);
        slot.subject.store(subject, Ordering::Relaxed);
        slot.a.store(a, Ordering::Relaxed);
        slot.b.store(b, Ordering::Relaxed);
        self.cursor.store(at + 1, Ordering::Release);
    }

    /// The record written at cursor position `at`, or `None` when the
    /// owner has since reused that slot.
    ///
    /// The re-read is the whole of the safety: the owner keeps writing
    /// while this copies, so a copy is trustworthy only if the cursor has
    /// not moved a full lap past the position it came from.
    fn read_at(&self, at: u64) -> Option<Event> {
        let slot = &self.records[(at as usize) & (CAPACITY - 1)];
        let event = Event {
            kind: slot.kind.load(Ordering::Relaxed),
            site: slot.site.load(Ordering::Relaxed),
            subject: slot.subject.load(Ordering::Relaxed),
            a: slot.a.load(Ordering::Relaxed),
            b: slot.b.load(Ordering::Relaxed),
            thread: self.thread,
        };

        // The fence, not the load, is what keeps the five readings above
        // from being taken after the check below: an acquire *load*
        // orders what follows it, so the record it is meant to validate
        // could be read past it and the check would validate nothing.
        // It pairs with the release fence in [`Self::write`], and neither
        // half holds alone — `ring_model.rs` exhibits an accepted torn
        // record for all three of the other combinations. This is the
        // table's version bracket again, and `ck_sequence_read_retry`
        // fences and then loads plainly for the same reason
        // (`array::head::StorageHead::coherent`, `dev/RESEARCH.md`).
        fence(Ordering::Acquire);
        let now = self.cursor.load(Ordering::Relaxed);
        if now.wrapping_sub(at) as usize >= CAPACITY {
            return None;
        }

        Some(event)
    }
}

/// Every ring in the process, live and retired alike, so one window
/// snapshot covers both without a second path.
///
/// The `Mutex` is taken at a thread's first record, at its exit and by an
/// investigator — never to write a record.
struct Registry {
    /// Rings of threads that are still running.
    live: Vec<*mut Ring>,
    /// Rings of threads that have exited, oldest first.
    retired: Vec<*mut Ring>,
    /// The identity the next registered ring is stamped with. It counts
    /// and is never reused: a thread's address is, so an identity derived
    /// from one would let a thread that started where another ended
    /// inherit its records — and a [`Mark`] names rings by identity
    /// precisely because it outlives the lock.
    next_thread: u64,
    /// Retired rings freed since the process started. A [`Mark`] records
    /// it, and the difference between two marks is how many whole thread
    /// histories the window lost.
    evicted: u64,
    /// Threads that will never have a ring — the allocator refused it, or
    /// the thread could not guarantee its retirement. They journal nothing
    /// for the rest of their lives and appear in no ring, so without a
    /// count of them a window's silence about them is indistinguishable
    /// from a process that never had them — the false *none* by the one
    /// door that opens under memory pressure, which is when the journal is
    /// switched on.
    refused: u64,
    /// Marks taken. It stamps each one, so that two marks handed to
    /// [`between`] in the wrong order are caught instead of answering a
    /// confident "nothing happened anywhere" — every ring's range comes
    /// out empty, which is the one answer this module may not invent.
    marks: u64,
    /// Rings the quota dropped, waiting to be freed by a thread that is
    /// not on its way out.
    ///
    /// The free cannot happen where the eviction does. A ring is one
    /// pooled block, and a block freed on a thread inside its own exit
    /// reaches structures that exit has already disposed. Testing for
    /// the exit at the eviction does not fix it: the two are ordered by
    /// nothing. So a retiring thread drops the rings here, and the next
    /// thread to journal or to take a mark — a live one — frees them. The
    /// vector is therefore an exit handoff, not the trace window's deferred
    /// slot reuse in `cycle::deferred_slot_reuse`.
    pending_free: Vec<*mut Ring>,
}

// The registry holds rings by raw pointer, and a ring is shared by
// construction: its owner writes it while investigators on other threads
// read it. Every field crossing that boundary is an atomic, and a ring is
// freed only after it has left both vectors, so no reader holding the
// lock can be handed a dead one.
unsafe impl Send for Registry {}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
        live: Vec::new(),
        retired: Vec::new(),
        next_thread: 0,
        evicted: 0,
        refused: 0,
        marks: 0,
        pending_free: Vec::new(),
    });

    &REGISTRY
}

/// The registry, with a poisoned lock treated as a live one.
///
/// Refusing here means panicking, and every caller is a path §9.7 forbids
/// to panic on: a record site inside the allocator, and thread exit,
/// where the panic runs in a TLS destructor and ends the process. What
/// the lock protects is two vectors and two counters, and every mutation
/// under it is one push, one retain, one drain or one addition — so a
/// panic elsewhere in the process cannot leave a half-built state behind
/// for the next caller to act on.
fn locked() -> MutexGuard<'static, Registry> {
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The ring registered under `thread`, or `None` when it has been freed.
///
/// The caller holds the registry's lock, and that is what makes the
/// returned pointer safe to read: a ring leaves the vectors before it is
/// freed, so a ring found here outlives the guard.
fn ring_of(registry: &Registry, thread: u64) -> Option<*mut Ring> {
    registry
        .live
        .iter()
        .chain(registry.retired.iter())
        .copied()
        .find(|&ring| unsafe { (*ring).thread } == thread)
}

/// Records raised on a thread whose journaling had already ended, over
/// the life of the process.
///
/// They are lost by construction and the count is the report: a thread's
/// own TLS destructors run after `heap::ll_thread_exit` on any platform
/// that destroys in reverse registration order, and what they raise has
/// no ring to go in. The runtime's own handovers are drained inside the
/// exit so that they are not among these (`heap::ll_thread_exit`); what
/// is left is a user destructor registered before the runtime's, and a
/// thread used past its exit.
///
/// A relaxed read-modify-write, on a path that writes no record — §9.1's
/// "no atomic RMW" is the write path's rule, and the threads contending
/// here have stopped journaling. It is not the *only* such path: a
/// refused thread is reported by [`Window::Refused`] instead, for its
/// whole life rather than per window, and a record raised from inside the
/// ring's own allocation is §9.7's documented first-record exception.
/// So this counts records lost to a retirement, and a reader after every
/// record the process dropped reads it beside those two.
static LOST: AtomicU64 = AtomicU64::new(0);

/// A ring this thread will never have. Neither value is an address any
/// allocation can return and neither is ever dereferenced — they exist to
/// be compared against, so that the states a thread can be in stop being
/// one.
///
/// Null means the first record site has not run yet. [`CLOSED`] means the
/// thread had a ring and has retired it; [`REFUSED`] means it never got
/// one. Both end journaling on the thread and the record path returns
/// without a lock and without an allocation — what separates them is the
/// report: a record arriving after a retirement is a **loss**, counted
/// per window, while a refused thread's silence is already reported for
/// its whole life by [`Window::Refused`] and counting its records again
/// would degrade every window it runs through.
const CLOSED: *mut Ring = std::ptr::without_provenance_mut(usize::MAX);

/// The thread never had a ring: see [`CLOSED`].
const REFUSED: *mut Ring = std::ptr::without_provenance_mut(usize::MAX - 1);

thread_local! {
    /// This thread's ring: null before the first record site runs,
    /// [`CLOSED`] once its ring is retired, [`REFUSED`] if it never got
    /// one, otherwise the ring. Which of the two ends it is decides how
    /// a later record is reported, which is why there are two of them
    /// rather than one.
    ///
    /// A `Cell<*mut _>` with no drop glue, under the rule every
    /// per-thread structure reachable from thread exit obeys
    /// (`dev/DECISIONS.md`, "thread exit owns the order its per-thread
    /// state dies in"): exit runs user code and TLS destructor order is
    /// unspecified. [`retire_thread_ring`] hands it over explicitly.
    static RING: Cell<*mut Ring> = const { Cell::new(std::ptr::null_mut()) };

    /// Set while this thread is inside [`ring_for_writing`]'s allocation,
    /// so a record site reached from inside the allocator finds no ring
    /// and returns instead of recursing (§9.7, "no re-entry").
    static ALLOCATING: Cell<bool> = const { Cell::new(false) };
}

/// Take this thread's ring now, whatever the mask says. Tests only, and
/// called from `block_pool::test_guard` so that every test holding the
/// pool's lock has its ring before its body starts. It writes no record,
/// so a window opened by the test body carries nothing from this call.
///
/// A ring is one pooled block, so the record that allocates one takes a
/// block out of the thread cache; a test that names a block, counts the
/// cache or reads a block's kind cannot have that happen in the middle of
/// it (`dev/POSTMORTEM.md`, "a ring is a block, and a thread's first
/// record decides when it is taken"). Which record is a thread's first is
/// otherwise decided by the enabled mask, which every quieting test moves
/// under every other thread in the run.
///
/// Idempotent, and silent about the outcome: a thread the allocator
/// refuses is refused here too, which is the state the journal's own
/// tests build deliberately.
#[cfg(all(test, feature = "debug-journal"))]
pub(crate) fn take_ring_for_test() {
    let _ = ring_for_writing();
}

/// This thread's ring identity, or `0` when it has none. Tests only:
/// identity is the registry's business, and no caller outside one has a
/// reason to ask for its own — what a test wants it for is to read back
/// only what **it** wrote, an address being a name shared with every
/// other thread the allocator has ever served.
#[cfg(test)]
pub(crate) fn this_thread_identity() -> u64 {
    let ring = RING.with(|cell| cell.get());
    if ring.is_null() || ring == CLOSED || ring == REFUSED {
        return 0;
    }

    unsafe { (*ring).thread }
}

/// Record one event on this thread's ring, allocating the ring on first
/// use. Silent and infallible from the caller's side: a thread whose ring
/// could not be allocated journals nothing, and the operation being
/// journaled proceeds either way.
///
/// `site` is `0` when unknown; `subject` is the address the event is
/// about; `a` and `b` are the kind's own two words. A `kind` of
/// [`KIND_UNSET`] is recorded rather than refused (§9.7).
pub fn record(kind: Kind, site: u32, subject: u64, a: u64, b: u64) {
    let ring = ring_for_writing();
    if ring.is_null() {
        return;
    }

    unsafe { (*ring).write(kind, site, subject, a, b) };
}

/// This thread's ring, allocated and registered on first use. Null when
/// this thread has none and will not get one, and null while this call is
/// itself inside the allocation.
fn ring_for_writing() -> *mut Ring {
    let existing = RING.with(|cell| cell.get());
    if existing == CLOSED {
        LOST.fetch_add(1, Ordering::Relaxed);
        return std::ptr::null_mut();
    }

    if existing == REFUSED {
        return std::ptr::null_mut();
    }

    if !existing.is_null() {
        return existing;
    }

    if ALLOCATING.with(|cell| cell.get()) {
        return std::ptr::null_mut();
    }

    // The guard covers registration and the pending frees below it, not
    // only the allocation: both can raise an event of their own once the
    // record sites exist, and a record that found this thread ringless
    // would allocate a second ring and re-enter the registry's lock,
    // which is not reentrant.
    ALLOCATING.with(|cell| cell.set(true));
    // A thread inside its own exit needs neither of the two calls below.
    // Its retirement is the step still to run, so nothing has to be armed
    // for it — and `ll_thread_init` there would rebuild the heap that
    // exit has just torn down and tell every later caller the thread may
    // free again. A record raised by a `__destruct` body in step 1 is the
    // death of a *finishing* thread, which is the hypothesis this journal
    // was built for, so it gets its ring.
    if !crate::memory::heap::thread_exit_running() {
        // A thread can reach a record site without ever having
        // initialised the runtime: the ring is larger than a heap slot,
        // so its allocation takes the large path and touches no thread
        // heap. Without this the exit guard is never registered.
        // Idempotent, and inside the guard because it allocates. Its
        // answer is not this call's business: a refused base block is a
        // thread the runtime will not run entity work on, and the abort that
        // enforces that belongs to candidate registration
        // (`cycle::queue::ensure_queue_base_or_abort`). What this call needs
        // from it is the guard, which the next line asks for directly.
        let _ = crate::memory::heap::ll_thread_init();
        // No ring is opened on a thread whose retirement is not
        // guaranteed. A ring opened where the guard cannot be armed —
        // TLS teardown has destroyed the slot — is retired by nothing and
        // stays on the live list for the life of the process, where every
        // later window reads it as a live thread doing nothing: a
        // standing false *none*, and a leak with it.
        if !crate::memory::heap::thread_exit_will_run() {
            close_this_thread();
            ALLOCATING.with(|cell| cell.set(false));
            return std::ptr::null_mut();
        }
    }

    let fresh = allocate_ring();
    if fresh.is_null() {
        // One refusal closes the thread. Retrying instead would ask the
        // OS for a block and take the registry's lock on every later
        // record, under exactly the memory pressure the journal is turned
        // on to investigate — the two things §9.7 forbids this path.
        close_this_thread();
        ALLOCATING.with(|cell| cell.set(false));
        return std::ptr::null_mut();
    }

    let pending = register_ring(fresh);
    RING.with(|cell| cell.set(fresh));
    free_rings(pending);
    ALLOCATING.with(|cell| cell.set(false));
    fresh
}

/// End this thread's journaling, and count it.
///
/// A thread with no ring is in no window, so the count is the only thing
/// standing between its silence and a reader's conclusion that it did
/// nothing ([`Window::Refused`]). The two reasons a thread ends up here
/// are the allocator refusing the ring and the thread being unable to
/// guarantee the ring's retirement; both are permanent, and neither
/// leaves the reader anything else to go on.
fn close_this_thread() {
    locked().refused += 1;
    RING.with(|cell| cell.set(REFUSED));
}

/// Stamp a fresh ring with its identity, put it on the live list, and
/// take away whatever rings a retiring thread left for a live one to
/// free.
///
/// The stamping and the push happen under one lock, so the identity is in
/// place before any reader can reach the ring through the registry, so the
/// field needs no atomic.
fn register_ring(ring: *mut Ring) -> Vec<*mut Ring> {
    let mut registry = locked();
    registry.next_thread += 1;
    unsafe {
        (&raw mut (*ring).thread).write(registry.next_thread);
    }

    registry.live.push(ring);
    take_pending(&mut registry)
}

/// The rings a retirement left behind, if this thread may free them.
///
/// A thread inside its own `ll_thread_exit` may not: the exit disposes
/// the structures a free reaches, so a ring given back inside it has
/// nowhere to land (`heap::thread_may_free` states the rule and its
/// history). The exit that runs by itself and the exit a caller invokes
/// by hand are the same sequence here, and `heap::thread_may_free`
/// answers for both — the guard's own state does not, being armed
/// throughout a hand-invoked one.
fn take_pending(registry: &mut Registry) -> Vec<*mut Ring> {
    if !crate::memory::heap::thread_may_free() {
        return Vec::new();
    }

    std::mem::take(&mut registry.pending_free)
}

/// Give the rings back to the allocator. Called on a live thread only:
/// a free reaches structures a thread inside its own exit has already
/// disposed ([`Registry::pending_free`]).
fn free_rings(rings: Vec<*mut Ring>) {
    for ring in rings {
        unsafe { crate::memory::stdapi::ll_free(ring as *mut u8) };
    }
}

/// A zeroed ring, or null if the allocator refused. Zeroed is what makes
/// an unwritten ring read as empty: `KIND_UNSET` is zero and so is a
/// fresh cursor. The identity is stamped by [`register_ring`], which is
/// where it can be drawn without a second lock.
fn allocate_ring() -> *mut Ring {
    let bytes = size_of::<Ring>();
    let memory = unsafe { crate::memory::stdapi::ll_malloc(bytes) };
    if memory.is_null() {
        return std::ptr::null_mut();
    }

    unsafe { std::ptr::write_bytes(memory, 0, bytes) };
    memory as *mut Ring
}

/// Hand this thread's ring to the retired list, and free the oldest
/// retired rings beyond [`RETIRED_KEPT`].
///
/// The thread journals nothing afterwards: the slot is closed rather than
/// emptied, or the heap teardown that follows this call in
/// `heap::ll_thread_exit` — decommissioning blocks is a default event
/// kind — would allocate a second ring under the same thread and leave it
/// on the live list for the life of the process.
///
/// Called from `heap::ll_thread_exit` and nowhere else. It goes **last**
/// in that sequence, after every structure whose teardown is worth
/// journaling. **It frees nothing**, because by the time it runs this
/// thread cannot: the structures a free reaches are disposed earlier in
/// the same sequence ([`Registry::pending_free`]).
pub fn retire_thread_ring() {
    // Only a thread that has a ring closes: a cell already holding a
    // sentinel keeps the one it has, so a refusal is not turned into a
    // retirement and the records after it are not counted twice — once as
    // a refused thread's for its whole life, once per window as a loss.
    let ring = RING.with(|cell| {
        let ring = cell.get();
        if ring.is_null() || ring == CLOSED || ring == REFUSED {
            return std::ptr::null_mut();
        }

        cell.set(CLOSED);
        ring
    });

    if ring.is_null() {
        return;
    }

    retire_ring(ring);
}

/// Move one ring from the live list to the retired one, closing it, and
/// leave whatever the quota pushes off the end for a live thread.
fn retire_ring(ring: *mut Ring) {
    let mut registry = locked();
    registry.live.retain(|&live| live != ring);
    registry.retired.push(ring);
    let over = registry.retired.len().saturating_sub(RETIRED_KEPT);
    evict_retired(&mut registry, over);
}

/// Let a thread that closed its journal at a previous exit journal again.
///
/// Called from `heap::ll_thread_init`, and only where that function
/// decides the thread has no runtime state yet — a pool thread running
/// `init`/`exit` per task is a sequence of thread lives on one OS thread,
/// and without this its second life journals nothing at all while looking
/// exactly like a thread that did nothing.
///
/// Both sentinels reopen: a refusal is final for the life it happened in,
/// and a new life on the same OS thread is a new thread by everything
/// else this module counts. A cell holding a live ring is left alone —
/// `ll_thread_init` is idempotent, and reopening a thread that already
/// has one would strand the ring on the live list and start a second
/// under a second identity.
pub fn reopen_thread() {
    RING.with(|cell| {
        let ring = cell.get();
        if ring == CLOSED || ring == REFUSED {
            cell.set(std::ptr::null_mut());
        }
    });
}

/// Take the `count` oldest retired rings out of the registry, count them
/// as evicted, and leave them for the next live thread to free.
///
/// Out of the vectors under the lock is what makes the later free safe: a
/// reader resolving an identity can no longer be handed one of these.
fn evict_retired(registry: &mut Registry, count: usize) {
    registry.evicted += count as u64;
    let evicted: Vec<*mut Ring> = registry.retired.drain(..count).collect();
    registry.pending_free.extend(evicted);
}

/// Where every ring stood at one moment — the two ends of a window are
/// two of these.
///
/// No `Default`, deliberately: a mark that names no ring and stands at no
/// moment would make `between` answer that nothing happened anywhere,
/// which is the answer this module may not invent. [`mark`] is the only
/// way to obtain one.
#[derive(Clone, Debug)]
pub struct Mark {
    /// Each registered ring's identity and cursor. Identity rather than
    /// address: a mark outlives the lock it was taken under, eviction
    /// gives a ring's block back to the allocator, and an address read
    /// afterwards would name whatever took its place.
    positions: Vec<(u64, u64)>,
    /// [`Registry::evicted`] at this moment, which is what lets a window
    /// report the rings that left the registry between two marks —
    /// neither mark can name them, since one is too early and the other
    /// too late.
    evictions: u64,
    /// [`Registry::refused`] at this moment. Cumulative rather than a
    /// difference: a thread refused before the window journals nothing
    /// inside it either.
    refusals: u64,
    /// [`LOST`] at this moment. A difference, unlike the refusals: a
    /// dropped record is a point event inside one window, and a
    /// cumulative count would mark every later window as degraded by it —
    /// "can tell" converted into "cannot tell", which is this module's
    /// own rule broken in the mirror.
    lost: u64,
    /// Which mark this is, so [`between`] can tell its two ends apart and
    /// date a ring's close against them.
    taken: u64,
}

/// A window's answer for one ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Window {
    /// Every record written inside the window, oldest first.
    Records(Vec<Event>),
    /// The ring cannot answer for the window: it was written past a full
    /// lap, or the registry freed it before the read. `written` counts the
    /// records the cursor pair spans, which is how many were lost
    /// (`dev/design/debug-modes.md` §9.3, "Eviction is reported, never
    /// hidden").
    Unknown { thread: u64, written: u64 },
    /// Rings the registry freed between the two marks, by count alone:
    /// what is gone is what would have said whose records they were.
    Evicted { rings: u64 },
    /// The two marks were handed over in the wrong order, so they bound no
    /// interval. An answer of its own, because an empty list of answers
    /// reads as "nothing happened anywhere".
    Reversed,
    /// Records raised inside the window on a thread whose journaling had
    /// already ended, and therefore dropped. A difference between the two
    /// marks rather than a running total (§9.3).
    Lost { records: u64 },
    /// Threads left without a ring in this process, refused by the
    /// allocator or unable to guarantee a ring's retirement. They are in
    /// no other answer, and the count is cumulative because a refusal
    /// lasts the life of its thread.
    Refused { threads: u64 },
}

/// Read every registered ring's cursor. One end of a window; the other is
/// another call to this after the interval.
pub fn mark() -> Mark {
    let (mark, pending) = {
        let mut registry = locked();
        registry.marks += 1;
        let taken = registry.marks;
        let positions = registry
            .live
            .iter()
            .chain(registry.retired.iter())
            .map(|&ring| unsafe { ((*ring).thread, (*ring).cursor.load(Ordering::Acquire)) })
            .collect();
        (
            Mark {
                positions,
                evictions: registry.evicted,
                refusals: registry.refused,
                lost: LOST.load(Ordering::Relaxed),
                taken,
            },
            take_pending(&mut registry),
        )
    };

    // An investigator's thread is a live one, so it is one of the two
    // that can carry out an eviction's free ([`Registry::pending_free`]).
    free_rings(pending);
    mark
}

/// What was recorded between two marks, one answer per ring.
///
/// A ring that did not exist at `start` is read from its beginning, which
/// is the correct answer rather than an approximation: a thread that
/// started inside the window has no earlier records. A ring whose owner
/// exited inside the window keeps its final cursor and is read like any
/// other. A ring the registry has freed answers [`Window::Unknown`] if a
/// mark named it and is counted by [`Window::Evicted`] if none could.
///
/// The registry's lock is held for the whole read, which is what makes
/// the copies safe: a ring is freed only after it leaves the registry, so
/// holding the lock is holding every ring this call resolves. The cost is
/// that a thread taking its first record waits, and an investigator's
/// read is where that cost belongs.
///
/// The caller owns the ordering caveat: records from one ring are in that
/// thread's program order, and records from different rings have no order
/// between them.
pub fn between(start: &Mark, end: &Mark) -> Vec<Window> {
    // A runtime test rather than an assertion, and an answer of its own
    // rather than a panic: the crate's release profile compiles
    // assertions out, and this is the one mistake whose uncaught answer
    // is a confident "nothing happened anywhere". It is answered before
    // the rings are walked, because a reversed pair taken when nothing
    // had journaled yet names no ring at all, and a per-ring answer over
    // no rings is an empty list — the very answer being refused.
    if start.taken > end.taken {
        return vec![Window::Reversed];
    }

    let registry = locked();
    let mut windows: Vec<Window> = end
        .positions
        .iter()
        .map(|&(thread, end_at)| {
            let start_at = start
                .positions
                .iter()
                .find(|&&(earlier, _)| earlier == thread)
                .map_or(0, |&(_, at)| at);
            match ring_of(&registry, thread) {
                Some(ring) => window_of(ring, thread, start_at, end_at),
                None => Window::Unknown {
                    thread,
                    written: end_at.saturating_sub(start_at),
                },
            }
        })
        .collect();
    let vanished = end.evictions.saturating_sub(start.evictions);
    if vanished > 0 {
        windows.push(Window::Evicted { rings: vanished });
    }

    let dropped = end.lost.saturating_sub(start.lost);
    if dropped > 0 {
        windows.push(Window::Lost { records: dropped });
    }

    if end.refusals > 0 {
        windows.push(Window::Refused {
            threads: end.refusals,
        });
    }

    windows
}

/// One rule decides an overflow, and it is the read-back:
/// [`Ring::read_at`] refuses a position the owner has lapped. Counting
/// `end - start` against [`CAPACITY`] first would be a second rule saying
/// the same thing — an early-out worth at most `CAPACITY` copies to an
/// investigator, at the price of two rules that can disagree.
///
/// `ring` is resolved from the registry under its lock, and the caller
/// still holds it.
fn window_of(ring: *mut Ring, thread: u64, start_at: u64, end_at: u64) -> Window {
    let mut events = Vec::new();
    for at in start_at..end_at {
        match unsafe { (*ring).read_at(at) } {
            Some(event) => events.push(event),
            None => {
                return Window::Unknown {
                    thread,
                    written: end_at.saturating_sub(start_at),
                };
            }
        }
    }

    Window::Records(events)
}

#[cfg(test)]
mod tests;
