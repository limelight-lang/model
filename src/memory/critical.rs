//! The per-thread critical reserve: blocks a collection draws when the
//! ordinary door has already refused.
//!
//! The protocol and who is entitled to it: `rfc/model/memory/`
//! `critical-reserve.md`, "The two doors" and "The three customers". Why
//! it stands beside the log reserve rather than inside it:
//! `rfc/runtime/exceptions.md`, "The reserve is three reserves, not one"
//! — a consumer that shared a reserve with another would make each
//! consumer's worst case the sum of both, and a collection that drained
//! the log reserve would turn its own abort into a store barrier that
//! cannot fail and does.
//!
//! # Who draws, and when
//!
//! Two customers exist in this crate. The cycle collection's shadow
//! arena (`crate::cycle::arena`) asks the ordinary pool first — the
//! in-line collection is the standard form rather than the emergency
//! one, and most of its runs begin with no refusal anywhere — and
//! reaches here only on a null from the pool, which on the pressure path
//! is its first draw because the refusal is what triggered the
//! collection. The enrolment queue's growth
//! (`crate::cycle::queue`) reaches here on a different condition: its
//! two spare cells are both empty, which means a poll's refill was
//! already refused, and the draw puts the runtime in reserve mode. The
//! third customer `critical-reserve.md` names, the mutator that cannot
//! collect, arrives with S38.4 of `PLAN.md`; no partition among the
//! three is built until one of their shares can be derived.
//!
//! **The queue is also the second caller of [`give_back`]**, its
//! segments coming back at thread exit, which is why that drain runs
//! before this reserve's own (`memory::heap::ll_thread_exit`).
//!
//! # What a drawn block owes
//!
//! It comes back through [`give_back`], which refills the reserve before
//! the pool sees anything. A collection that returned its blocks
//! straight to the pool would leave the reserve empty for the retry that
//! follows an abort, and the next pressure event would find nothing here
//! until a safepoint ran.

use std::cell::RefCell;

use crate::memory::block_pool::{BLOCK_KIND_ARENA, BlockHeader, BlockPool};

/// Blocks held back per thread: eight, which is 512 KiB and is
/// `critical-reserve.md`'s 500 KB figure read at block granularity. The
/// figure is a starting one rather than a measured one, and what would
/// settle it is a workload — that document's "Sizing" says so of all
/// three shares.
///
/// What it buys, at four bytes a row: about thirty blocks of the
/// smallest size class traced, more than a hundred at the middle
/// classes. On the pressure path that capacity **is** the in-line
/// collection's trace budget, and exhausting it aborts the collection
/// into the retry `rfc/runtime/exceptions.md` already promises.
pub(crate) const CRITICAL_BLOCKS: usize = 8;

/// A fixed array rather than a `Vec`, and this is the reserve's own
/// rule turned on itself: a `Vec::push` that cannot allocate calls
/// `handle_alloc_error`, which aborts, and the first push of an empty
/// `Vec` happens inside `replenish` on a thread initialising under the
/// very pressure this reserve exists to survive. `heap.rs` refuses the
/// same failure mode for `ThreadHeaps` and `block_pool` for its regions.
struct Critical {
    blocks: [*mut BlockHeader; CRITICAL_BLOCKS],
    held: usize,
}

impl Drop for Critical {
    /// The fallback for a thread that never ran `ll_thread_exit`, the
    /// same one [`crate::memory::reserve`] keeps and for the same
    /// reason: the pool serves threads this runtime never initialised,
    /// and a dying thread must not take pool blocks with it.
    fn drop(&mut self) {
        for &block in &self.blocks[..self.held] {
            BlockPool::global().put(block);
        }
    }
}

thread_local! {
    static CRITICAL: RefCell<Critical> = const {
        RefCell::new(Critical {
            blocks: [std::ptr::null_mut(); CRITICAL_BLOCKS],
            held: 0,
        })
    };
}

/// Fill the reserve to capacity, returning false if it could not be
/// filled completely.
///
/// Best-effort by construction: it runs at thread init and at
/// safepoints, both places where a refusal is already reported by
/// something else — the thread's first allocation returns null, and a
/// poll that cannot refill leaves the drawn flag set for the next one.
pub(crate) fn replenish() -> bool {
    CRITICAL
        .try_with(|c| {
            let mut c = match c.try_borrow_mut() {
                Ok(c) => c,
                // Reentered from inside a draw; the poll will come back.
                Err(_) => return false,
            };
            while c.held < CRITICAL_BLOCKS {
                let block = BlockPool::global().get();
                if block.is_null() {
                    return false;
                }

                // Through `store_block_kind` for the reason every other
                // commissioning uses it: the collector acquire-loads the
                // kind of every block in every carved region. The kind is
                // `ARENA` because what matters is that it is not
                // `ENTITY` — a trace never enters a block of any other
                // kind (`crate::cycle::row`).
                unsafe {
                    crate::memory::block_pool::store_block_kind(
                        &raw const (*block).kind,
                        BLOCK_KIND_ARENA,
                    )
                };

                let held = c.held;
                c.blocks[held] = block;
                c.held = held + 1;
            }

            true
        })
        // During TLS teardown there is no reserve to fill, and nothing
        // that would draw on one either.
        .unwrap_or(false)
}

/// Take one block, or null when the reserve is empty.
///
/// The caller owns the block from here and owes it back through
/// [`give_back`]. Null is the answer that ends a collection: both doors
/// have refused, and the caller aborts rather than failing the process.
pub(crate) fn draw() -> *mut BlockHeader {
    CRITICAL
        .try_with(|c| {
            let mut c = match c.try_borrow_mut() {
                Ok(c) => c,
                Err(_) => return std::ptr::null_mut(),
            };

            if c.held == 0 {
                return std::ptr::null_mut();
            }

            c.held -= 1;
            let taken = c.held;
            std::mem::replace(&mut c.blocks[taken], std::ptr::null_mut())
        })
        .unwrap_or(std::ptr::null_mut())
}

/// Return a block the reserve lent out. The reserve keeps it if it is
/// below capacity, and hands it to the pool otherwise.
///
/// The caller passes back every block it holds, whatever door it came
/// through, and this decides: at capacity the block is ordinary memory
/// again, below capacity it refills the reserve without waiting for a
/// safepoint. That ordering is what leaves an aborted collection's
/// retry with a reserve to draw on (module doc).
pub(crate) fn give_back(block: *mut BlockHeader) {
    let kept = CRITICAL
        .try_with(|c| {
            let mut c = match c.try_borrow_mut() {
                Ok(c) => c,
                Err(_) => return false,
            };

            if c.held >= CRITICAL_BLOCKS {
                return false;
            }

            // Stamped here rather than trusted: every block the reserve
            // hands out is assumed to carry this kind already, and what
            // enforces that is each caller stamping its own — the arena's
            // reset and the enrolment queue's drain, which are the two.
            // A release store on a cold path is cheaper than an invariant
            // living in two other files.
            unsafe {
                crate::memory::block_pool::store_block_kind(
                    &raw const (*block).kind,
                    BLOCK_KIND_ARENA,
                )
            };

            let held = c.held;
            c.blocks[held] = block;
            c.held = held + 1;
            true
        })
        .unwrap_or(false);

    if !kept {
        BlockPool::global().put(block);
    }
}

/// Whether the reserve is short and wants a safepoint to refill it.
///
/// The count itself, rather than a flag a draw sets. A flag is false in
/// the one state that most needs the poll: a thread whose `replenish` at
/// init was refused holds nothing, has never drawn, and would never be
/// asked again — the poll skips it, the next pressure event finds the
/// door shut, and the collection aborts having traced nothing.
pub(crate) fn is_drawn() -> bool {
    CRITICAL
        .try_with(|c| c.borrow().held < CRITICAL_BLOCKS)
        .unwrap_or(false)
}

/// Give the blocks back to the pool by hand, while the thread still
/// exists. Called from `heap::ll_thread_exit` beside the log reserve's
/// drain and for the same reason: a block going home is a journal event,
/// and a TLS destructor runs after the exit pass on any platform that
/// destroys in reverse registration order.
///
/// Idempotent: a drained reserve drains to nothing.
pub(crate) fn drain() {
    let mut taken = [std::ptr::null_mut(); CRITICAL_BLOCKS];
    let held = CRITICAL
        .try_with(|c| {
            let mut c = match c.try_borrow_mut() {
                Ok(c) => c,
                Err(_) => return 0,
            };

            let held = c.held;
            taken[..held].copy_from_slice(&c.blocks[..held]);
            c.blocks = [std::ptr::null_mut(); CRITICAL_BLOCKS];
            c.held = 0;
            held
        })
        .unwrap_or(0);

    for &block in &taken[..held] {
        BlockPool::global().put(block);
    }
}

#[cfg(test)]
pub(crate) fn blocks_held() -> usize {
    CRITICAL.with(|c| c.borrow().held)
}

/// Tests only: give the blocks back, so a test that exhausts the pool
/// starts from a known state. An emptied reserve reads as drawn, which
/// is what it is.
#[cfg(test)]
pub(crate) fn drain_for_test() {
    drain();
}

#[cfg(test)]
mod tests;
