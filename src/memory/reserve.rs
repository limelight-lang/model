//! The per-thread log reserve: blocks kept back so the store barrier
//! cannot fail.
//!
//! `ll_ref_store` has no way to report a failure — it returns nothing,
//! and giving it a channel would mean a check after every reference
//! store, which is the Zend shape this runtime rejects
//! (`rfc/runtime/exceptions.md`, "The enumeration"). But the barrier can
//! fail: recording an escape or a release-at-reset grows an arena log,
//! and growing it needs memory. A lost record is not a failed operation,
//! it is a dangling reference at reset.
//!
//! So the memory is kept back in advance. Two blocks per mutator thread,
//! filled where a refusal is already reportable (thread init, whose
//! failure surfaces as a null from the thread's first allocation), drawn
//! only by log growth and only after the ordinary path has been refused.
//!
//! **A reserve block never becomes an arena's bump block.** That is the
//! rule the whole thing rests on: if ordinary allocation could reach it,
//! the next object would eat the reserve instead of getting the null
//! that lets a Limelight frame raise. The arena carves log segments from
//! it with a cursor of its own and links the block into its block list,
//! so reset returns it to the pool like any other.
//!
//! **What "cannot fail" really means.** The reserve is finite, so this
//! is not a proof — it is a conversion. Drawing on it sets a flag that
//! the compiler's safepoint poll checks (`ll_gc_maybe_collect`), and the
//! poll runs where a Limelight frame exists: it refills, and if it
//! cannot, it is free to raise memory-exhausted as an ordinary
//! exception. The failure the barrier could not report becomes a raise
//! at the next poll, thousands of records before the reserve would
//! actually run dry. The contract that makes the arithmetic hold — a
//! bounded number of barrier operations between two polls — is owed by
//! the compiler, and there is no compiler yet.

use std::cell::RefCell;

use crate::memory::block_pool::{BLOCK_KIND_ARENA, BlockHeader, BlockPool};

/// Blocks held back per thread. Two, sized from the poll contract: one
/// block holds ~15 log segments of 500 records each, so two cover
/// ~15 000 records — several times any sane bound on barrier operations
/// between two safepoints.
pub(crate) const RESERVE_BLOCKS: usize = 2;

struct Reserve {
    blocks: Vec<*mut BlockHeader>,
    /// Set when a block was taken and not yet replaced. Read by the
    /// safepoint poll; never cleared by the drawing path itself.
    drawn: bool,
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
        for &block in &self.blocks {
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
    let blocks = RESERVE
        .try_with(|reserve| std::mem::take(&mut reserve.borrow_mut().blocks))
        .unwrap_or_default();
    for block in blocks {
        BlockPool::global().put(block);
    }
}

thread_local! {
    static RESERVE: RefCell<Reserve> = const {
        RefCell::new(Reserve { blocks: Vec::new(), drawn: false })
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
            while r.blocks.len() < RESERVE_BLOCKS {
                let block = BlockPool::global().get();
                if block.is_null() {
                    return false;
                }
                unsafe { (*block).kind = BLOCK_KIND_ARENA };
                r.blocks.push(block);
            }
            r.drawn = false;
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
            match r.blocks.pop() {
                Some(block) => {
                    r.drawn = true;
                    block
                }
                None => std::ptr::null_mut(),
            }
        })
        .unwrap_or(std::ptr::null_mut())
}

/// Whether the reserve is short and wants a safepoint to refill it.
pub(crate) fn is_drawn() -> bool {
    RESERVE.try_with(|r| r.borrow().drawn).unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn blocks_held() -> usize {
    RESERVE.with(|r| r.borrow().blocks.len())
}

/// Tests only: give the blocks back and forget them, so a test that
/// exhausts the pool starts from a known state. The giving back is
/// [`drain`]'s; what is test-only is forgetting that a block was drawn.
#[cfg(test)]
pub(crate) fn drain_for_test() {
    drain();
    RESERVE.with(|r| r.borrow_mut().drawn = false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filling_then_drawing_reports_that_it_wants_a_refill() {
        let _g = crate::memory::block_pool::test_guard();
        drain_for_test();

        assert!(replenish());
        assert_eq!(blocks_held(), RESERVE_BLOCKS);
        assert!(!is_drawn());

        let block = draw();
        assert!(!block.is_null());
        assert!(is_drawn(), "a draw asks the next safepoint for a refill");
        assert_eq!(blocks_held(), RESERVE_BLOCKS - 1);

        BlockPool::global().put(block);
        assert!(replenish());
        assert!(!is_drawn(), "and the refill answers it");
        drain_for_test();
    }

    /// The reserve exists for the case where the pool says no, so it has
    /// to keep working when it does.
    #[test]
    fn a_drawn_block_is_available_while_the_pool_refuses() {
        let _g = crate::memory::block_pool::test_guard();
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;
        drain_for_test();
        assert!(replenish());

        FORCE_OOM.store(true, Ordering::Relaxed);
        assert!(BlockPool::global().get().is_null(), "the pool is refusing");
        let block = draw();
        assert!(!block.is_null(), "the reserve still hands one out");
        assert!(!replenish(), "and cannot be refilled while it lasts");
        FORCE_OOM.store(false, Ordering::Relaxed);

        BlockPool::global().put(block);
        drain_for_test();
    }
}
