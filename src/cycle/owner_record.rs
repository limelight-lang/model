//! The owner's record: the words of one mutator thread that a collector
//! thread reaches — its trace token, and beside it the outbox, the inbox
//! and the request word the worker's handoff uses — in storage that outlives
//! the thread (`rfc/model/gc/rc-cycle.md`, "Worker-to-owner handoff";
//! `rfc/dev/DECISIONS.md`, "the owner detaches at its poll, and the worker
//! takes the chain from a one-word outbox").
//!
//! # Why the storage outlives the thread
//!
//! A worker's first access to an owner is a load of its outbox word, made
//! before it holds any token. Nothing in the design covers a read made under
//! no claim except the lifetime of what is read: a word in a thread-local dies
//! with the thread, and a word in a block the exit returns can be read after
//! the pool has reissued the block. So the records stand in a chain of
//! GC-metadata blocks the process never returns, and a record a thread has
//! finished with goes to a free list for the next thread rather than back to
//! the pool.
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
//! collection — keeps its record claimable for the life of the process; it
//! offers nothing, so a worker that reads its outbox never claims it.
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
//! # The three words beside the token
//!
//! The outbox carries a chain the owner detached at its poll on a worker's
//! request, the inbox the chain the worker traced and posted back, the
//! request word the worker's ask, and beside them the owner's note that it
//! ran short, which the worker's ask relays. Each is one word, and the owner is the
//! only party that fills the outbox and empties the inbox, the worker the
//! only one that empties the outbox and fills the inbox: the exchanges are
//! how the two hand a chain across without either reading the other's
//! (`crate::cycle::queue`, "The chain a collector thread takes"). The worker
//! touches the outbox and the inbox only under the token; its one earlier
//! access is a load of the outbox word. The collector thread that makes the
//! round, and the round itself, are `crate::cycle::worker`; the round reaches
//! every record through [`for_each_record`].

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
    /// The chain the owner's poll detached for a worker: the head segment's
    /// address with its fill in the low sixteen bits, or zero.
    outbox: AtomicUsize,
    /// The chain a worker traced and posted back, in the same form, or zero.
    inbox: AtomicUsize,
    /// The next free record, meaningful while this one is on the registry's
    /// free list and written under its lock alone.
    free_link: Cell<*mut OwnerRecord>,
    /// Whether the holder of [`OwnerRecord::token`] is the owner itself.
    /// Written by the owner alone, beside its take and its release, and
    /// read by the owner alone, so relaxed on both sides.
    owner_holds: AtomicBool,
    /// Whether a worker asked the owner's next poll for an offer.
    request: AtomicBool,
    /// Whether the owner ran short of memory since a worker last relayed
    /// that into a request: the owner's pressure path writes it, the
    /// collector thread's round takes it ([`note_shortage`], [`take_shortage`]).
    shortage: AtomicBool,
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
    /// particular, nothing offered, nothing posted, nothing asked.
    const fn taken() -> Self {
        Self {
            token: TraceToken::new_held(),
            outbox: AtomicUsize::new(0),
            inbox: AtomicUsize::new(0),
            free_link: Cell::new(std::ptr::null_mut()),
            owner_holds: AtomicBool::new(false),
            request: AtomicBool::new(false),
            shortage: AtomicBool::new(false),
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

/// Publish a detached chain to `record`'s outbox for a collector thread,
/// answering false when an earlier offer still stands there. The store is a
/// release: every entry and link store of the chain precedes it, and the
/// worker's acquire exchange follows it ([`take_offer`]).
///
/// # Safety
/// `record` is this thread's record and `word` names a chain this thread
/// detached and will not touch until it comes back through [`reclaim`] or
/// [`take_inbox`].
pub(crate) unsafe fn offer(record: *mut OwnerRecord, word: usize) -> bool {
    unsafe { &*record }
        .outbox
        .compare_exchange(0, word, Ordering::Release, Ordering::Relaxed)
        .is_ok()
}

/// Whether an offer stands untaken in `record`'s outbox.
///
/// # Safety
/// `record` is a record of the registry's.
pub(crate) unsafe fn offer_stands(record: *mut OwnerRecord) -> bool {
    unsafe { &*record }.outbox.load(Ordering::Relaxed) != 0
}

/// Take the offer back, answering the chain's word or zero when a worker took
/// it: the owner's exchange before it collects in line and before its exit.
///
/// # Safety
/// `record` is this thread's record.
pub(crate) unsafe fn reclaim(record: *mut OwnerRecord) -> usize {
    unsafe { &*record }.outbox.swap(0, Ordering::Acquire)
}

/// Take the offered chain for a trace, answering its word or zero when the
/// owner reclaimed it meanwhile. The collector thread's exchange, made after
/// it claimed `record`'s token and before its first read of the chain.
///
/// # Safety
/// The caller holds `record`'s token.
pub(crate) unsafe fn take_offer(record: *mut OwnerRecord) -> usize {
    unsafe { &*record }.outbox.swap(0, Ordering::Acquire)
}

/// Post a traced chain to `record`'s inbox for the owner's pickup, marked or
/// not. A release store, made before the token's release store; the inbox is
/// empty by the worker's own check before its take.
///
/// # Safety
/// The caller holds `record`'s token, the inbox is empty, and `word` names
/// the chain the caller took from the outbox.
pub(crate) unsafe fn post(record: *mut OwnerRecord, word: usize) {
    let previous = unsafe { &*record }.inbox.swap(word, Ordering::Release);
    debug_assert_eq!(previous, 0, "a chain was posted over one not yet picked up");
}

/// Whether a posted chain stands unpicked in `record`'s inbox.
///
/// # Safety
/// `record` is a record of the registry's.
pub(crate) unsafe fn proposal_stands(record: *mut OwnerRecord) -> bool {
    unsafe { &*record }.inbox.load(Ordering::Relaxed) != 0
}

/// Take the posted chain, answering its word or zero: the owner's pickup at
/// its poll and at its exit.
///
/// # Safety
/// `record` is this thread's record.
pub(crate) unsafe fn take_inbox(record: *mut OwnerRecord) -> usize {
    unsafe { &*record }.inbox.swap(0, Ordering::Acquire)
}

/// Ask `record`'s owner to offer its lane at its next poll. The collector
/// thread's write; the poll clears it as it offers ([`take_request`]).
///
/// # Safety
/// `record` is a record of the registry's.
pub(crate) unsafe fn request(record: *mut OwnerRecord) {
    unsafe { &*record }.request.store(true, Ordering::Relaxed);
}

/// Whether a worker asked this owner for an offer, clearing the request.
///
/// # Safety
/// `record` is this thread's record.
pub(crate) unsafe fn take_request(record: *mut OwnerRecord) -> bool {
    unsafe { &*record }.request.swap(false, Ordering::Relaxed)
}

/// Note that this owner ran short of memory: the pressure path's write,
/// which the collector thread's next round turns into a request
/// ([`take_shortage`]). The note is what makes an owner offer its lane at
/// all — a worker asks only an owner that ran short since it last asked
/// (`dev/DECISIONS.md`, "the worker relays the owner's shortage into its
/// request").
///
/// # Safety
/// `record` is this thread's record.
pub(crate) unsafe fn note_shortage(record: *mut OwnerRecord) {
    unsafe { &*record }.shortage.store(true, Ordering::Relaxed);
}

/// Whether the owner ran short since the last call, clearing the note. The
/// collector thread's read, made in the round that relays it.
///
/// # Safety
/// `record` is a record of the registry's.
pub(crate) unsafe fn take_shortage(record: *mut OwnerRecord) -> bool {
    unsafe { &*record }.shortage.swap(false, Ordering::Relaxed)
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
            (*released).outbox.store(0, Ordering::Relaxed);
            (*released).inbox.store(0, Ordering::Relaxed);
            (*released).request.store(false, Ordering::Relaxed);
            (*released).shortage.store(false, Ordering::Relaxed);
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
