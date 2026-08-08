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
//! nothing if a silent eviction can produce the same answer.
//!
//! The ring is allocated through [`crate::memory::stdapi::ll_malloc`] on
//! the thread's first record, never from an arena and never through
//! `entity_alloc`: the journal has to be readable while the collector
//! holds an epoch and while an arena resets, and it records events raised
//! from inside the allocator without re-entering it. A refused allocation
//! turns journaling off for that thread and the journaled operation
//! proceeds.
//!
//! Rings outlive their threads, because a thread's records matter most
//! once it is gone. At exit the ring is handed to the registry's retired
//! list, which keeps the most recent [`RETIRED_KEPT`] and frees the rest.
//!
//! This module is compiled into every build. What §9.6 promises to
//! compile away without the `debug-journal` feature is the **record
//! sites** on the allocation and death paths, not the ring: a ring nobody
//! writes costs one thread-local pointer that stays null, and keeping the
//! module in the ordinary build is what keeps its tests inside the gate.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

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
    /// know which is which.
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
    /// including after that thread has exited.
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
    fn write(&self, kind: Kind, site: u32, subject: u64, a: u64, b: u64) {
        debug_assert_ne!(kind, KIND_UNSET, "kind 0 is the unset marker");
        let at = self.cursor.load(Ordering::Relaxed);
        let slot = &self.records[(at as usize) & (CAPACITY - 1)];
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
        let now = self.cursor.load(Ordering::Acquire);
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
}

// The registry holds rings by raw pointer, and a ring is shared by
// construction: its owner writes it while investigators on other threads
// read it. Every field crossing that boundary is an atomic, and the
// allocation outlives every reader by the retirement rule above.
unsafe impl Send for Registry {}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
        live: Vec::new(),
        retired: Vec::new(),
    });
    &REGISTRY
}

thread_local! {
    /// This thread's ring, or null when it has none — never allocated,
    /// or refused, or already retired at thread exit.
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
/// `kind` must not be [`KIND_UNSET`]. `site` is `0` when unknown;
/// `subject` is the address the event is about; `a` and `b` are the
/// kind's own two words.
pub fn record(kind: Kind, site: u32, subject: u64, a: u64, b: u64) {
    let ring = ring_for_writing();
    if ring.is_null() {
        return;
    }
    unsafe { (*ring).write(kind, site, subject, a, b) };
}

/// This thread's ring, allocated and registered on first use. Null when
/// the allocation was refused, and null while this call is itself inside
/// that allocation.
fn ring_for_writing() -> *mut Ring {
    let existing = RING.with(|cell| cell.get());
    if !existing.is_null() {
        return existing;
    }
    if ALLOCATING.with(|cell| cell.get()) {
        return std::ptr::null_mut();
    }
    ALLOCATING.with(|cell| cell.set(true));
    let fresh = allocate_ring();
    ALLOCATING.with(|cell| cell.set(false));
    if fresh.is_null() {
        return std::ptr::null_mut();
    }
    RING.with(|cell| cell.set(fresh));
    registry()
        .lock()
        .expect("journal registry poisoned")
        .live
        .push(fresh);
    fresh
}

/// A zeroed ring stamped with this thread's identity, or null if the
/// allocator refused. Zeroed is what makes an unwritten ring read as
/// empty: `KIND_UNSET` is zero and so is a fresh cursor.
fn allocate_ring() -> *mut Ring {
    let bytes = size_of::<Ring>();
    let memory = unsafe { crate::memory::stdapi::ll_malloc(bytes) };
    if memory.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { std::ptr::write_bytes(memory, 0, bytes) };
    let ring = memory as *mut Ring;
    // The address of a thread-local is this crate's thread identity
    // everywhere else it needs one; `std::thread::ThreadId` is not
    // available to a `#[no_mangle]` C-ABI caller and is not comparable
    // across the FFI boundary either.
    unsafe { (&raw mut (*ring).thread).write(thread_identity()) };
    ring
}

/// A number naming this thread for the life of its ring. Derived from the
/// address of a thread-local cell, which is unique among live threads;
/// after a thread exits the address may be reused, and a retired ring
/// keeps the number it was stamped with, so identity is unique among
/// *live* rings rather than over the process's history.
fn thread_identity() -> u64 {
    RING.with(|cell| cell as *const _ as u64)
}

/// Hand this thread's ring to the retired list, and free the oldest
/// retired rings beyond [`RETIRED_KEPT`].
///
/// Called from `heap::ll_thread_exit` and nowhere else. It goes **last**
/// in that sequence, after every structure whose teardown is worth
/// journaling, and before the thread's heaps go away — the ring's memory
/// came from `ll_malloc`, and freeing an evicted ring needs an allocator.
pub fn retire_thread_ring() {
    let ring = RING.with(|cell| cell.replace(std::ptr::null_mut()));
    if ring.is_null() {
        return;
    }
    let evicted = {
        let mut registry = registry().lock().expect("journal registry poisoned");
        registry.live.retain(|&live| live != ring);
        registry.retired.push(ring);
        let over = registry.retired.len().saturating_sub(RETIRED_KEPT);
        registry.retired.drain(..over).collect::<Vec<_>>()
    };
    // Outside the lock: the free is another module's business and can be
    // a cross-thread one, which the registry has no reason to serialize.
    for old in evicted {
        unsafe { crate::memory::stdapi::ll_free(old as *mut u8) };
    }
}

/// Where every ring stood at one moment — the two ends of a window are
/// two of these.
#[derive(Clone, Debug, Default)]
pub struct Mark {
    positions: Vec<(*mut Ring, u64)>,
}

/// A window's answer for one ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Window {
    /// Every record written inside the window, oldest first.
    Records(Vec<Event>),
    /// The window overflowed this ring: more records were written than it
    /// holds, so what was evicted cannot be named. **Not the same as an
    /// empty answer**, and the distinction is the reason a hunt can trust
    /// "nothing happened".
    Unknown { thread: u64, written: u64 },
}

/// Read every registered ring's cursor. One end of a window; the other is
/// another call to this after the interval.
pub fn mark() -> Mark {
    let registry = registry().lock().expect("journal registry poisoned");
    let positions = registry
        .live
        .iter()
        .chain(registry.retired.iter())
        .map(|&ring| (ring, unsafe { (*ring).cursor.load(Ordering::Acquire) }))
        .collect();
    Mark { positions }
}

/// What was recorded between two marks, one answer per ring.
///
/// A ring that did not exist at `start` is read from its beginning, which
/// is the correct answer rather than an approximation: a thread that
/// started inside the window has no earlier records. A ring whose owner
/// exited inside the window keeps its final cursor and is read like any
/// other.
///
/// The caller owns the ordering caveat: records from one ring are in that
/// thread's program order, and records from different rings have no order
/// between them.
pub fn between(start: &Mark, end: &Mark) -> Vec<Window> {
    end.positions
        .iter()
        .map(|&(ring, end_at)| {
            let start_at = start
                .positions
                .iter()
                .find(|&&(earlier, _)| earlier == ring)
                .map_or(0, |&(_, at)| at);
            window_of(ring, start_at, end_at)
        })
        .collect()
}

/// One rule decides an overflow, and it is the read-back:
/// [`Ring::read_at`] refuses a position the owner has lapped. Counting
/// `end - start` against [`CAPACITY`] first would be a second rule saying
/// the same thing — an early-out worth at most `CAPACITY` copies to an
/// investigator, at the price of two rules that can disagree.
fn window_of(ring: *mut Ring, start_at: u64, end_at: u64) -> Window {
    let thread = unsafe { (*ring).thread };
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
mod tests {
    use super::*;

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
        for (offset, subject) in [(3, CAPACITY as u64), (2, CAPACITY as u64 + 1)] {
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

        let mine = thread_identity();
        let inside: Vec<u64> = between(&start, &end)
            .into_iter()
            .filter_map(|window| match window {
                Window::Records(events) => Some(events),
                Window::Unknown { .. } => None,
            })
            .flatten()
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

        let mine = thread_identity();
        let mut seen = false;
        for window in between(&start, &end) {
            if let Window::Unknown { thread, written } = window
                && thread == mine
            {
                assert_eq!(written, CAPACITY as u64 * 2);
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

        let joined = std::thread::spawn(|| {
            crate::memory::heap::ll_thread_init();
            record(5, 0, SUBJECT, 0, 0);
            let id = thread_identity();
            retire_thread_ring();
            id
        })
        .join()
        .expect("the journaling thread panicked");

        let end = mark();
        let found = between(&start, &end)
            .into_iter()
            .filter_map(|window| match window {
                Window::Records(events) => Some(events),
                Window::Unknown { .. } => None,
            })
            .flatten()
            .any(|event| event.thread == joined && event.subject == SUBJECT);
        assert!(found, "the exited thread's ring left the window with it");
    }
}
