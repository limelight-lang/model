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

/// One reset in flight on this thread.
struct ResetWindow {
    /// Bodies whose free would return memory to the system, held until
    /// the reset's last reader is done with them. Only the two
    /// large-entity kinds: every other kind either recycles nothing
    /// (arena, immortal) or leaves the freed bytes mapped where a
    /// header read still lands (a heap slot, a retained block).
    parked_large: Vec<*mut u8>,
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
    for ptr in window.parked_large {
        unsafe { crate::memory::stdapi::ll_free(ptr) };
    }
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
