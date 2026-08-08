//! The event journal: a fixed-width record of what the runtime did,
//! written into a per-thread ring and read back afterwards. It answers
//! one question exactly — **what was recorded inside this window** —
//! which neither a counter nor a census of what exists now can answer
//! (`dev/design/debug-modes.md` §9).
//!
//! **One ring per thread, and no global sequence number.** A window is
//! marked by reading every registered ring's cursor before and after the
//! interval; membership follows from the two readings. The price is that
//! two records in different rings cannot be ordered against each other,
//! and it is affordable because the investigation this exists for asks
//! about membership rather than order. What it buys: the write path
//! takes no atomic read-modify-write, a thread that allocates hard cannot
//! evict the records of the thread under investigation, and thread
//! identity lives in the ring header instead of in every record.
//!
//! **An overflowed window answers `unknown`, never `none`.** A hunt that
//! turns on "nothing of this kind happened inside the window" is worth
//! nothing if a silent eviction can produce the same answer. The rule
//! covers the ring that is gone as well as the ring that lapped: a
//! [`Mark`] carries the registry's eviction count, so a window that lost
//! a whole thread's history says so ([`Window::Evicted`]).
//!
//! The ring is allocated through [`crate::memory::stdapi::ll_malloc`] on
//! the thread's first record, never from an arena and never through
//! `entity_alloc`: the journal has to be readable while the collector
//! holds an epoch and while an arena resets, and a site reached from
//! inside that allocation records nothing rather than recursing.
//!
//! **The first record on a thread is the exception to §9.7, and it is one
//! by design**: it initialises the thread, allocates the ring and takes
//! the registry's lock, and only the records after it are free of all
//! three. What follows for a record site (§9.5, unbuilt) is a rule rather
//! than a caveat — **a site must not sit anywhere that path can reach**:
//! `ll_thread_init` and everything under it, the block pool's free list,
//! and the pool's thread cache, whose `RefCell` is held across a push to
//! the global list. The failure is not always the deadlock the phrase
//! suggests; against the `RefCell` it is a borrow panic, and this crate
//! aborts on a panic. [`ALLOCATING`] cannot save that case: it is raised
//! once the first record has already decided to allocate. A refused
//! allocation turns journaling off for that thread **for good** — the
//! thread's slot holds [`CLOSED`] from then on, so no later record asks
//! the allocator again — and the journaled operation proceeds. Refusals
//! are counted, because a thread with no ring is in no window and the
//! count is all that keeps its silence from reading as inactivity
//! ([`Window::Refused`]).
//!
//! Rings outlive their threads, because a thread's records matter most
//! once it is gone. **The ring is retired by the last act of
//! `heap::ll_thread_exit`**, after every step of the teardown — the
//! `__destruct` bodies of step 1 and the block frees of the heap drop
//! alike — so nothing a dying thread does inside the contract goes
//! unrecorded, and a window over its death is complete rather than
//! confessed. The ring is handed to the registry's retired list, which
//! keeps the most recent [`RETIRED_KEPT`], and the thread's slot is
//! closed the same way a refusal closes it. Nothing this thread still
//! owes the allocator is freed there: its deferral backlog is gone by
//! then, so an evicted ring waits for a live thread.
//! `ll_thread_init` reopens a closed slot, so one OS thread running a
//! pool's tasks journals each life into a ring of its own — and every
//! self-initialising way back into the runtime runs that function, so a
//! thread still raising events with its slot closed is one being used
//! after `ll_thread_exit` outside the runtime's contract. The silence
//! there is deliberate and goes unreported.
//!
//! **A ring is named by identity, never by address, everywhere a name
//! outlives the registry's lock.** The identity is a counter's next value
//! handed out at registration; the address is a `ll_malloc` block that
//! eviction gives back and the allocator hands to somebody else.
//!
//! This module is compiled into every build. What §9.6 promises to
//! compile away without the `debug-journal` feature is the **record
//! sites** on the allocation and death paths, not the ring: a ring nobody
//! writes costs one thread-local pointer that stays null, and keeping the
//! module in the ordinary build is what keeps its tests inside the gate.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};
use std::sync::{Mutex, MutexGuard};

// A model of the ring's read-back bracket, checked by `loom` rather than
// by the suite: it exists only under `--cfg loom`, where the
// dev-dependency exists too. How to run it, and what it demonstrated, are
// in the file.
#[cfg(loom)]
mod ring_model;

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
/// its owner may still be writing.
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
        // (`array::table::coherent_entries`, `dev/RESEARCH.md`).
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
    /// the thread could not guarantee its retirement. They journal nothing for
    /// the rest of their lives and appear in no ring, so without a count
    /// of them a window's silence about them is indistinguishable from a
    /// process that never had them — the false *none* by the one door
    /// that opens under memory pressure, which is when the journal is
    /// switched on.
    refused: u64,
    /// Marks taken. It stamps each one, so that two marks handed to
    /// [`between`] in the wrong order are caught instead of answering a
    /// confident "nothing happened anywhere" — every ring's range comes
    /// out empty, which is the one answer this module may not invent.
    /// It also dates a ring's close ([`Ring::closed_after`]): marks are
    /// the only clock a window has.
    marks: u64,
    /// Rings the quota dropped, waiting to be freed by a thread that is
    /// not on its way out.
    ///
    /// The free cannot happen where the eviction does. A ring is one
    /// pooled block, so `ll_free` parks it while a collector epoch is in
    /// flight — onto a backlog `heap::ll_thread_exit` disposed three
    /// steps before it reaches retirement, which would rebuild the list
    /// on a dying thread and leak it with the ring inside. Checking for
    /// an epoch does not fix it either: one can open between the check
    /// and the free. So a retiring thread drops the rings here, and the
    /// next thread to journal or to take a mark — a live thread, whose
    /// parked backlog is still its own — frees them.
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

/// A ring this thread will never have. It is an address no allocation can
/// return and is never dereferenced — the value exists only to be
/// compared against, so that "not asked yet" and "asked and answered no"
/// stop being the same state.
///
/// Null means the first record site has not run yet. `CLOSED` means the
/// allocator refused, or the thread has retired its ring, and in both
/// cases journaling on this thread is over: the record path returns
/// without a lock and without an allocation.
const CLOSED: *mut Ring = std::ptr::without_provenance_mut(usize::MAX);

thread_local! {
    /// This thread's ring: null before the first record site runs,
    /// [`CLOSED`] once it will never have one, otherwise the ring.
    ///
    /// A `Cell<*mut _>` with no drop glue, under the rule every
    /// per-thread structure reachable from thread exit obeys
    /// (`dev/DECISIONS.md`, 2026-08-03): exit runs user code and TLS
    /// destructor order is unspecified. [`retire_thread_ring`] hands it
    /// over explicitly.
    static RING: Cell<*mut Ring> = const { Cell::new(std::ptr::null_mut()) };

    /// Set while this thread is inside [`ring_for_writing`]'s allocation,
    /// so a record site reached from inside the allocator finds no ring
    /// and returns instead of recursing (§9.7, "no re-entry").
    static ALLOCATING: Cell<bool> = const { Cell::new(false) };
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
        // Idempotent, and inside the guard because it allocates.
        crate::memory::heap::ll_thread_init();
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
    RING.with(|cell| cell.set(CLOSED));
}

/// Stamp a fresh ring with its identity, put it on the live list, and
/// take away whatever rings a retiring thread left for a live one to
/// free.
///
/// The stamping and the push happen under one lock, so the identity is in
/// place before any reader can reach the ring through the registry —
/// which is the whole of why the field needs no atomic.
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
/// A thread inside its own `ll_thread_exit` may not: the deferral backlog
/// a parked free needs is disposed within that sequence and nothing
/// rebuilds it, so the ring would be parked onto a list dropped
/// unreleased. The exit that runs by itself and the exit a caller invokes
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
/// the free can park for a collector epoch, and a parked free needs the
/// backlog its own thread still owns ([`Registry::pending_free`]).
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
/// thread cannot: a ring's free can park for a collector epoch, and the
/// backlog a park needs was disposed three steps earlier
/// ([`Registry::pending_free`]).
pub fn retire_thread_ring() {
    let ring = RING.with(|cell| cell.replace(CLOSED));
    if ring.is_null() || ring == CLOSED {
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
/// A cell holding a live ring is left alone: `ll_thread_init` is
/// idempotent, and reopening a thread that already has one would strand
/// the ring on the live list and start a second under a second identity.
pub fn reopen_thread() {
    RING.with(|cell| {
        if cell.get() == CLOSED {
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
    /// Which mark this is, so [`between`] can tell its two ends apart and
    /// date a ring's close against them.
    taken: u64,
}

/// A window's answer for one ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Window {
    /// Every record written inside the window, oldest first.
    Records(Vec<Event>),
    /// This ring cannot answer for the window: it was written past a full
    /// lap, or the registry freed it before the read, or the two marks
    /// bound no window at all. `written` counts the records the cursor
    /// pair spans, and is `None` where there is no pair to count. **Not
    /// the same as an empty answer**, and the distinction is the reason a
    /// hunt can trust "nothing happened".
    Unknown { thread: u64, written: Option<u64> },
    /// Rings the registry freed inside the window: whole thread histories
    /// no answer above can carry, the ring having left the registry
    /// between the two marks. Named by count alone, because what is gone
    /// is what would have told a reader whose records they were.
    Evicted { rings: u64 },
    /// The two marks were handed over in the wrong order, so they bound
    /// no window: there is no interval to answer for, and no ring's
    /// silence to report. An answer of its own rather than an empty list
    /// of answers, which reads as "nothing happened anywhere" — and reads
    /// that way even when no ring is named, which is where an answer per
    /// ring cannot reach.
    Reversed,
    /// Threads left without a ring, in this process, ever — refused by the
    /// allocator, or unable to guarantee a ring's retirement. They
    /// have written nothing and are in no other answer, so a window that
    /// did not carry this number would spell "these threads did nothing"
    /// exactly as it spells "these threads do not exist". Cumulative, and
    /// deliberately: a refusal is for the life of its thread.
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
                    written: Some(end_at.saturating_sub(start_at)),
                },
            }
        })
        .collect();
    let vanished = end.evictions.saturating_sub(start.evictions);
    if vanished > 0 {
        windows.push(Window::Evicted { rings: vanished });
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
                    written: Some(end_at.saturating_sub(start_at)),
                };
            }
        }
    }
    Window::Records(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This thread's ring identity, or `0` when it has none. Tests only:
    /// identity is the registry's business, and no caller outside one has
    /// a reason to ask for its own.
    fn this_thread_identity() -> u64 {
        let ring = RING.with(|cell| cell.get());
        if ring.is_null() || ring == CLOSED {
            return 0;
        }
        unsafe { (*ring).thread }
    }

    /// How many rings are live and how many retired. Tests only, and the
    /// live count is what a resurrected ring shows up in.
    fn registry_counts() -> (usize, usize) {
        let registry = locked();
        (registry.live.len(), registry.retired.len())
    }

    /// Rings evicted and waiting for a live thread to free them.
    fn pending_count() -> usize {
        locked().pending_free.len()
    }

    /// Free one retired ring by identity, the way the quota's eviction
    /// frees the oldest. Tests only: firing the quota takes
    /// `RETIRED_KEPT + 1` threads and 2 MiB of rings to observe one line
    /// of arithmetic, while what the tests below are about is what a
    /// window says once a ring is gone.
    fn evict_retired_ring(thread: u64) -> bool {
        let ring = {
            let mut registry = locked();
            match registry
                .retired
                .iter()
                .position(|&ring| unsafe { (*ring).thread } == thread)
            {
                Some(at) => {
                    registry.evicted += 1;
                    registry.retired.remove(at)
                }
                None => return false,
            }
        };
        unsafe { crate::memory::stdapi::ll_free(ring as *mut u8) };
        true
    }

    /// Every event the answers carry, in the order the windows came in.
    fn events(windows: Vec<Window>) -> Vec<Event> {
        windows
            .into_iter()
            .flat_map(|window| match window {
                Window::Records(events) => events,
                _ => Vec::new(),
            })
            .collect()
    }

    /// A thread that journals one record and then exits through the whole
    /// exit sequence, as a dying thread does. Returns its ring identity.
    fn a_journaling_thread(subject: u64) -> u64 {
        std::thread::spawn(move || {
            crate::memory::heap::ll_thread_init();
            record(5, 0, subject, 0, 0);
            let identity = this_thread_identity();
            crate::memory::heap::ll_thread_exit();
            identity
        })
        .join()
        .expect("the journaling thread panicked")
    }

    /// A ring wraps and keeps the last `CAPACITY` records, and the cursor
    /// keeps counting past the wrap — which is what makes a window's
    /// arithmetic subtraction rather than a comparison of wrapped
    /// positions.
    #[test]
    fn a_ring_wraps_and_its_cursor_does_not() {
        let _g = crate::memory::block_pool::test_guard();
        let ring = allocate_ring();
        assert!(!ring.is_null());
        let ring = unsafe { &*ring };

        for i in 0..(CAPACITY as u64 + 3) {
            ring.write(1, 0, i, 0, 0);
        }
        assert_eq!(ring.cursor.load(Ordering::Relaxed), CAPACITY as u64 + 3);
        // The three newest are readable and name the last three subjects.
        for (offset, subject) in [
            (3, CAPACITY as u64),
            (2, CAPACITY as u64 + 1),
            (1, CAPACITY as u64 + 2),
        ] {
            let at = ring.cursor.load(Ordering::Relaxed) - offset;
            assert_eq!(
                ring.read_at(at).expect("still inside the ring").subject,
                subject
            );
        }
        // The record that was lapped is gone rather than stale.
        assert!(ring.read_at(0).is_none(), "a lapped position read as live");

        unsafe { crate::memory::stdapi::ll_free(ring as *const Ring as *mut u8) };
    }

    /// The window is the cursor pair, so a record written before the
    /// first mark is outside it and one written after is inside — which
    /// is the whole of "what happened between these two moments".
    #[test]
    fn a_cursor_pair_names_exactly_what_was_written_inside_it() {
        let _g = crate::memory::block_pool::test_guard();
        const BEFORE: u64 = 0x0B4;
        const FIRST_INSIDE: u64 = 0x1_11;
        const SECOND_INSIDE: u64 = 0x2_22;
        const AFTER: u64 = 0x0AF;

        record(7, 0, BEFORE, 0, 0);
        let start = mark();
        record(7, 0, FIRST_INSIDE, 1, 0);
        record(7, 0, SECOND_INSIDE, 2, 0);
        let end = mark();
        record(7, 0, AFTER, 0, 0);

        let mine = this_thread_identity();
        let inside: Vec<u64> = events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == mine)
            .map(|event| event.subject)
            .collect();
        assert_eq!(inside, vec![FIRST_INSIDE, SECOND_INSIDE]);
        retire_thread_ring();
    }

    /// An overflowed window answers `unknown`, and that is the point of
    /// the whole mechanism: the hunt it exists for turned on "no string
    /// died inside the window", and a silent eviction would have made
    /// that finding false.
    #[test]
    fn an_overflowed_window_answers_unknown_rather_than_none() {
        let _g = crate::memory::block_pool::test_guard();
        let start = mark();
        for i in 0..(CAPACITY as u64 * 2) {
            record(9, 0, i, 0, 0);
        }
        let end = mark();

        let mine = this_thread_identity();
        let mut seen = false;
        for window in between(&start, &end) {
            if let Window::Unknown { thread, written } = window
                && thread == mine
            {
                assert_eq!(written, Some(CAPACITY as u64 * 2));
                seen = true;
            }
        }
        assert!(
            seen,
            "an overflowed ring reported records instead of unknown"
        );
        retire_thread_ring();
    }

    /// A thread's records matter most once it is gone — the census flake
    /// this journal was designed for is a hypothesis about a *finishing*
    /// thread — so a retired ring stays readable and stays in the window.
    #[test]
    fn a_retired_threads_ring_is_still_read_by_a_window() {
        const SUBJECT: u64 = 0xDEAD;
        let _g = crate::memory::block_pool::test_guard();
        let start = mark();

        let joined = a_journaling_thread(SUBJECT);

        let end = mark();
        let found = events(between(&start, &end))
            .into_iter()
            .any(|event| event.thread == joined && event.subject == SUBJECT);
        assert!(found, "the exited thread's ring left the window with it");
    }

    /// Thread exit is not the last thing a dying thread does: the heap
    /// teardown that follows the journal's own step decommissions blocks,
    /// which is a default event kind. A record from there must find the
    /// thread closed rather than open a second ring under the same
    /// identity — one nothing would ever retire, and one that would make
    /// `RETIRED_KEPT` bound a list the leak is not on.
    #[test]
    fn a_thread_that_journals_after_its_exit_starts_no_second_ring() {
        const BEFORE_EXIT: u64 = 0xE1;
        const AFTER_EXIT: u64 = 0xE2;
        let _g = crate::memory::block_pool::test_guard();
        let (live_before, retired_before) = registry_counts();
        let start = mark();

        let identity = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            record(3, 0, BEFORE_EXIT, 0, 0);
            let identity = this_thread_identity();
            crate::memory::heap::ll_thread_exit();
            record(3, 0, AFTER_EXIT, 0, 0);
            identity
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        let (live_after, retired_after) = registry_counts();
        assert_eq!(
            live_after, live_before,
            "the exited thread left a live ring behind"
        );
        assert_eq!(
            retired_after,
            retired_before + 1,
            "the thread retired a number of rings other than its one"
        );

        // The post-exit record is not in it, and its silence is not
        // reported: a thread raising events after `ll_thread_exit` is
        // being used outside the runtime's contract, and that is the one
        // silence at exit this module leaves unreported.
        let subjects: Vec<u64> = events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == identity)
            .map(|event| event.subject)
            .collect();
        assert_eq!(subjects, vec![BEFORE_EXIT]);
    }

    /// A refused allocation closes the thread instead of queueing a
    /// retry. Retrying would take two process-global mutexes and ask the
    /// OS for a block on every later record — under the memory pressure
    /// the journal was turned on to investigate, which is where §9.7's
    /// "no allocation, no lock" is worth the most.
    #[test]
    fn a_refused_ring_is_not_asked_for_a_second_time() {
        use crate::memory::block_pool::FORCE_OOM;
        let _g = crate::memory::block_pool::test_guard();
        let counts_before = registry_counts();

        let identity = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            FORCE_OOM.store(true, Ordering::Relaxed);
            record(4, 0, 1, 0, 0);
            FORCE_OOM.store(false, Ordering::Relaxed);
            // The pressure is gone and this thread still journals
            // nothing: the refusal was final, not a bad moment.
            record(4, 0, 2, 0, 0);
            let identity = this_thread_identity();
            crate::memory::heap::ll_thread_exit();
            identity
        })
        .join()
        .expect("the journaling thread panicked");

        assert_eq!(identity, 0, "a refused thread ended up with a ring");
        // The retired count, not the live one: the thread ran its exit,
        // so a ring it did get would have moved off the live list before
        // this line and left that count telling nothing.
        assert_eq!(
            registry_counts(),
            counts_before,
            "a refused ring was asked for again, and granted"
        );
    }

    /// A ring the registry frees inside a window takes a whole thread's
    /// history with it, and the window has to say so: reporting the rings
    /// that are left is the conversion of *unknown* into *none* this
    /// module exists to prevent.
    #[test]
    fn a_ring_freed_inside_the_window_is_reported_rather_than_missing() {
        const SUBJECT: u64 = 0x105E;
        let _g = crate::memory::block_pool::test_guard();

        let identity = a_journaling_thread(SUBJECT);
        let start = mark();
        assert!(
            evict_retired_ring(identity),
            "the exited thread's ring was not on the retired list"
        );
        let end = mark();

        assert!(
            between(&start, &end)
                .into_iter()
                .any(|window| window == Window::Evicted { rings: 1 }),
            "a ring freed inside the window left no trace in it"
        );
    }

    /// A mark names rings by identity, so a ring freed after it is taken
    /// is reported as unknown rather than read through an address the
    /// allocator has handed to somebody else.
    ///
    /// What it pins reliably is the *repair*. On the defect it failed
    /// because the freed block still held its old contents and the read
    /// reported records; that is a use-after-free, so the shape of the
    /// failure was the allocator's to decide and Miri is what names it as
    /// one.
    #[test]
    fn a_ring_freed_after_the_mark_is_not_read_through_its_address() {
        const SUBJECT: u64 = 0x5EE;
        let _g = crate::memory::block_pool::test_guard();

        let start = mark();
        let identity = a_journaling_thread(SUBJECT);
        let end = mark();
        assert!(
            evict_retired_ring(identity),
            "the exited thread's ring was not on the retired list"
        );

        let answer = between(&start, &end)
            .into_iter()
            .find(|window| matches!(window, Window::Unknown { thread, .. } if *thread == identity));
        assert_eq!(
            answer,
            Some(Window::Unknown {
                thread: identity,
                written: Some(1)
            }),
            "a freed ring was read rather than reported"
        );
    }

    /// A window that ended before a ring closed keeps its answer when the
    /// thread later exits. `Closed` says "records were lost inside this
    /// window"; a close dated after the window lost it nothing, and an
    /// answer that changes under a reader's feet is one that stops
    /// meaning anything.
    #[test]
    fn a_window_that_ended_before_the_close_is_not_reclassified_by_it() {
        const SUBJECT: u64 = 0xC105;
        let _g = crate::memory::block_pool::test_guard();

        let start = mark();
        let (sender, receiver) = std::sync::mpsc::channel();
        let (go, wait) = std::sync::mpsc::channel();
        let journaling = std::thread::spawn(move || {
            crate::memory::heap::ll_thread_init();
            record(8, 0, SUBJECT, 0, 0);
            sender
                .send(this_thread_identity())
                .expect("the test hung up");
            wait.recv().expect("the test hung up");
            crate::memory::heap::ll_thread_exit();
        });
        let identity = receiver.recv().expect("the journaling thread hung up");
        let end = mark();

        // Both marks are taken while the thread is alive, so the window
        // is complete. Only then does the thread exit.
        go.send(()).expect("the journaling thread hung up");
        journaling.join().expect("the journaling thread panicked");

        let answer = between(&start, &end)
            .into_iter()
            .find(|window| match window {
                Window::Records(events) => events.iter().any(|event| event.thread == identity),
                _ => false,
            })
            .expect("the window lost the thread it was taken around");
        assert!(
            matches!(answer, Window::Records(_)),
            "a window that was complete stopped being one: {answer:?}"
        );
    }

    /// A pool thread runs `ll_thread_init` and `ll_thread_exit` once per
    /// task, so one OS thread is a sequence of thread lives. The second
    /// life journals into a ring of its own: without that it journals
    /// nothing at all and looks exactly like a thread doing nothing.
    #[test]
    fn a_second_life_on_one_thread_journals_into_a_ring_of_its_own() {
        const FIRST: u64 = 0x11FE;
        const SECOND: u64 = 0x21FE;
        let _g = crate::memory::block_pool::test_guard();
        let start = mark();

        let (first, second) = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            record(6, 0, FIRST, 0, 0);
            let first = this_thread_identity();
            crate::memory::heap::ll_thread_exit();

            crate::memory::heap::ll_thread_init();
            record(6, 0, SECOND, 0, 0);
            let second = this_thread_identity();
            crate::memory::heap::ll_thread_exit();
            (first, second)
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        assert_ne!(first, second, "the second life reused the first's ring");
        assert_ne!(second, 0, "the second life journaled nothing");
        let subjects: Vec<u64> = events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == first || event.thread == second)
            .map(|event| event.subject)
            .collect();
        assert_eq!(subjects, vec![FIRST, SECOND]);
    }

    /// A retiring thread frees no ring, because by then it cannot: its
    /// parked backlog is gone, and a ring's free parks while a collector
    /// epoch is in flight. The rings wait for a thread that is not on its
    /// way out — an investigator taking a mark is one.
    #[test]
    fn an_evicted_ring_is_freed_by_a_live_thread_rather_than_a_dying_one() {
        const SUBJECT: u64 = 0xEB0;
        let _g = crate::memory::block_pool::test_guard();
        let identity = a_journaling_thread(SUBJECT);

        {
            let mut registry = locked();
            evict_retired(&mut registry, 1);
        }
        assert_eq!(
            pending_count(),
            1,
            "the eviction freed on the spot instead of leaving the ring"
        );

        let _ = mark();
        assert_eq!(pending_count(), 0, "a mark left the eviction unfreed");
        // The oldest retired ring is whichever test ran before this one,
        // so the eviction above may have taken that rather than this
        // test's. Leave nothing of its own behind either way.
        let _ = evict_retired_ring(identity);
    }

    /// A window's two ends handed over the wrong way round bound no
    /// interval, and an empty list of answers reads as "nothing happened
    /// anywhere" — the one answer this module may not invent. It says so
    /// in one answer of its own, in the release build too, where an
    /// assertion would have been compiled out, and whether or not any
    /// ring is named: a pair taken before anything journaled names none,
    /// and a per-ring answer over no rings is the empty list again.
    #[test]
    fn two_marks_in_the_wrong_order_answer_that_they_bound_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        record(2, 0, 0x0D, 0, 0);
        let start = mark();
        let end = mark();

        assert_eq!(between(&end, &start), vec![Window::Reversed]);
        retire_thread_ring();
    }

    /// The same, with no ring in either mark — where a per-ring answer
    /// has nothing to answer over and the empty list comes back.
    #[test]
    fn a_reversed_pair_naming_no_ring_still_answers() {
        let _g = crate::memory::block_pool::test_guard();
        let start = Mark {
            positions: Vec::new(),
            evictions: 0,
            refusals: 0,
            taken: 1,
        };
        let end = Mark {
            positions: Vec::new(),
            evictions: 0,
            refusals: 0,
            taken: 2,
        };
        assert_eq!(between(&end, &start), vec![Window::Reversed]);
    }

    /// A `__destruct` body runs in step 1 of `heap::ll_thread_exit` and
    /// journals like any other code; the ring is retired by the last act
    /// of that same function, so the record lands in it. That ordering is
    /// the whole of what this journal was built for — the census
    /// hypothesis is about a *finishing* thread — and a retirement placed
    /// earlier loses exactly those records.
    #[test]
    fn a_destructor_at_thread_exit_is_recorded_before_the_ring_retires() {
        const SUBJECT: u64 = 0xD1E;
        let _g = crate::memory::block_pool::test_guard();
        let start = mark();

        let identity = std::thread::spawn(|| {
            use crate::class::ClassBuilder;
            use crate::memory::arena::Arena;
            use crate::memory::context::LLContext;
            use crate::refcount::MemoryCategory;
            use crate::value::{Tag, Value};

            /// A `__destruct` that journals, which is what a record site
            /// on the death path will do once §9.5's set is built.
            unsafe extern "C" fn journaling_destructor(_obj: *mut crate::object::Object) {
                record(1, 0, SUBJECT, 0, 0);
            }

            crate::memory::heap::ll_thread_init();
            let identity = {
                record(1, 0, 0, 0, 0);
                this_thread_identity()
            };

            // A static holding the object is what makes thread exit the
            // point its destructor runs (`static_block.rs`).
            let cls = ClassBuilder::new("JournalingAtExit")
                .destructor(journaling_destructor as *const ())
                .build();
            let holder = ClassBuilder::new("StaticsOfJournalingAtExit")
                .prop("kept", true)
                .build();
            let size = unsafe { (*holder).object_size } as usize;
            let block = unsafe {
                std::alloc::alloc_zeroed(std::alloc::Layout::from_size_align(size, 16).unwrap())
            };
            assert!(!block.is_null());

            let mut arena = Arena::new();
            let mut ctx = LLContext { arena: &mut arena };
            let obj =
                unsafe { crate::object::new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
            unsafe {
                let slot = block.add(16) as *mut Value;
                assert!(crate::memory::barrier::store_box(
                    &mut arena,
                    MemoryCategory::LongLived,
                    slot,
                    Value::entity(Tag::Object, obj as *mut crate::refcount::RcHeader),
                ));
                crate::static_block::ll_static_block_register(block, holder);
                // The static's store took the second reference.
                assert!(!crate::refcount::ll_release(
                    obj as *mut crate::refcount::RcHeader
                ));
            }

            crate::memory::heap::ll_thread_exit();
            identity
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        let subjects: Vec<u64> = events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == identity)
            .map(|event| event.subject)
            .collect();
        assert_eq!(
            subjects,
            vec![0, SUBJECT],
            "the destructor's record was raised into a ring already retired"
        );
    }

    /// A thread that has begun its own exit frees no evicted ring. Its
    /// deferral backlog is disposed inside that sequence and nothing
    /// rebuilds it, so a parked free there is dropped unreleased — and
    /// the exit a caller invokes by hand is the same sequence, which is
    /// what the exit guard's own state cannot tell.
    #[test]
    fn a_thread_inside_its_own_exit_takes_no_ring_to_free() {
        const SUBJECT: u64 = 0xF2F2;
        let _g = crate::memory::block_pool::test_guard();
        let identity = a_journaling_thread(SUBJECT);
        {
            let mut registry = locked();
            evict_retired(&mut registry, 1);
        }
        assert_eq!(pending_count(), 1, "the eviction left nothing to free");

        let taken = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            crate::memory::heap::ll_thread_exit();
            // Both doors into the pending list go through this.
            let mut registry = locked();
            take_pending(&mut registry).len()
        })
        .join()
        .expect("the exiting thread panicked");

        assert_eq!(taken, 0, "a thread past its own exit took a ring to free");
        assert_eq!(pending_count(), 1, "the ring was taken by somebody");
        // This thread is live, so it is one that may.
        let pending = std::mem::take(&mut locked().pending_free);
        free_rings(pending);
        let _ = evict_retired_ring(identity);
    }

    /// A thread that cannot arm its exit guard gets no ring: the guard is
    /// what retires one, and a ring nothing retires stays on the live
    /// list for the life of the process, where every later window reads
    /// it as a live thread doing nothing. The state is real — a
    /// destructor that allocates reaches a record site with the guard's
    /// slot already destroyed — and it is counted like a refusal, being
    /// the same silence from the reader's side.
    #[test]
    fn a_thread_that_cannot_arm_its_exit_guard_is_given_no_ring() {
        use crate::memory::heap::FORCE_GUARD_UNARMED;
        let _g = crate::memory::block_pool::test_guard();
        let counts_before = registry_counts();
        let start = mark();

        let identity = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            FORCE_GUARD_UNARMED.store(true, Ordering::Relaxed);
            record(4, 0, 5, 0, 0);
            let identity = this_thread_identity();
            FORCE_GUARD_UNARMED.store(false, Ordering::Relaxed);
            crate::memory::heap::ll_thread_exit();
            identity
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        assert_eq!(identity, 0, "a thread with no exit guard opened a ring");
        assert_eq!(
            registry_counts(),
            counts_before,
            "a ring nothing will retire was registered anyway"
        );
        let reported = between(&start, &end)
            .into_iter()
            .find_map(|window| match window {
                Window::Refused { threads } => Some(threads),
                _ => None,
            });
        assert_eq!(
            reported,
            Some(start.refusals + 1),
            "the thread left no trace in the window that covered it"
        );
    }

    /// A thread whose ring the allocator refused is in no window at all,
    /// so the count of such threads is the only thing standing between a
    /// reader and the conclusion that they did nothing.
    #[test]
    fn a_thread_refused_a_ring_is_counted_since_it_is_in_no_window() {
        use crate::memory::block_pool::FORCE_OOM;
        let _g = crate::memory::block_pool::test_guard();
        let start = mark();

        std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            FORCE_OOM.store(true, Ordering::Relaxed);
            record(4, 0, 3, 0, 0);
            FORCE_OOM.store(false, Ordering::Relaxed);
            crate::memory::heap::ll_thread_exit();
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        let reported = between(&start, &end)
            .into_iter()
            .find_map(|window| match window {
                Window::Refused { threads } => Some(threads),
                _ => None,
            });
        assert_eq!(
            reported,
            Some(start.refusals + 1),
            "a refused thread left no trace in the window that covered it"
        );
    }

    /// The retired list stops growing at [`RETIRED_KEPT`], and what it
    /// drops is the oldest. That bound is the only thing between a
    /// program that spawns a thread per request and a ring per request
    /// held for the life of the process.
    #[test]
    fn the_retired_list_keeps_the_newest_and_drops_the_oldest() {
        let _g = crate::memory::block_pool::test_guard();
        let (_, retired_before) = registry_counts();

        let mut mine = Vec::new();
        let mut freed = Vec::new();
        for _ in 0..=RETIRED_KEPT {
            let ring = allocate_ring();
            assert!(!ring.is_null());
            free_rings(register_ring(ring));
            mine.push(unsafe { (*ring).thread });
            retire_ring(ring);
            let evicted = std::mem::take(&mut locked().pending_free);
            for old in evicted {
                freed.push(unsafe { (*old).thread });
                unsafe { crate::memory::stdapi::ll_free(old as *mut u8) };
            }
        }

        let (_, retired_after) = registry_counts();
        assert_eq!(
            retired_after, RETIRED_KEPT,
            "the retired list outgrew its bound"
        );
        assert_eq!(
            freed.len(),
            retired_before + 1,
            "the list dropped a number of rings other than its overflow"
        );
        // Oldest first: everything that was there before, then the first
        // of this test's own.
        assert_eq!(freed.last(), mine.first().map(|_| &mine[0]));

        // Leave the list as it was found, so that a later test's window
        // does not carry this one's evictions.
        for thread in mine.into_iter().skip(1) {
            assert!(
                evict_retired_ring(thread),
                "a ring of this test went missing"
            );
        }
    }
}
