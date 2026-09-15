//! The owner's record: the words of one mutator thread that a collector
//! thread reaches — its trace token, and beside it the owner's note of who
//! holds that token — in storage that outlives the thread
//! (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff"; `dev/DECISIONS.md`,
//! "the token stands in a record the process keeps, and the exit's claim on
//! it is never released"). The candidate ring a collector reads behind its
//! writer, and the verdict ring the owner reads back, stand in this record
//! too once they are built (`rfc/dev/DECISIONS.md`, "the candidate queue is
//! read behind its writer, and the collector's verdicts come back by a
//! second ring"); until then the record is the token's line alone.
//!
//! # Why the storage outlives the thread
//!
//! A collector's first access to an owner is a load of a word of its record,
//! made before it holds any token. Nothing in the design covers a read made
//! under no claim except the lifetime of what is read: a word in a
//! thread-local dies with the thread, and a word in a block the exit returns
//! can be read after the pool has reissued the block. So the records stand
//! in a chain of GC-metadata blocks the process never returns, and a record
//! a thread has finished with goes to a free list for the next thread rather
//! than back to the pool.
//!
//! # The token is what says whether a record is anyone's
//!
//! A record's token is held from the moment the registry carves it until the
//! thread that took it has finished its initialisation, and again from the
//! exit's final claim until the next thread's initialisation ends. A worker's
//! claim is a compare-and-swap from free, so it fails on a record nobody has
//! taken yet, on one an exit has released, and on one whose next thread is
//! not yet ready — without a liveness word of its own, which would have to be
//! ordered against the token anyway. The exit's claim is never released: the
//! record goes to the free list held, and the taker releases it
//! ([`initialize_thread_record`]). Until that claim — through the static
//! blocks' teardown, the exit's first step — the token is free and a claim
//! succeeds, which is what the claim's wait is for. A thread that never
//! exits — one the runtime never registered, taking its record at its first
//! collection — keeps its record claimable for the life of the process.
//!
//! **The exit draws no record.** A thread that reaches its exit without one
//! is reached by no collector, so its claim is empty, and a record taken by
//! one of the exit's rounds would be released by that round's guard and go
//! to the free list free — a record a worker could then claim with nobody
//! in it. [`ensure_thread_record`] answers null while the exit runs.
//!
//! # The owner's own claim, told from a foreign one
//!
//! The free path withholds a return while a foreign holder has the token
//! (`crate::cycle::deferred_slot_reuse`, "A foreign holder of the token"),
//! and the exit's rounds of collection run under the owner's own claim, so
//! the word alone does not say which. [`OwnerRecord::owner_holds`] is the
//! owner's note to itself, written beside its take and its release and read
//! only when the token reads held, so the free path's common case stays one
//! load. The ruling that made the token one bit dissolved the holder kind
//! because no reader needed it (`rfc/dev/DECISIONS.md`, "the trace token
//! covers the trace alone, and the accelerator hands off by buffer swap");
//! the exit's held claim is the reader that does, and its note is the
//! owner's rather than the word's, so a worker still reads one bit.
//!
//! The collector thread that makes the round, and the round itself, are
//! `crate::cycle::worker`; the round reaches every record through
//! [`for_each_record`].

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cycle::token::TraceToken;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata;

/// One mutator thread's record. Sixty-four bytes, one line, so that the
/// worker's loads of one owner touch nothing of another's.
#[repr(C, align(64))]
pub(crate) struct OwnerRecord {
    /// The trace token, taken by a collector thread around its trace of this
    /// owner's graph and by the owner around its own.
    pub(crate) token: TraceToken,
    /// The next free record, meaningful while this one is on the registry's
    /// free list and written under its lock alone.
    free_link: Cell<*mut OwnerRecord>,
    /// Whether the holder of [`OwnerRecord::token`] is the owner itself.
    /// Written by the owner alone, beside its take and its release, and
    /// read by the owner alone, so relaxed on both sides.
    owner_holds: AtomicBool,
    /// Whether a case has asked the registry to leave this record on the
    /// free list: other tests' threads start and exit under the parallel
    /// harness, and a record they could pop is one no case can read after
    /// its own thread's exit.
    #[cfg(test)]
    pinned: AtomicBool,
}

// The free link is written under the registry's lock and every other field
// is atomic, which is what lets a record be reached from two threads.
unsafe impl Sync for OwnerRecord {}

const _: () = assert!(size_of::<OwnerRecord>() == 64);
const _: () = assert!(
    !std::mem::needs_drop::<OwnerRecord>(),
    "a record is written in place and never dropped"
);

/// Records one block's payload holds.
const RECORDS_PER_BLOCK: usize = BLOCK_PAYLOAD / size_of::<OwnerRecord>();

/// The process's records: the chain of blocks they stand in, how far the head
/// block is carved, and the records threads have finished with.
struct Registry {
    /// The block records are carved from, and behind it through
    /// [`BlockHeader::next`] every block carved before it.
    head: *mut BlockHeader,
    /// Records carved out of `head` so far.
    carved: usize,
    /// Records released by exited threads, each with its token held.
    free: *mut OwnerRecord,
}

// The pointers are the registry's own and are touched under its lock.
unsafe impl Send for Registry {}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    head: std::ptr::null_mut(),
    carved: 0,
    free: std::ptr::null_mut(),
});

thread_local! {
    /// Non-owning locator of this thread's record, null while it has none.
    /// A `const` cell of a pointer, so the exit path may read it after other
    /// thread-locals are gone.
    static OWNER_RECORD: Cell<*mut OwnerRecord> = const { Cell::new(std::ptr::null_mut()) };
}

impl OwnerRecord {
    /// A record in the state the registry hands out: token held by nobody in
    /// particular.
    const fn taken() -> Self {
        Self {
            token: TraceToken::new_held(),
            free_link: Cell::new(std::ptr::null_mut()),
            owner_holds: AtomicBool::new(false),
            #[cfg(test)]
            pinned: AtomicBool::new(false),
        }
    }

    /// Whether a thread other than the owner holds the token now: a reading,
    /// stale in both directions in the ways [`TraceToken::is_held`] names.
    #[inline]
    pub(crate) fn held_by_another(&self) -> bool {
        self.token.is_held() && !self.owner_holds.load(Ordering::Relaxed)
    }
}

/// Call `visit` on every record the registry has carved so far, in no
/// particular order: the records threads live in, the ones on the free list
/// and the calling thread's own alike, since a record is never returned and
/// a walk cannot tell them apart without a claim. The registry's lock is held
/// for the reading of how far the carve got and not across the visits, so a
/// record carved during the walk is the next walk's.
pub(crate) fn for_each_record(mut visit: impl FnMut(*mut OwnerRecord)) {
    let (head, carved) = {
        let registry = REGISTRY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (registry.head, registry.carved)
    };

    // Every block behind the head is carved whole: a block leaves the head
    // position only once `carved` reaches its capacity.
    let mut block = head;
    let mut records = carved;
    while !block.is_null() {
        let first = BlockHeader::payload_start(block) as *mut OwnerRecord;
        for index in 0..records {
            visit(unsafe { first.add(index) });
        }

        block = unsafe { (*block).next };
        records = RECORDS_PER_BLOCK;
    }
}

/// This thread's record, or null while it has none.
#[inline]
pub(crate) fn this_thread_record() -> *mut OwnerRecord {
    OWNER_RECORD.with(Cell::get)
}

/// This thread's record, taking one from the registry if it has none yet.
/// Null when the pool refuses the block a fresh record would stand in, and
/// the next call asks again.
///
/// The record comes out with its token held; the caller is the one that
/// releases it, or keeps it as its own claim ([`crate::cycle::token::HeldToken`]).
/// Whether the token was just taken is `taken`, so the caller can tell a
/// record it already lived in from one it has this instant.
pub(crate) fn ensure_thread_record() -> (*mut OwnerRecord, bool) {
    let present = this_thread_record();
    if !present.is_null() {
        return (present, false);
    }

    if crate::memory::heap::thread_exit_running() {
        return (std::ptr::null_mut(), true);
    }

    let record = take_record();
    if !record.is_null() {
        OWNER_RECORD.with(|cell| cell.set(record));
        #[cfg(test)]
        RECORDS_TAKEN.with(|count| count.set(count.get() + 1));
    }

    (record, true)
}

/// Give this thread a record at its initialisation and make it claimable:
/// the token is released here, once nothing else of the initialisation is
/// left to do. Answers false when the pool refused; the thread then takes a
/// record at its first collection instead
/// (`crate::memory::heap::ll_thread_init`).
pub(crate) fn initialize_thread_record() -> bool {
    let (record, taken) = ensure_thread_record();
    if record.is_null() {
        return false;
    }

    if taken {
        unsafe { (*record).token.release() };
    }

    true
}

/// Note whether the owner holds its own token. Written by
/// the owner beside its take and its release, and by nothing else.
///
/// # Safety
/// `record` is this thread's record and this thread is the holder whose
/// claim the note describes.
#[inline]
pub(crate) unsafe fn note_owner_holds(record: *mut OwnerRecord, holds: bool) {
    unsafe { (*record).owner_holds.store(holds, Ordering::Relaxed) };
}

/// Whether the owner's own claim stands on `record`.
///
/// # Safety
/// `record` is this thread's record.
#[inline]
pub(crate) unsafe fn owner_holds(record: *mut OwnerRecord) -> bool {
    unsafe { (*record).owner_holds.load(Ordering::Relaxed) }
}

/// Give this thread's record back to the registry, for the next thread.
///
/// The token stays held: the exit's final claim is what stands on it, and
/// the thread that takes the record next releases it when its own
/// initialisation is complete. A thread that never had a record has nothing
/// to give back.
///
/// # Safety
/// This thread holds its record's token and will not touch the record again.
pub(crate) unsafe fn release_thread_record() {
    let record = OWNER_RECORD.with(|cell| cell.replace(std::ptr::null_mut()));
    if record.is_null() {
        return;
    }

    debug_assert!(
        unsafe { (*record).token.is_held() && owner_holds(record) },
        "a record goes back under the exit's own claim"
    );
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    unsafe { (*record).free_link.set(registry.free) };
    registry.free = record;
}

/// Take a record out of the registry: a released one first, then one carved
/// out of the head block, then one out of a block drawn for it. Null when
/// the pool refuses that draw.
fn take_record() -> *mut OwnerRecord {
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let released = first_free_record(&mut registry);
    if !released.is_null() {
        // In place rather than a fresh `taken()`: a worker's pointer to the
        // token outlives the last life, and the word it will compare must be
        // the held one the exit left rather than a rewritten one.
        unsafe {
            (*released).owner_holds.store(false, Ordering::Relaxed);
            (*released).free_link.set(std::ptr::null_mut());
        }
        return released;
    }

    if registry.head.is_null() || registry.carved == RECORDS_PER_BLOCK {
        let block = gc_metadata::acquire();
        if block.is_null() {
            return std::ptr::null_mut();
        }

        unsafe { (*block).next = registry.head };
        registry.head = block;
        registry.carved = 0;
    }

    let record = unsafe {
        (BlockHeader::payload_start(registry.head) as *mut OwnerRecord).add(registry.carved)
    };
    registry.carved += 1;
    gc_metadata::charge(size_of::<OwnerRecord>());
    unsafe { record.write(OwnerRecord::taken()) };
    record
}

/// Unlink and answer the first record of the free list, or null.
#[cfg(not(test))]
fn first_free_record(registry: &mut Registry) -> *mut OwnerRecord {
    let record = registry.free;
    if !record.is_null() {
        registry.free = unsafe { (*record).free_link.get() };
    }

    record
}

/// Unlink and answer the first record of the free list a taker may have, or
/// null: every record on the list but a pinned one.
#[cfg(test)]
fn first_free_record(registry: &mut Registry) -> *mut OwnerRecord {
    let mut link: *mut *mut OwnerRecord = &raw mut registry.free;
    loop {
        let record = unsafe { *link };
        if record.is_null() {
            return record;
        }

        if unsafe { (*record).pinned.load(Ordering::Relaxed) } {
            link = unsafe { (*record).free_link.as_ptr() };
            continue;
        }

        unsafe { *link = (*record).free_link.get() };
        return record;
    }
}

/// Keep `record` on the free list, or let it go again, for a case that
/// reads a record after its thread's exit.
#[cfg(test)]
pub(crate) fn pin_for_test(record: *mut OwnerRecord, pinned: bool) {
    unsafe { (*record).pinned.store(pinned, Ordering::Relaxed) };
}

#[cfg(test)]
thread_local! {
    /// Records this thread has taken out of the registry, for a case that
    /// reads whether an exit drew one.
    static RECORDS_TAKEN: Cell<usize> = const { Cell::new(0) };
}

/// How many records this thread has taken out of the registry so far.
#[cfg(test)]
pub(crate) fn records_taken() -> usize {
    RECORDS_TAKEN.with(Cell::get)
}

/// Whether `record` stands on the registry's free list now, for a case that
/// reads the exit's hand-back. Other tests' threads exit under the parallel
/// harness, so a count of the list would move under a case; membership of
/// one record does not.
#[cfg(test)]
pub(crate) fn registry_lists_free(record: *mut OwnerRecord) -> bool {
    let registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut free = registry.free;
    while !free.is_null() {
        if free == record {
            return true;
        }

        free = unsafe { (*free).free_link.get() };
    }

    false
}

/// Whether `block` is one of the registry's, for a case that reads where a
/// record stands.
#[cfg(test)]
pub(crate) fn registry_owns_block(block: *mut BlockHeader) -> bool {
    let registry = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut head = registry.head;
    while !head.is_null() {
        if head == block {
            return true;
        }

        head = unsafe { (*head).next };
    }

    false
}

#[cfg(test)]
mod tests;
