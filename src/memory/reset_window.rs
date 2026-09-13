//! The window an arena reset holds over its own frees.
//!
//! A reset collects survivors during its fixpoint and reads that list
//! again after it (`promote::arena_reset_full`). Between the two, the
//! release-log drain can kill a survivor — the shape is a heap `&` box
//! stored into an arena slot, whose logged release tears the promoted
//! entity down. The passes that follow must still be able to read one
//! word of every address they hold — the weak walk and
//! `retained::is_occupied` both do — while whether an entity is torn down
//! is decided by the bit its teardown left in that word rather than by the
//! count in it (`dev/DECISIONS.md`, "the reset reads no corpse", and "the
//! record of a torn-down entity is its own header bit").
//!
//! For a survivor in a shared retained block that costs nothing: such a
//! block recycles nothing inside itself, so the torn-down entity's header
//! stays where it is. For a survivor in a block of its own it is not free —
//! `large_entity::free` hands a 128 KiB-and-up run back to the system,
//! and the next reader of that address reads memory the process no longer
//! owns. So while the window is open the free of either large-entity kind
//! is deferred and made after it closes.
//!
//! **This is the reset's window, not the collector's.** It turns on no
//! GC state: a collector withholds against a collection, this defers
//! against a reset, and the two are independent. The deferred free
//! re-enters `ll_free`, so a body meets whatever holds it then: a
//! collection's own withholding if one is in flight, or — when a
//! destructor of an outer reset drove this one — nothing, the outermost
//! close being the one that makes every deferred free.
//!
//! The window also **absorbs** one free rather than deferring it: an
//! unregistered torn-down entity in a retained block whose occupant count
//! is not established yet. Its death is already accounted for, because
//! `retained::register` declines to count it. A registered candidate is the
//! exception: the register counts its held slot, so owner-side retirement has
//! one count to spend and the raw queue pointer keeps its allocation identity.
//!
//! # What it owns, and where that memory comes from
//!
//! **Nothing here is a Rust container**, and no path of the window can end
//! the process on an allocation the manager could have refused
//! (`dev/DECISIONS.md`, "the reset window's memory comes from the manager,
//! and an allocation it cannot get is a refusal"). The window struct stands
//! in the reset's own frame ([`open`]); a deferred free stands in the body
//! it defers, one stack per thread threaded through byte 8 of each
//! ([`deferred_link`]); torn-down membership is a bit in the entity's own
//! header ([`is_torn_down`]); and the one structure that grows with the
//! reset — the log the reconciliation reads, holding a capture of each COW
//! survivor's count beside the promotion-time edges and compensating
//! retains that correct it ([`Record`]) — is drawn through
//! `stdapi::ll_alloc` in segments, and a segment the manager refuses is
//! answered to the recorder as a refusal ([`record_promotion_edge`],
//! [`record_cow_capture`]).
//!
//! The thread-locals are `Cell<*mut _>` rather than `RefCell<Vec<_>>`: a
//! `Vec` in a thread-local registers drop glue, and this path is reachable
//! from thread exit, where TLS destructor order is unspecified. Every
//! thread-local on that path took this shape on 2026-08-03.

use std::cell::Cell;
use std::marker::PhantomData;

use crate::refcount::RcHeader;

/// One reset in flight on this thread. Declared by the reset in its own
/// frame and lent to [`open`]; a closed one is [`ResetWindow::closed`].
pub(crate) struct ResetWindow {
    /// The newest segment of this window's log, or null while the log is
    /// empty. Segments chain through [`SegmentHeader::next`], newest first.
    log: *mut Segment,
    /// Whether a promotion-edge record was refused since the reset last
    /// asked ([`take_refused_promotion_edge`]).
    refused_promotion_edge: bool,
    /// Whether the log stands ordered by child right now: set by
    /// [`order_log_by_child`] and cleared by the append that leaves the
    /// order stale. Read by the searches, which the order is what they
    /// stand on, and only in a debug build — no other reader depends on it.
    log_ordered: bool,
    /// The window this one displaced. A destructor run by one reset can
    /// resolve another arena and reset it, so the windows nest and each
    /// close restores its predecessor.
    prev: *mut ResetWindow,
    /// The arena this window is open for, which [`open`] reads off the
    /// chain to refuse a second reset of the same one.
    arena: usize,
}

impl ResetWindow {
    /// A window that is not open: what a reset declares before [`open`].
    pub(crate) const fn closed() -> Self {
        ResetWindow {
            log: std::ptr::null_mut(),
            refused_promotion_edge: false,
            log_ordered: false,
            prev: std::ptr::null_mut(),
            arena: 0,
        }
    }
}

thread_local! {
    /// The innermost open window, or null.
    static WINDOW: Cell<*mut ResetWindow> = const { Cell::new(std::ptr::null_mut()) };

    /// The newest deferred free, or null. One stack for the whole window
    /// chain: an inner reset's deferred body may be an outer reset's
    /// survivor, whose passes still read its header, so only the outermost
    /// close makes any deferred free.
    static DEFERRED_FREES: Cell<*mut u8> = const { Cell::new(std::ptr::null_mut()) };
}

/// A window closed by leaving the scope that opened it, whatever ends
/// that scope. An unwind is reachable — `arena_reset_full` asserts on a
/// fixpoint that will not converge, and a `__destruct` body can panic —
/// and a window left open would defer every later large free on the
/// thread into a stack nobody pops and absorb every later retained free
/// into nothing. The guard borrows the window's storage for its whole
/// life, so the frame the thread-local points into cannot move or be
/// touched behind it.
#[must_use = "the window closes when the guard drops, so it must be held"]
pub(crate) struct Guard<'frame> {
    window: *mut ResetWindow,
    frame: PhantomData<&'frame mut ResetWindow>,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        unsafe { close(self.window) };
    }
}

/// Open a window for the reset of `arena`, about to run on this thread,
/// over storage the reset declares in its own frame; closed when the guard
/// leaves scope.
///
/// **Refuses a second reset of an arena already being reset on this
/// thread**, in every build. Nesting itself is ordinary — a destructor can
/// resolve another arena and reset that — but the same arena twice would
/// have the inner `finish_reset` return the blocks the outer reset's logs,
/// survivors and chains stand in, and the outer walk would then read memory
/// the pool has handed on. No program can ask for it: a `__destruct` body
/// reaches the runtime to allocate, to log an escape and to track a
/// destructor (`memory::context::resolve_arena`), while `ll_arena_reset` is
/// the host's call at the end of a request. The check is here so that a
/// host or a test that makes the call anyway is told which rule it broke,
/// rather than reaching an ownership assert in `memory::gc_metadata` that
/// names a block and mentions neither arena nor reset.
pub(crate) fn open(window: &mut ResetWindow, arena: *mut crate::memory::arena::Arena) -> Guard<'_> {
    let mut open_window = WINDOW.with(|cell| cell.get());
    while !open_window.is_null() {
        assert_ne!(
            unsafe { (*open_window).arena },
            arena as usize,
            "this arena is already being reset on this thread"
        );
        open_window = unsafe { (*open_window).prev };
    }

    window.log = std::ptr::null_mut();
    window.refused_promotion_edge = false;
    window.log_ordered = false;
    window.arena = arena as usize;
    window.prev = WINDOW.with(|cell| cell.get());
    let window: *mut ResetWindow = window;
    WINDOW.with(|cell| cell.set(window));
    Guard {
        window,
        frame: PhantomData,
    }
}

/// Close `window`, give its log back, and — when it is the outermost — make
/// every deferred free of the chain.
///
/// The frees run **after** the close, so each takes the ordinary route:
/// with a trace in flight an entity body is withheld again, since a shadow
/// row may still hold the address, and otherwise it goes back now
/// (`crate::cycle::deferred_slot_reuse`).
///
/// # Safety
/// `window` is the innermost open window on this thread, and every reader
/// of the deferred bodies must be done — for the reset that means after
/// `finish_reset` and after the last pass over its survivor list.
unsafe fn close(window: *mut ResetWindow) {
    debug_assert_eq!(
        WINDOW.with(|cell| cell.get()),
        window,
        "a reset closed a window that is not the innermost"
    );
    let prev = unsafe { (*window).prev };
    WINDOW.with(|cell| cell.set(prev));
    unsafe { release_log(window) };
    if !prev.is_null() {
        return;
    }

    // The head is taken before the first pop, so what the pops re-enter
    // finds the stack empty and the window closed; each link is read before
    // its body is handed over, because the return overwrites the word.
    let mut body = DEFERRED_FREES.with(|cell| cell.replace(std::ptr::null_mut()));
    while !body.is_null() {
        let next = unsafe { deferred_link(body).read() };
        // The deferral was a refusal inside `ll_free`, which had already
        // taken the slot, so a plain free here reads as a repeat and is
        // refused.
        unsafe { crate::memory::stdapi::hand_back_and_free(body) };
        body = next;
    }
}

/// The word a deferred body names the next one through: the eight bytes a
/// free slot links through (`crate::memory::heap::FREE_LIST_LINK_OFFSET`),
/// which hold nothing while the body is dead and which the return
/// overwrites. The trace window's stack of withheld returns threads through
/// the same word of the same kinds, and the two never hold one body at
/// once: the deferral stands ahead of the withholding in `ll_free`, and the
/// deferred free re-enters that entry point only after this stack has let
/// the body go.
///
/// Plain rather than atomic: the stack has one writer and one reader, the
/// thread whose window is open, and no other thread reads these bytes until
/// it receives the return.
///
/// For an object this word is where the class pointer stood, so a pass
/// that walked a deferred body would follow the link as a `*const Class`.
/// What keeps every pass off it is the skip [`is_torn_down`] decides, and a
/// build without that skip faults rather than over-counts.
///
/// # Safety
/// `body` is a dead large entity, which is at least the free list's two
/// words.
#[inline]
unsafe fn deferred_link(body: *mut u8) -> *mut *mut u8 {
    unsafe { body.add(crate::memory::heap::FREE_LIST_LINK_OFFSET) as *mut *mut u8 }
}

/// Defer a large entity's free if a reset is in flight on this thread.
/// **True** when it was deferred and the caller owes it nothing further.
///
/// # Safety
/// `body` is a just-freed large-entity body, owned by this call, whose
/// slot `ll_free` has taken.
pub(crate) unsafe fn defer_free(body: *mut u8) -> bool {
    if WINDOW.with(|cell| cell.get()).is_null() {
        return false;
    }

    let head = DEFERRED_FREES.with(|cell| cell.get());
    unsafe { deferred_link(body).write(head) };
    DEFERRED_FREES.with(|cell| cell.set(body));
    true
}

/// Count a slot `ll_free` has just taken while a reset is open on this
/// thread, which is a teardown the reset's passes will read as completed.
/// Called at the head of `ll_free`, past the take, in test builds alone.
/// Every entity death on the thread counts, a heap temporary a destructor
/// drops as much as a survivor's, so a test reads the figure as "at least
/// one" and never as an exact count.
#[cfg(test)]
pub(crate) fn note_slot_taken() {
    if !WINDOW.with(|cell| cell.get()).is_null() {
        TEARDOWNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether this address is an entity whose teardown completed inside a
/// reset still open on this thread.
///
/// One reading of the header: [`crate::refcount::SlotState::DeadInPlace`],
/// the bit `ll_free`'s head takes before any arm, for every kind a survivor
/// can be — a retained-block occupant and both large kinds — and the record
/// of every completed teardown, each of the four teardown bodies freeing a
/// GcHeap entity through that entry point and a request-arena entity never
/// being torn down at all, its count reaching zero without a teardown
/// (`refcount::release_word`). A resurrection frees nothing and carries no
/// bit. The count alone would not do — `promote::mark_child` zeroes a live
/// survivor's count during the fixpoint — and the bit is written by the
/// death and by nothing else: nothing hands a survivor's slot back while a
/// window is open, and no survivor's slot is reissued while one is, a
/// retained block having no free list and a large body being deferred
/// (`dev/DECISIONS.md`, "the record of a torn-down entity is its own header
/// bit").
///
/// # Safety
/// `entity` addresses a published or torn-down entity header, readable at
/// its first eight bytes.
pub(crate) unsafe fn is_torn_down(entity: *mut RcHeader) -> bool {
    let state = unsafe { crate::refcount::slot_state(entity) };
    state == crate::refcount::SlotState::DeadInPlace
}

/// Slots taken inside a window and torn-down entities walked since a test
/// last cleared them ([`take_counters`]). The re-trace walks no torn-down
/// entity, and nothing about that is visible in a count or in an ordinary
/// run — the memory a torn-down entity leaves behind is readable, and its
/// stale slots name no unmarked arena child. So a test reads these instead
/// of the memory the walk would have touched.
///
/// Plain statics rather than thread-locals: every test that runs a reset
/// holds `block_pool::test_guard`, which serializes the suite, so no
/// unguarded thread writes these behind a reader.
#[cfg(test)]
static TEARDOWNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TORN_DOWN_WALKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Count `entity` if a pass is about to walk it after its teardown
/// completed. Called by the re-trace past its own skip, so removing that
/// skip is what makes the count non-zero.
///
/// # Safety
/// As [`is_torn_down`].
#[cfg(test)]
pub(crate) unsafe fn note_walk(entity: *mut RcHeader) {
    if unsafe { is_torn_down(entity) } {
        TORN_DOWN_WALKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The two counters, cleared by the read: slots taken inside a window, and
/// torn-down entities walked by a pass that should have skipped them. A
/// test reads the first to know its own shape happened at all.
#[cfg(test)]
pub(crate) fn take_counters() -> (usize, usize) {
    use std::sync::atomic::Ordering::Relaxed;
    (TEARDOWNS.swap(0, Relaxed), TORN_DOWN_WALKS.swap(0, Relaxed))
}

/// One entry of a window's log, in two words: what the record is, and the
/// COW child it is about.
///
/// The first word carries all three kinds. Null is a compensating retain
/// the counting pass gave an already-promoted child; an address is the
/// survivor that held `child` at the instant of its promotion; and
/// [`CAPTURE_BIT`] set is that survivor's own count at that instant, in the
/// bits above the tag. An entity address is eight-aligned, so the bit
/// belongs to neither of the other two readings.
///
/// An edge and a decrement are recorded for every COW child that reaches
/// the recorder, including one this reset never promoted. What narrows them
/// to the entities the arithmetic is about is the capture:
/// `promote::reconcile_cow_counts` walks the captures and asks corrections
/// of their children alone (`dev/DECISIONS.md`, "the COW count is the log's
/// edges plus the delta").
#[repr(C)]
#[derive(Clone, Copy)]
struct Record {
    kind_word: usize,
    child: *mut RcHeader,
}

/// What in [`Record::kind_word`] says the record is a capture rather than a
/// holder or a null: the count it carries stands above this bit.
const CAPTURE_BIT: usize = 1;

const _: () = assert!(
    size_of::<usize>() > size_of::<u32>(),
    "a capture carries a whole refcount above the tag bit"
);

impl Record {
    /// An edge `holder` held to the COW `child` when `holder` was counted.
    fn promotion_edge(holder: *mut RcHeader, child: *mut RcHeader) -> Record {
        debug_assert!(
            !holder.is_null(),
            "a holder is what tells an edge from a decrement"
        );
        debug_assert!(
            holder as usize & CAPTURE_BIT == 0,
            "an entity address is eight-aligned, which is what leaves the tag bit free"
        );
        Record {
            kind_word: holder as usize,
            child,
        }
    }

    /// A compensating retain `child` was given and the reconciliation owes
    /// back.
    fn deferred_decrement(child: *mut RcHeader) -> Record {
        Record {
            kind_word: 0,
            child,
        }
    }

    /// `child`'s count `at` the instant of its promotion, which is what
    /// makes it one of this reset's own COW survivors.
    fn capture(child: *mut RcHeader, at: u32) -> Record {
        Record {
            kind_word: (at as usize) << 1 | CAPTURE_BIT,
            child,
        }
    }

    /// The correction this record owes the child's count, and **`None` for
    /// a capture**, which owes none: it states the count the correction
    /// terms are applied to.
    fn correction(self) -> Option<Correction> {
        if self.kind_word & CAPTURE_BIT != 0 {
            return None;
        }

        Some(if self.kind_word == 0 {
            Correction::DeferredDecrement
        } else {
            Correction::DeferredIncrement
        })
    }

    /// The count captured at the child's promotion, and **`None`** for the
    /// two correction kinds.
    fn captured_count(self) -> Option<u32> {
        if self.kind_word & CAPTURE_BIT == 0 {
            return None;
        }

        Some((self.kind_word >> 1) as u32)
    }
}

/// The bytes one log segment takes from `ll_alloc`: a size class of the
/// thread's heap, below `heap::MAX_SMALL`, so a segment costs the reset no
/// arena byte — the arena's last bytes are the survivor lists' — and no
/// block of its own.
const SEGMENT_BYTES: usize = 4096;

/// How many records a segment holds after its header.
const RECORDS_PER_SEGMENT: usize =
    (SEGMENT_BYTES - size_of::<SegmentHeader>()) / size_of::<Record>();

#[repr(C)]
struct SegmentHeader {
    /// The segment recorded before this one, or null.
    next: *mut Segment,
    /// Records written, from the front of `records`.
    len: usize,
    /// The lowest and the highest child address the segment holds, written
    /// by [`order_log_by_child`], which also puts the chain in ascending
    /// order of the first of them. That order is what lets the search stop:
    /// past the first segment whose lowest child is above the address, no
    /// segment on the chain can hold it. A reset records a child's edge in
    /// the round that meets it, so a segment's records are a slice of the
    /// descent and their addresses a slice of the arena's bump, which is
    /// what makes the ranges disjoint enough for the stop to pay
    /// (`dev/BENCHMARKS.md`, 2026-09-13, the reconciliation).
    ///
    /// An empty range — the first above the second — is a segment no search
    /// enters, which is what an unordered or empty log reads as.
    lowest_child: usize,
    highest_child: usize,
}

#[repr(C)]
struct Segment {
    header: SegmentHeader,
    records: [Record; RECORDS_PER_SEGMENT],
}

const _: () = assert!(size_of::<Segment>() <= SEGMENT_BYTES);
const _: () = assert!(SEGMENT_BYTES <= crate::memory::heap::MAX_SMALL);

/// Append `record` to the innermost window's log. **False** when the log
/// needed a segment and the manager refused it; the record is then not
/// kept, and the caller answers the refusal (`promote::count_children`).
fn append(record: Record) -> bool {
    let window = WINDOW.with(|cell| cell.get());
    debug_assert!(!window.is_null(), "a record outside a window");
    // The append lands past the ordered run, so the order the searches
    // stand on is stale until it is taken again.
    unsafe { (*window).log_ordered = false };
    let mut segment = unsafe { (*window).log };
    if segment.is_null() || unsafe { (*segment).header.len } == RECORDS_PER_SEGMENT {
        let fresh = unsafe { draw_segment() };
        if fresh.is_null() {
            return false;
        }

        unsafe {
            (*fresh).header.next = segment;
            (*fresh).header.len = 0;
            // An empty range, so a search that meets an unordered segment
            // walks past it rather than into its uninitialised header.
            (*fresh).header.lowest_child = usize::MAX;
            (*fresh).header.highest_child = 0;
            (*window).log = fresh;
        }
        segment = fresh;
    }

    // Through the raw array pointer rather than a slice method: the tail
    // past `len` is uninitialised, and a slice over the whole array would
    // be a reference to it.
    unsafe {
        let len = (*segment).header.len;
        (&raw mut (*segment).records)
            .cast::<Record>()
            .add(len)
            .write(record);
        (*segment).header.len = len + 1;
    }
    true
}

/// Append a correction, or answer false as a refused segment would. The two
/// correction kinds are refused together so that a test can hold them still
/// while a capture lands (`RefusedRecords`, which only a test build has).
fn append_correction(record: Record) -> bool {
    #[cfg(test)]
    if refusing(Refusing::Corrections) {
        return false;
    }

    append(record)
}

/// One segment from the thread's heap, or null on a refusal. Uninitialised
/// past its header, which [`append`] writes before the first record.
///
/// # Safety
/// The thread has a heap or can build one, which is `ll_alloc`'s own
/// contract.
unsafe fn draw_segment() -> *mut Segment {
    #[cfg(test)]
    if REFUSING.with(|cell| cell.get()) == Some(Refusing::Everything) {
        return std::ptr::null_mut();
    }

    unsafe { crate::memory::stdapi::ll_alloc(SEGMENT_BYTES, align_of::<Segment>()) as *mut Segment }
}

/// Give a window's segments back, newest first.
///
/// # Safety
/// `window` is open and its log is read by nobody any more.
unsafe fn release_log(window: *mut ResetWindow) {
    let mut segment = unsafe { std::mem::replace(&mut (*window).log, std::ptr::null_mut()) };
    while !segment.is_null() {
        let next = unsafe { (*segment).header.next };
        unsafe { crate::memory::stdapi::ll_free(segment as *mut u8) };
        segment = next;
    }
}

/// Record that `holder` held `child`, a COW entity, at the instant of its
/// promotion. The caller is the counting pass, which is where that instant
/// is. A record the manager refuses is answered through one channel only:
/// [`take_refused_promotion_edge`] answers true once for the round, and the
/// reset retains every COW child of the round's survivors once their counts
/// are captured, so the refused edge is counted by that retain instead
/// (`promote::count_children`; `dev/DECISIONS.md`, "the COW count is the
/// log's edges plus the delta"). Nothing is answered here, because a caller
/// answering a refusal at the edge would retain the child ahead of its
/// count's capture, and the capture discards it.
pub(crate) fn record_promotion_edge(holder: *mut RcHeader, child: *mut RcHeader) {
    let window = WINDOW.with(|cell| cell.get());
    if window.is_null() || append_correction(Record::promotion_edge(holder, child)) {
        return;
    }

    #[cfg(test)]
    REFUSED_RECORDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    unsafe { (*window).refused_promotion_edge = true };
}

/// Whether a promotion-edge record was refused since this was last asked,
/// cleared by the read. The reset asks it once per round, after the round's
/// survivors have their counts captured and their categories rewritten.
pub(crate) fn take_refused_promotion_edge() -> bool {
    let window = WINDOW.with(|cell| cell.get());
    if window.is_null() {
        return false;
    }

    unsafe { std::mem::replace(&mut (*window).refused_promotion_edge, false) }
}

/// Record a compensating retain the counting pass gave an already promoted
/// COW child, which the reconciliation takes back as a deferred decrement.
/// A record the manager refuses needs no answer: the retain stands
/// uncredited and the child settles one reference too high, which is a
/// bounded leak and never an under-count.
pub(crate) fn record_deferred_decrement(child: *mut RcHeader) {
    if WINDOW.with(|cell| cell.get()).is_null()
        || append_correction(Record::deferred_decrement(child))
    {
        return;
    }

    #[cfg(test)]
    REFUSED_RECORDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Record `child`'s count `at` the instant the reset promoted it, which is
/// the last instant the reset can attribute that count to arena holders.
/// The capture is also what says `child` is one of this reset's own COW
/// survivors: the reconciliation settles the children it finds a capture
/// for and no others.
///
/// **False when the manager refused the segment**, and the caller answers
/// that by counting and reporting it — there is no compensation to run.
/// A survivor the reconciliation never reaches keeps the references its
/// arena holders held, `now` being `reconciled + pre`, which is a bounded
/// leak of the class the deferred decrement's own refusal already carries
/// (`dev/plans/S47.md`, the Critic round over S47.7's design). The refusal
/// is kept off [`take_refused_promotion_edge`]'s channel deliberately: that
/// flag is read before the promoting loop and compensated after it, so a
/// refusal raised inside the loop would retain the next round's population.
///
/// True outside a window as well, where no reconciliation will read it.
pub(crate) fn record_cow_capture(child: *mut RcHeader, at: u32) -> bool {
    #[cfg(test)]
    if refusing(Refusing::Captures) {
        REFUSED_RECORDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return false;
    }

    if WINDOW.with(|cell| cell.get()).is_null() || append(Record::capture(child, at)) {
        return true;
    }

    #[cfg(test)]
    REFUSED_RECORDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    false
}

/// A correction term of `promote::reconcile_cow_counts`, named rather than
/// signed: the two carry opposite signs at the call site, and a boolean
/// would let a swapped arm compile and turn every +1 into a -1. A capture
/// is neither term and is answered by [`LogByChild::for_each_capture`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Correction {
    /// An edge a survivor of this reset held at its promotion: the count
    /// captured then is discarded whole, so the edge is counted here, and
    /// the release it may have earned since, by the holder's teardown or
    /// by a store into its slot, is inside the child's delta. **+1.**
    DeferredIncrement,
    /// A compensating retain the counting pass gave an already-promoted
    /// COW child: it lands in the child's delta while the edge behind it is
    /// in the log as well. **-1.**
    DeferredDecrement,
}

/// The innermost window's log, ordered for reading by the child each record
/// names, which is what lets [`corrections_for`](LogByChild::corrections_for)
/// find a child's records by search rather than by walking every record of
/// every segment. Empty outside a reset.
pub(crate) struct LogByChild {
    /// The window whose log is read, or null outside a reset. The window
    /// rather than its head: a search stands on an order an append makes
    /// stale, and the window is where that is recorded.
    window: *mut ResetWindow,
}

/// Order the log for reading and answer the handle that reads it.
///
/// **The last record must already be written.** An append lands past the
/// ordered run and so makes the order stale, which a debug build reports at
/// the next read rather than at the append; ordering again is what a caller
/// with more records to write does.
///
/// The segments keep their contents; what moves is the order of the records
/// inside each and the order of the segments themselves, neither of which a
/// reader before this one depends on. The chain stops being newest-first,
/// so an append after it may draw a segment while another still has room.
pub(crate) fn order_log_by_child() -> LogByChild {
    let window = WINDOW.with(|cell| cell.get());
    if window.is_null() {
        return LogByChild {
            window: std::ptr::null_mut(),
        };
    }

    let mut segment = unsafe { (*window).log };
    while !segment.is_null() {
        unsafe {
            let records = records_of(segment);
            records.sort_unstable_by_key(|record| record.child as usize);
            let (lowest, highest) = match (records.first(), records.last()) {
                (Some(first), Some(last)) => (first.child as usize, last.child as usize),
                _ => (usize::MAX, 0),
            };
            (*segment).header.lowest_child = lowest;
            (*segment).header.highest_child = highest;
            segment = (*segment).header.next;
        }
    }

    // The chain in ascending order of the ranges just written, by insertion:
    // a reset fills tens of segments, so the square is tens of pointer
    // writes, and it buys the search its stopping point.
    let mut ordered: *mut Segment = std::ptr::null_mut();
    let mut segment = unsafe { (*window).log };
    while !segment.is_null() {
        let next = unsafe { (*segment).header.next };
        let mut behind = &raw mut ordered;
        unsafe {
            while !(*behind).is_null()
                && (**behind).header.lowest_child <= (*segment).header.lowest_child
            {
                behind = &raw mut (**behind).header.next;
            }

            (*segment).header.next = *behind;
            *behind = segment;
        }

        segment = next;
    }

    unsafe {
        (*window).log = ordered;
        (*window).log_ordered = true;
    }

    LogByChild { window }
}

/// The records a segment holds, which is the front `len` of its array and
/// never the uninitialised tail past it.
///
/// # Safety
/// `segment` belongs to an open window's log, and no other reference into
/// its records is live.
unsafe fn records_of<'a>(segment: *mut Segment) -> &'a mut [Record] {
    unsafe {
        std::slice::from_raw_parts_mut(
            (&raw mut (*segment).records).cast::<Record>(),
            (*segment).header.len,
        )
    }
}

/// Where `child`'s run begins in `segment`: the first record naming an
/// address at or past it, or `len` when the segment holds none.
///
/// # Safety
/// `segment` belongs to a log [`order_log_by_child`] has ordered.
unsafe fn first_record_of(segment: *mut Segment, child: usize) -> usize {
    let base = unsafe { (&raw const (*segment).records).cast::<Record>() };
    let (mut low, mut high) = (0, unsafe { (*segment).header.len });
    while low < high {
        let middle = low + (high - low) / 2;
        if (unsafe { base.add(middle).read() }.child as usize) < child {
            low = middle + 1;
        } else {
            high = middle;
        }
    }

    low
}

impl LogByChild {
    /// The newest segment of the log this handle reads, or null when it was
    /// taken outside a reset.
    fn head(&self) -> *mut Segment {
        if self.window.is_null() {
            return std::ptr::null_mut();
        }

        debug_assert!(
            unsafe { (*self.window).log_ordered },
            "a record was appended since the log was ordered, so the searches below it are stale"
        );
        unsafe { (*self.window).log }
    }

    /// Every COW survivor this reset promoted, once each, with the count it
    /// carried at that instant. The order is the log's own.
    pub(crate) fn for_each_capture(&self, mut f: impl FnMut(*mut RcHeader, u32)) {
        let mut segment = self.head();
        while !segment.is_null() {
            let len = unsafe { (*segment).header.len };
            let base = unsafe { (&raw const (*segment).records).cast::<Record>() };
            for index in 0..len {
                let record = unsafe { base.add(index).read() };
                if let Some(at) = record.captured_count() {
                    f(record.child, at);
                }
            }

            segment = unsafe { (*segment).header.next };
        }
    }

    /// What this reset owes `child`'s count beyond its delta, one call of
    /// `f` per correction. **Every segment is searched**, because a child's
    /// records are spread over as many segments as the reset filled, and a
    /// membership read from one of them drops the rest.
    ///
    /// A holder's fate is not read: an edge is one increment whether the
    /// holder stands, was torn down, or let the edge go.
    ///
    /// **One capture per child, which this is the cheap place to check.**
    /// The reconciliation settles a survivor by reading its count and
    /// storing the sum back, so a second visit to one capture reads what
    /// the first stored: `at = 3` with one edge settles to 1, and the
    /// second pass to `1 - 3 + 1`, clamped to zero — a promoted holder left
    /// naming an entity the next release frees. The walk below reads the
    /// child's whole run anyway.
    pub(crate) fn corrections_for(&self, child: *mut RcHeader, mut f: impl FnMut(Correction)) {
        let mut captures = 0;
        let mut segment = self.head();
        while !segment.is_null() {
            let len = unsafe { (*segment).header.len };
            let base = unsafe { (&raw const (*segment).records).cast::<Record>() };
            let header = unsafe { &raw const (*segment).header };
            // The chain ascends by lowest child, so a segment that begins
            // past the address ends the walk rather than skipping one.
            if (child as usize) < unsafe { (*header).lowest_child } {
                break;
            }

            if (child as usize) > unsafe { (*header).highest_child } {
                segment = unsafe { (*segment).header.next };
                continue;
            }

            let mut index = unsafe { first_record_of(segment, child as usize) };
            while index < len {
                let record = unsafe { base.add(index).read() };
                if record.child != child {
                    break;
                }

                match record.correction() {
                    Some(correction) => f(correction),
                    None => captures += 1,
                }

                index += 1;
            }

            segment = unsafe { (*segment).header.next };
        }

        debug_assert!(
            captures <= 1,
            "a child captured twice is settled twice, and the second sum is the first one's answer"
        );
    }
}

/// Whether a reset is in flight on this thread.
///
/// Read by `cycle::collect` before it opens a collection, and by tests that
/// ask the question instead of reading the thread-local themselves: a reset
/// runs user destructors over a heap whose promoted survivors are stamped and
/// not yet listed, which is not a heap a trace may read
/// (`memory::retained::register`).
pub(crate) fn is_open() -> bool {
    !WINDOW.with(|cell| cell.get()).is_null()
}

/// How many resets are in flight on this thread, counting outwards along
/// the chain. A test of the nesting reads it from inside a destructor:
/// without it, a death that happened beside the inner reset rather than
/// inside it produces the same counts and the test proves nothing.
#[cfg(test)]
pub(crate) fn depth() -> usize {
    let mut window = WINDOW.with(|cell| cell.get());
    let mut depth = 0;
    while !window.is_null() {
        depth += 1;
        window = unsafe { (*window).prev };
    }

    depth
}

/// Records the manager refused since a test last cleared the figure.
#[cfg(test)]
static REFUSED_RECORDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Refused records, cleared by the read.
#[cfg(test)]
pub(crate) fn take_refused_records() -> usize {
    REFUSED_RECORDS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// What the log refuses while a [`RefusedRecords`] guard stands, or
    /// `None` when it takes everything it is given.
    static REFUSING: Cell<Option<Refusing>> = const { Cell::new(None) };
}

/// What a [`RefusedRecords`] guard refuses.
///
/// A refused segment refuses whatever record meets it next and every one
/// after it, so which arm a test reaches is decided by the record the reset
/// happened to be writing. The two narrow settings hold one arm still while
/// the other is read: within one reset the manager's answer does change,
/// the drain giving memory back between the counting pass and the promoting
/// loop.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusing {
    /// Every record, refused at the segment draw itself.
    Everything,
    /// The promotion edges and the deferred decrements, the captures
    /// landing.
    Corrections,
    /// The captures, the corrections landing.
    Captures,
}

/// Refuse what `Refusing` names on this thread for the guard's life, which
/// is how a test reaches a refusal arm without emptying the thread's heap.
#[cfg(test)]
pub(crate) struct RefusedRecords(());

#[cfg(test)]
impl RefusedRecords {
    pub(crate) fn arm(what: Refusing) -> Self {
        REFUSING.with(|cell| cell.set(Some(what)));
        RefusedRecords(())
    }
}

#[cfg(test)]
impl Drop for RefusedRecords {
    fn drop(&mut self) {
        REFUSING.with(|cell| cell.set(None));
    }
}

/// Whether the log turns a record of this kind away at the recorder itself.
/// **`Everything` is not one of these**: it refuses the segment draw
/// instead, which is the refusal the manager actually makes, and every
/// record then meets it through [`append`].
#[cfg(test)]
fn refusing(kind: Refusing) -> bool {
    REFUSING.with(|cell| cell.get()) == Some(kind)
}

/// Whether a free of an occupant of retained `block` is this reset's own
/// torn-down entity, whose death the reset accounts for by not counting it.
/// **True** means the caller drops the free entirely.
///
/// False outside a reset, and false for a block that counts a held occupant:
/// that count either belongs to an earlier reset's live occupant or to a dead
/// candidate awaiting retirement, and a later free is the event that will
/// eventually return the block. The count and not the list is what is asked, so a
/// block whose reset could place no list still counts its deaths
/// (`memory::retained::has_held_occupants`).
///
/// # Safety
/// `block` is the header of a mapped block stamped `BLOCK_KIND_RETAINED`.
pub(crate) unsafe fn absorbs_retained_free(block: usize) -> bool {
    if WINDOW.with(|cell| cell.get()).is_null() {
        return false;
    }

    !unsafe { crate::memory::retained::has_held_occupants(block) }
}

#[cfg(test)]
mod tests;
