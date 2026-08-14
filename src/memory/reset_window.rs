//! The window an arena reset holds over its own frees.
//!
//! A reset collects survivors during its fixpoint and reads that list
//! again after it (`promote::arena_reset_full`). Between the two, the
//! release-log drain can kill a survivor — the shape is a heap `&` box
//! stored into an arena slot, whose logged release tears the promoted
//! entity down. The passes that follow must still be able to read one
//! word of every address they hold, which is how they tell a corpse from
//! a live entity (`retained::is_occupied`, and the skip in
//! `promote::reconcile_cow_counts`).
//!
//! For a survivor in a shared retained block that costs nothing: such a
//! block recycles nothing inside itself, so the corpse's header stays
//! where it is. For a survivor in a block of its own it is not free —
//! `large_entity::free` hands a 128 KiB-and-up run back to the system,
//! and the next reader of that address reads memory the process no longer
//! owns. So while the window is open those two kinds park here and are
//! freed after it closes.
//!
//! **This is the reset's window, not the collector's.** It is compiled in
//! both builds and turns on no GC state: `deferred_free` parks against an
//! epoch, this parks against a reset, and a build without epochs runs
//! this code unchanged. The flush re-enters `ll_free`, so a parked body
//! meets whatever holds it then: the epoch's own parking if one is in
//! flight, or — when a destructor of an outer reset drove this one — the
//! outer window, which frees it at its own close.
//!
//! The window also **absorbs** one free rather than deferring it: a
//! corpse in a retained block this reset has not registered yet. Its
//! death is already accounted for, because `retained::register` declines
//! to count an occupant whose header reads zero, and replaying that free
//! afterwards would take the block's live count below its true occupancy
//! and hand it to the pool under living survivors.
//!
//! A `Cell<*mut _>` rather than a `RefCell<Vec<_>>`, for
//! `deferred_free::PARKED`'s reason: a `Vec` in a thread-local registers
//! drop glue, and this path is reachable from thread exit, where TLS
//! destructor order is unspecified.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use crate::refcount::RcHeader;

/// One reset in flight on this thread.
struct ResetWindow {
    /// Bodies whose free would return memory to the system, held until
    /// the reset's last reader is done with them. Only the two
    /// large-entity kinds: every other kind either recycles nothing
    /// (arena, immortal) or leaves the freed bytes mapped where a
    /// header read still lands (a heap slot, a retained block).
    parked_large: Vec<*mut u8>,
    /// What each survivor of **this** reset held in COW children at the
    /// instant it was promoted, which is the instant the child's captured
    /// count still accounts for. A holder that dies pays its snapshot
    /// forward into [`ResetWindow::escrow`]; one that lives needs
    /// nothing, its edges being walked where they are
    /// (`dev/DECISIONS.md`, "the reset reads no corpse").
    snapshots: HashMap<usize, Vec<*mut RcHeader>>,
    /// One entry per edge a dead holder of this reset held at promotion:
    /// the release that edge earned is inside the child's delta, and
    /// nothing walks the holder any more, so the count is restored here.
    escrow: Vec<*mut RcHeader>,
    /// One entry per compensating retain `promote::count_children` handed
    /// an **already promoted** COW child in a later round. That retain is
    /// promote's own hand landing in the delta, while the edge behind it
    /// is walked as well, so the pair would count twice.
    credits: Vec<*mut RcHeader>,
    /// The window this one displaced. A destructor run by one reset can
    /// resolve another arena and reset it, so the windows nest and each
    /// close restores its predecessor.
    prev: *mut ResetWindow,
}

thread_local! {
    /// The innermost open window, or null.
    static WINDOW: Cell<*mut ResetWindow> = const { Cell::new(std::ptr::null_mut()) };
}

/// A window closed by leaving the scope that opened it, whatever ends
/// that scope. An unwind is reachable — `arena_reset_full` asserts on a
/// fixpoint that will not converge, and a `__destruct` body can panic —
/// and a window left open would park every later large free on the
/// thread into a list nobody drains and absorb every later retained free
/// into nothing. The guard lives on the stack, so it carries none of the
/// drop-glue problem the module doc argues against for the thread-local
/// itself.
pub(crate) struct Guard(());

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { close_and_flush() };
    }
}

/// Open a window for a reset about to run on this thread, closed when the
/// guard leaves scope.
#[must_use = "the window closes when the guard drops, so it must be held"]
pub(crate) fn opened() -> Guard {
    open();
    Guard(())
}

/// Open a window for a reset about to run on this thread.
fn open() {
    let prev = WINDOW.with(|cell| cell.get());
    let window = Box::into_raw(Box::new(ResetWindow {
        parked_large: Vec::new(),
        snapshots: HashMap::new(),
        escrow: Vec::new(),
        credits: Vec::new(),
        prev,
    }));

    WINDOW.with(|cell| cell.set(window));
}

/// Close the innermost window and free what it parked.
///
/// The frees run **after** the close, so each takes the ordinary route:
/// under an epoch in flight a body parks again in `deferred_free`, whose
/// walker may still hold the address, and otherwise it goes back now.
///
/// # Safety
/// A window must be open on this thread, and every reader of the parked
/// bodies must be done — for the reset that means after `finish_reset`
/// and after the last pass over its survivor list.
unsafe fn close_and_flush() {
    let window = WINDOW.with(|cell| cell.get());
    debug_assert!(!window.is_null(), "a reset closed a window it never opened");
    if window.is_null() {
        return;
    }

    let window = unsafe { Box::from_raw(window) };
    WINDOW.with(|cell| cell.set(window.prev));
    if !window.prev.is_null() {
        // A reset driven by a destructor of an outer one closes first,
        // and what it parked may be an **outer** survivor: the outer
        // reset still has passes to run over it, and its weak walk still
        // reads headers. So the bodies move outward and only the
        // outermost close frees anything.
        unsafe { (*window.prev).parked_large.extend(window.parked_large) };
        return;
    }

    for ptr in window.parked_large {
        unsafe { crate::memory::stdapi::ll_free(ptr) };
    }

    // The deaths are the whole chain's, so they go when the chain does.
    let died = DIED.with(|cell| cell.replace(std::ptr::null_mut()));
    if !died.is_null() {
        drop(unsafe { Box::from_raw(died) });
    }
}

thread_local! {
    /// Every entity whose teardown **completed** while a reset was open
    /// on this thread. Shared by the whole window chain: a survivor of an
    /// outer reset can die inside an inner one, and the outer reset's
    /// passes are the ones that must not walk it.
    static DIED: Cell<*mut HashSet<usize>> = const { Cell::new(std::ptr::null_mut()) };
}

fn died_set() -> *mut HashSet<usize> {
    DIED.with(|cell| {
        let mut set = cell.get();
        if set.is_null() {
            set = Box::into_raw(Box::new(HashSet::new()));
            cell.set(set);
        }

        set
    })
}

/// Record that `survivor` held `child`, a COW entity, at the instant of
/// its promotion. Taken by the counting pass, which runs after every
/// destructor of its round and before the category rewrite — the one
/// point where a survivor's edges are provably the ones its children's
/// captured counts were taken against.
pub(crate) fn snapshot_edge(survivor: *mut RcHeader, child: *mut RcHeader) {
    let window = WINDOW.with(|cell| cell.get());
    if window.is_null() {
        return;
    }

    unsafe {
        (*window)
            .snapshots
            .entry(survivor as usize)
            .or_default()
            .push(child)
    };
}

/// Record a compensating retain the counting pass gave an already
/// promoted COW child ([`ResetWindow::credits`]).
pub(crate) fn credit(child: *mut RcHeader) {
    let window = WINDOW.with(|cell| cell.get());
    if window.is_null() {
        return;
    }

    unsafe { (*window).credits.push(child) };
}

/// Record a completed teardown, and pay the entity's promotion-time
/// snapshot into the escrow of the reset that took it — which is not
/// necessarily the reset whose drain is running, a destructor of one
/// reset being able to drive another.
///
/// An entity no open reset promoted records the death and nothing else:
/// no pass of any open reset walks it.
pub(crate) fn record_death(entity: *mut RcHeader) {
    if WINDOW.with(|cell| cell.get()).is_null() {
        return;
    }

    unsafe { (*died_set()).insert(entity as usize) };
    #[cfg(test)]
    DEATHS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let mut window = WINDOW.with(|cell| cell.get());
    while !window.is_null() {
        if let Some(edges) = unsafe { (*window).snapshots.remove(&(entity as usize)) } {
            unsafe { (*window).escrow.extend(edges) };
            return;
        }

        window = unsafe { (*window).prev };
    }
}

/// Deaths recorded and corpses walked since a test last cleared them
/// ([`take_counters`]). The passes after the fixpoint walk no corpse, and
/// nothing about that is visible in a count or in an ordinary run — the
/// memory a corpse leaves behind is readable, so the walk finds a stale
/// edge whose release is already in the delta and the two cancel. So a
/// test reads these instead of the memory the walk would have touched.
///
/// Plain statics rather than thread-locals: every test that reads them
/// holds `block_pool::test_guard`, which serializes the suite.
#[cfg(test)]
static DEATHS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static CORPSE_WALKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Count `entity` if a pass is about to walk it after its teardown
/// completed. Called by each post-fixpoint pass past its own skip, so
/// removing that skip is what makes the count non-zero.
#[cfg(test)]
pub(crate) fn note_walk(entity: *mut RcHeader) {
    if has_died(entity) {
        CORPSE_WALKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The two counters, cleared by the read: deaths recorded, and corpses
/// walked by a pass that should have skipped them. A test reads the first
/// to know its own shape happened at all.
#[cfg(test)]
pub(crate) fn take_counters() -> (usize, usize) {
    use std::sync::atomic::Ordering::Relaxed;
    (DEATHS.swap(0, Relaxed), CORPSE_WALKS.swap(0, Relaxed))
}

/// Whether this address is one whose teardown completed inside a reset
/// still open on this thread.
pub(crate) fn has_died(entity: *mut RcHeader) -> bool {
    let died = DIED.with(|cell| cell.get());
    !died.is_null() && unsafe { (*died).contains(&(entity as usize)) }
}

/// The escrowed edges of this reset's corpses, and the credits its
/// counting pass handed out — the two correction terms of
/// `promote::reconcile_cow_counts`.
pub(crate) fn corrections() -> (Vec<*mut RcHeader>, Vec<*mut RcHeader>) {
    let window = WINDOW.with(|cell| cell.get());
    if window.is_null() {
        return (Vec::new(), Vec::new());
    }

    unsafe { ((*window).escrow.clone(), (*window).credits.clone()) }
}

/// Park a large entity's body if a reset is in flight on this thread.
/// **True** when it was parked and the caller owes it nothing further.
///
/// # Safety
/// `ptr` is a just-freed large-entity body, owned by this call.
pub(crate) unsafe fn park_large(ptr: *mut u8) -> bool {
    let window = WINDOW.with(|cell| cell.get());
    if window.is_null() {
        return false;
    }

    unsafe { (*window).parked_large.push(ptr) };
    true
}

/// Whether a reset is in flight on this thread — what a test asks
/// instead of reading the thread-local itself.
#[cfg(test)]
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

/// Whether a free of an occupant of retained `block` is this reset's own
/// corpse, whose death the reset accounts for by not counting it.
/// **True** means the caller drops the free entirely.
///
/// False outside a reset, and false for a block already registered: that
/// index belongs to an earlier reset, which counted this occupant as
/// live, and its death is the event that will eventually return the
/// block.
pub(crate) fn absorbs_retained_free(block: usize) -> bool {
    if WINDOW.with(|cell| cell.get()).is_null() {
        return false;
    }

    !crate::memory::retained::is_registered(block)
}

#[cfg(test)]
mod tests;
