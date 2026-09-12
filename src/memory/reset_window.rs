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
//! reset — the log of promotion-time COW edges and compensating retains the
//! reconciliation reads ([`Record`]) — is drawn through `stdapi::ll_alloc`
//! in segments, and a segment the manager refuses is answered to the
//! recorder as a refusal ([`record_promotion_edge`]).
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
    /// The window this one displaced. A destructor run by one reset can
    /// resolve another arena and reset it, so the windows nest and each
    /// close restores its predecessor.
    prev: *mut ResetWindow,
}

impl ResetWindow {
    /// A window that is not open: what a reset declares before [`open`].
    pub(crate) const fn closed() -> Self {
        ResetWindow {
            log: std::ptr::null_mut(),
            refused_promotion_edge: false,
            prev: std::ptr::null_mut(),
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

/// Open a window for a reset about to run on this thread, over storage the
/// reset declares in its own frame; closed when the guard leaves scope.
pub(crate) fn open(window: &mut ResetWindow) -> Guard<'_> {
    window.log = std::ptr::null_mut();
    window.refused_promotion_edge = false;
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
/// bit. The count alone would not do — `promote::mark_one` zeroes a live
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

/// One entry of a window's log: a promotion-time COW edge when `holder` is
/// the survivor that held `child`, and a compensating retain the counting
/// pass gave `child` when `holder` is null.
///
/// Both are recorded for every COW child that reaches the recorder,
/// including one this reset never promoted; what narrows them to the
/// entities the arithmetic is about is the consumer,
/// `promote::reconcile_cow_counts`, which drops a correction naming no row
/// of its own (`dev/DECISIONS.md`, "the COW count is the log's edges plus
/// the delta").
#[repr(C)]
#[derive(Clone, Copy)]
struct Record {
    holder: *mut RcHeader,
    child: *mut RcHeader,
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
    let mut segment = unsafe { (*window).log };
    if segment.is_null() || unsafe { (*segment).header.len } == RECORDS_PER_SEGMENT {
        let fresh = unsafe { draw_segment() };
        if fresh.is_null() {
            return false;
        }

        unsafe {
            (*fresh).header.next = segment;
            (*fresh).header.len = 0;
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

/// One segment from the thread's heap, or null on a refusal. Uninitialised
/// past its header, which [`append`] writes before the first record.
///
/// # Safety
/// The thread has a heap or can build one, which is `ll_alloc`'s own
/// contract.
unsafe fn draw_segment() -> *mut Segment {
    #[cfg(test)]
    if REFUSE_SEGMENTS.with(|cell| cell.get()) {
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
    if window.is_null() || append(Record { holder, child }) {
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
        || append(Record {
            holder: std::ptr::null_mut(),
            child,
        })
    {
        return;
    }

    #[cfg(test)]
    REFUSED_RECORDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A correction term of `promote::reconcile_cow_counts`, named rather than
/// signed: the two carry opposite signs at the call site, and a boolean
/// would let a swapped arm compile and turn every +1 into a -1.
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

/// What this reset owes its COW survivors' counts beyond their deltas,
/// one call of `f` per correction, in no particular order. Nothing outside
/// a reset. A holder's fate is not read: an edge is one increment whether
/// the holder stands, was torn down, or let the edge go.
pub(crate) fn for_each_correction(mut f: impl FnMut(*mut RcHeader, Correction)) {
    let window = WINDOW.with(|cell| cell.get());
    if window.is_null() {
        return;
    }

    let mut segment = unsafe { (*window).log };
    while !segment.is_null() {
        let len = unsafe { (*segment).header.len };
        for index in 0..len {
            let record = unsafe {
                (&raw const (*segment).records)
                    .cast::<Record>()
                    .add(index)
                    .read()
            };
            if record.holder.is_null() {
                f(record.child, Correction::DeferredDecrement);
            } else {
                f(record.child, Correction::DeferredIncrement);
            }
        }

        segment = unsafe { (*segment).header.next };
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
    /// While set, every segment draw answers null, which is how a test
    /// reaches the refusal arm without emptying the thread's heap.
    static REFUSE_SEGMENTS: Cell<bool> = const { Cell::new(false) };
}

/// Refuse every segment draw on this thread for the guard's life.
#[cfg(test)]
pub(crate) struct RefusedSegments(());

#[cfg(test)]
impl RefusedSegments {
    pub(crate) fn arm() -> Self {
        REFUSE_SEGMENTS.with(|cell| cell.set(true));
        RefusedSegments(())
    }
}

#[cfg(test)]
impl Drop for RefusedSegments {
    fn drop(&mut self) {
        REFUSE_SEGMENTS.with(|cell| cell.set(false));
    }
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
