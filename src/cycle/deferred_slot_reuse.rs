//! The trace window over physical entity-slot return.
//!
//! A trace's shadow rows are indexed by slot. If an entity dies after its row
//! has been met and the allocator reuses that slot before the trace finishes,
//! the new occupant inherits the dead occupant's row: visited state, working
//! count and verdict all name the address rather than an allocation identity.
//! Observable teardown still happens at refcount zero; only the slot's return
//! to its heap waits here (`rfc/model/gc/rc-cycle.md`, "Zero-count entities
//! pending slot reuse").
//!
//! There are two independent reasons a dead slot may still be named:
//!
//! - a candidate-queue entry, represented by
//!   [`crate::refcount::CANDIDATE_BIT`];
//! - this window, represented by [`TRACE_ACTIVE`], while mark or scan may
//!   still use a shadow row for the slot.
//!
//! Every attempted return goes through `memory::stdapi::ll_free`. That door
//! first refuses the queue window and then calls [`defer_reuse_if_tracing`] for
//! this one. Closing a trace replays its withheld returns through the same
//! door, so an entry still standing keeps the slot withheld without a second
//! record. Conversely, retiring an entry while the trace still runs reaches
//! this list. The two windows can therefore close in either order.
//!
//! The list is a raw pointer in a `Cell`, not a `RefCell<Vec<_>>`. It has no
//! TLS drop glue: thread-exit order is owned explicitly by
//! `memory::heap::ll_thread_exit`, and a runtime structure first touched by a
//! destructor may not depend on the platform's TLS destructor order
//! (`dev/DECISIONS.md`, "thread exit owns the order its per-thread state dies
//! in").
//!
//! # What it allocates, and what a refusal would cost
//!
//! One allocation per thread, and it is the one this stage does not fix: the
//! list is a `Box<Vec<*mut u8>>` out of the global allocator, drawn at the
//! first deferred return and freed at [`dispose_thread_state`]. A `Vec` that
//! cannot grow aborts the process rather than answering, which puts this list
//! in the crate's fatal class beside the queue's base-block draw and its
//! overflow-capacity bound (`crate::cycle::queue`) — the difference being that
//! those two are funded on purpose and this one is an allocator call left
//! over. Replacing it with
//! manager-owned memory is a structural change and belongs to the step that
//! owns it, not to a rename (`PLAN.md`, S41, and
//! `dev/CYCLE-TERMINOLOGY-AUDIT.md`, "Deferred slot reuse").

use std::cell::Cell;

thread_local! {
    /// True on the owning mutator while its in-line trace may still address
    /// shadow rows. S38 moves this state to the per-owner trace token before a
    /// worker can trace one thread from another.
    static TRACE_ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// Returns attempted while [`TRACE_ACTIVE`] was true, in attempt order.
    static DEFERRED_SLOTS: Cell<*mut Vec<*mut u8>> = const { Cell::new(std::ptr::null_mut()) };
}

/// An open in-line trace and the arena whose rows it protects.
///
/// The arena is owned rather than borrowed independently so the close order is
/// structural: its sweep nulls every block shadow and releases every scratch
/// block before `TRACE_ACTIVE` comes down and any entity slot is replayed.
/// Dropping is the abort path too, so a trace that gives up cannot strand the
/// slots whose reuse it delayed.
#[must_use = "dropping the trace window closes the slot-reuse barrier"]
pub(crate) struct ActiveTrace {
    arena: crate::cycle::arena::TraceScratchArena,
    // A window belongs to the TLS state of the thread that opened it. Moving
    // the guard would close another thread's window and strand this one's.
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl ActiveTrace {
    /// Open this thread's one trace window.
    pub(crate) fn open() -> Self {
        TRACE_ACTIVE.with(|active| {
            let was_active = active.replace(true);
            assert!(!was_active, "a thread runs at most one trace at a time");
        });

        Self {
            arena: crate::cycle::arena::TraceScratchArena::new(),
            _not_send: std::marker::PhantomData,
        }
    }

    /// The trace's working memory. No arena reference can outlive the window,
    /// which is what makes the close order above enforceable by the type.
    pub(crate) fn arena(&mut self) -> &mut crate::cycle::arena::TraceScratchArena {
        &mut self.arena
    }
}

impl Drop for ActiveTrace {
    fn drop(&mut self) {
        // First and unconditionally: after `TRACE_ACTIVE` falls, a physical
        // return may recommission the block whose shadow pointer this sweep
        // must null.
        self.arena.reset();

        TRACE_ACTIVE.with(|active| {
            let was_active = active.replace(false);
            assert!(was_active, "closing a trace window that is not open");
        });

        let list = DEFERRED_SLOTS.with(|slots| slots.get());
        if list.is_null() {
            return;
        }

        // Take the contents before replaying them. `ll_free` may journal, and
        // journal initialisation can re-enter runtime initialisation on this
        // thread; no borrow of the list may span that call.
        let returns = unsafe { std::mem::take(&mut *list) };
        for ptr in returns {
            // Safety: each record is one entity slot whose observable teardown
            // completed before `defer_reuse_if_tracing` accepted the return.
            // Replaying it once through the ordinary door is the return it
            // still owes.
            unsafe { crate::memory::stdapi::ll_free(ptr) };
        }
    }
}

/// Refuse a physical return while the current trace can still address
/// the slot, recording the return for the window's close.
///
/// Called only after the queue-entry window has refused the same return. A
/// replay that still finds `CANDIDATE_BIT` stops before here, because the
/// queue entry itself remains the record.
///
/// # Safety
/// `ptr` is a dead entity slot whose teardown has completed and which this call
/// owns until either the function returns `false` or the window closes.
#[inline]
pub(crate) unsafe fn defer_reuse_if_tracing(ptr: *mut u8) -> bool {
    if !TRACE_ACTIVE.with(Cell::get) {
        return false;
    }

    DEFERRED_SLOTS.with(|cell| {
        let mut list = cell.get();
        if list.is_null() {
            list = Box::into_raw(Box::new(Vec::new()));
            cell.set(list);
        }

        unsafe { (*list).push(ptr) };
    });
    true
}

/// Retire this thread's empty withheld-return list at thread exit.
///
/// A live window at exit would leave a trace using blocks whose owner is being
/// abandoned; that is outside the protocol. In a release build the list is
/// deliberately leaked in that impossible state rather than returning slots
/// while a trace may still name them.
pub(crate) fn dispose_thread_state() {
    let active = TRACE_ACTIVE.with(|active| active.replace(false));
    assert!(!active, "a thread cannot exit inside its trace window");

    let list = DEFERRED_SLOTS.with(|slots| slots.replace(std::ptr::null_mut()));
    if list.is_null() {
        return;
    }

    let list = unsafe { Box::from_raw(list) };
    debug_assert!(
        list.is_empty(),
        "a closed trace flushes every parked return"
    );
    if active || !list.is_empty() {
        std::mem::forget(list);
    }
}

#[cfg(test)]
pub(crate) fn deferred_slot_count() -> usize {
    let list = DEFERRED_SLOTS.with(Cell::get);
    if list.is_null() {
        0
    } else {
        unsafe { (*list).len() }
    }
}

#[cfg(test)]
mod tests;
