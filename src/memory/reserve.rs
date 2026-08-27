//! The per-thread log reserve: blocks kept back so the store barrier
//! cannot fail.
//!
//! The protocol, and the compiler-side contract it rests on:
//! `rfc/runtime/exceptions.md`, "Allocation failure is an ordinary
//! exception". How this crate builds it: `docs/memory-manager.md`,
//! "The log reserve".

use std::cell::RefCell;

use crate::memory::block_pool::{BLOCK_KIND_ARENA, BlockHeader, BlockPool};

/// Blocks held back per thread. Two, sized from the poll contract: one
/// block holds ~15 log segments of 500 records each, so two cover
/// ~15 000 records — several times any sane bound on barrier operations
/// between two safepoints.
pub(crate) const RESERVE_BLOCKS: usize = 2;

/// A fixed array rather than a `Vec`: a `Vec::push` that cannot
/// allocate calls `handle_alloc_error`, which aborts, and the first push
/// of an empty `Vec` happens inside [`replenish`] on a thread
/// initialising under pressure. The reserve exists so that the store
/// barrier's failure becomes reportable, and an abort inside its own
/// refill is the failure mode it was built to remove.
struct Reserve {
    blocks: [*mut BlockHeader; RESERVE_BLOCKS],
    held: usize,
}

impl Drop for Reserve {
    /// The fallback for a thread that never ran `ll_thread_exit` — the
    /// pool serves threads this runtime never initialised. On the
    /// contract path [`drain`] has already emptied this, so the loop
    /// finds nothing.
    ///
    /// A dying thread must not take the blocks with it either way — same
    /// rule as the pool's thread cache, and the reserve is pure spare
    /// memory, so there is nothing to decide here.
    fn drop(&mut self) {
        for &block in &self.blocks[..self.held] {
            BlockPool::global().put(block);
        }
    }
}

/// Give the reserve's blocks back to the pool, by hand, while the thread
/// still exists.
///
/// Called from `heap::ll_thread_exit` before the journal's ring retires,
/// so that the handovers are inside the ring rather than after it: a
/// block going back to the pool is a default event kind
/// (`dev/design/debug-modes.md` §9.5), and a TLS destructor runs after
/// the exit on any platform that destroys in reverse registration order,
/// which is where this cell sits (`heap::ll_thread_exit`).
///
/// `try_with`, because it can be reached from a destructor after this
/// cell's own has run. Idempotent: a drained reserve drains to nothing.
pub(crate) fn drain() {
    let mut taken = [std::ptr::null_mut(); RESERVE_BLOCKS];
    let held = RESERVE
        .try_with(|reserve| {
            let mut r = match reserve.try_borrow_mut() {
                Ok(r) => r,
                Err(_) => return 0,
            };

            let held = r.held;
            taken[..held].copy_from_slice(&r.blocks[..held]);
            r.blocks = [std::ptr::null_mut(); RESERVE_BLOCKS];
            r.held = 0;
            held
        })
        .unwrap_or(0);

    for &block in &taken[..held] {
        BlockPool::global().put(block);
    }
}

thread_local! {
    static RESERVE: RefCell<Reserve> = const {
        RefCell::new(Reserve {
            blocks: [std::ptr::null_mut(); RESERVE_BLOCKS],
            held: 0,
        })
    };
}

/// Fill the reserve to capacity. Best-effort by construction: this runs
/// at thread init and at safepoints, both places where a refusal is
/// already reported by something else. Returns false if it could not be
/// filled completely — the caller decides whether that is worth acting
/// on.
pub(crate) fn replenish() -> bool {
    RESERVE
        .try_with(|r| {
            let mut r = match r.try_borrow_mut() {
                Ok(r) => r,
                // Reentered from inside a draw; the poll will come back.
                Err(_) => return false,
            };
            while r.held < RESERVE_BLOCKS {
                let block = BlockPool::global().get();
                if block.is_null() {
                    return false;
                }

                // Through `store_block_kind` for the reason every other
                // commissioning uses it: the collector acquire-loads the
                // kind of every block in every carved region.
                unsafe {
                    crate::memory::block_pool::store_block_kind(
                        &raw const (*block).kind,
                        BLOCK_KIND_ARENA,
                    )
                };

                let held = r.held;
                r.blocks[held] = block;
                r.held = held + 1;
            }

            true
        })
        // During TLS teardown there is no reserve to fill, and nothing
        // that would draw on one either.
        .unwrap_or(false)
}

/// Take one block from the reserve, or null when it is empty. The caller
/// owns the block from here — it must link it somewhere that returns it
/// to the pool (an arena's block list) rather than dropping it.
pub(crate) fn draw() -> *mut BlockHeader {
    RESERVE
        .try_with(|r| {
            let mut r = match r.try_borrow_mut() {
                Ok(r) => r,
                Err(_) => return std::ptr::null_mut(),
            };

            if r.held == 0 {
                return std::ptr::null_mut();
            }

            r.held -= 1;
            let taken = r.held;
            std::mem::replace(&mut r.blocks[taken], std::ptr::null_mut())
        })
        .unwrap_or(std::ptr::null_mut())
}

/// Whether the reserve is short and wants a safepoint to refill it.
///
/// The count itself, rather than a flag a draw sets. A flag is false in
/// the state that most needs the poll: a thread whose `replenish` at init
/// was refused holds nothing, has never drawn, and would never be asked
/// again — after which the barrier's first growth has no reserve behind
/// it and no channel to say so.
pub(crate) fn is_drawn() -> bool {
    RESERVE
        .try_with(|r| r.borrow().held < RESERVE_BLOCKS)
        .unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn blocks_held() -> usize {
    RESERVE.with(|r| r.borrow().held)
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
