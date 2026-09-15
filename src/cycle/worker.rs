//! What a collector thread does for one owner: take the chain the owner's
//! poll offered, trace it through the collector's reader, mark the roots
//! whose components read potentially unreachable, and post the chain back
//! for the owner's exact reading (`rfc/model/gc/rc-cycle.md`,
//! "Worker-to-owner handoff"; `rfc/dev/DECISIONS.md`, "the owner detaches at
//! its poll, and the worker takes the chain from a one-word outbox").
//!
//! # The round over one owner
//!
//! The outbox word is read first, under no claim, and null is a skip. Set,
//! the owner's token is claimed by compare-and-swap — held is a skip — and
//! only then is anything of the owner's touched: an unpicked proposal in the
//! inbox is a skip, since the inbox holds one chain; the outbox is exchanged
//! with null, and a null answer is the owner having reclaimed the offer for a
//! collection of its own, another skip. The chain taken is traced through
//! `cells::AtomicCells` over a workspace of the collector thread's own, and
//! **it is always posted, walked or not** — a trace the pool refuses posts the
//! chain unmarked, a trace that panics posts it from the unwind, and the
//! owner's close puts every unmarked root back in its lane — before the
//! token's release store, so that an owner reading its token free finds the
//! chain in its inbox. A trace that did not complete marks nothing: its
//! colours are no verdict. The marks are the only thing written into the
//! owner's memory here, and they are written over entries of a chain nobody
//! else holds.
//!
//! # What this module does not decide
//!
//! When to ask an owner, which owners, and on what thread: the round over the
//! records, the request that precedes an offer and the thread's own birth are
//! `PLAN.md` S38.7's. A case that stands in for that thread drives
//! [`serve`] from a thread of its own.

// The collector thread that drives this is S38.7's; until it lands the
// round is driven by a case standing in for it.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the collector thread that rounds over the records is S38.7's"
    )
)]

use crate::cells::AtomicCells;
use crate::cycle::arena::{TraceScratchArena, find_initialized_row};
use crate::cycle::owner_record::{self, OwnerRecord};
use crate::cycle::queue::InFlightBatch;
use crate::cycle::row::{EdgeTarget, resolve_edge_target};
use crate::cycle::shadow::{self, Color};
use crate::cycle::trace::{ALL_ROOTS, TraceOutcome, trace_batch};

/// What one round over one owner did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Served {
    /// The outbox read empty.
    NothingOffered,
    /// The owner, or another collector, holds the token.
    TokenHeld,
    /// A chain posted earlier is not yet picked up.
    ProposalUnpicked,
    /// The owner reclaimed the offer between the read and the take.
    Reclaimed,
    /// The chain was traced and posted, `proposed` of its `roots` marked.
    Posted { roots: usize, proposed: usize },
    /// The chain was posted unmarked: the collector thread's workspace or
    /// the trace's rows could not be had.
    PostedUntraced,
}

/// Serve `record`'s owner once: take its offer under its token, trace it,
/// mark, post, release.
///
/// Runs on a collector thread, which holds a base block of its own for the
/// workspace the trace opens (`crate::memory::heap::ll_thread_init`).
///
/// # Safety
/// `record` is a record of the registry's, and the calling thread is not its
/// owner.
pub(crate) unsafe fn serve(record: *mut OwnerRecord) -> Served {
    if !unsafe { owner_record::offer_stands(record) } {
        return Served::NothingOffered;
    }

    let token = unsafe { &(*record).token };
    if !token.try_take() {
        return Served::TokenHeld;
    }

    // Released on the unwind too: a collector that panicked mid-trace would
    // otherwise leave the owner's exit waiting forever.
    struct ReleaseOnDrop<'a>(&'a crate::cycle::token::TraceToken);
    impl Drop for ReleaseOnDrop<'_> {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    let _held = ReleaseOnDrop(token);

    if unsafe { owner_record::proposal_stands(record) } {
        return Served::ProposalUnpicked;
    }

    let Some(mut taken) = (unsafe { TakenChain::take(record) }) else {
        return Served::Reclaimed;
    };

    match TraceScratchArena::open() {
        Some(mut arena) => {
            let served = unsafe { trace_and_mark(&mut arena, taken.batch()) };
            // The rows go back and every shadow pointer the trace stamped on
            // the owner's blocks is nulled before the chain is posted: the
            // owner's own next trace stamps them afresh.
            arena.reset();
            served
        }
        None => Served::PostedUntraced,
    }
    // `taken` posts as it drops, after the arena's reset and before `_held`
    // releases the token: the declaration order is the post's order.
}

/// A chain taken out of an owner's outbox, posted to the owner's inbox when
/// this drops — on the return and on the unwind alike, so a trace that
/// panics still hands every root back. Declared after the token guard so
/// that it drops first and the post precedes the release.
struct TakenChain {
    record: *mut OwnerRecord,
    batch: Option<InFlightBatch>,
}

impl TakenChain {
    /// Exchange `record`'s outbox with null and hold what it named, or `None`
    /// when the owner reclaimed the offer meanwhile.
    ///
    /// # Safety
    /// The caller holds `record`'s token.
    unsafe fn take(record: *mut OwnerRecord) -> Option<Self> {
        let word = unsafe { owner_record::take_offer(record) };
        if word == 0 {
            return None;
        }

        Some(Self {
            record,
            batch: Some(InFlightBatch::from_word(word, false)),
        })
    }

    fn batch(&mut self) -> &mut InFlightBatch {
        self.batch
            .as_mut()
            .expect("the chain is posted only at the drop")
    }
}

impl Drop for TakenChain {
    fn drop(&mut self) {
        let batch = self.batch.take().expect("the chain is posted once");
        unsafe { owner_record::post(self.record, batch.into_word()) };
    }
}

/// Trace `batch` through the collector's reader and mark the roots the scan
/// read potentially unreachable.
///
/// # Safety
/// The calling thread holds the owner's token, and `batch` is the chain it
/// took from the owner's outbox.
unsafe fn trace_and_mark(arena: &mut TraceScratchArena, batch: &mut InFlightBatch) -> Served {
    let (outcome, roots) = unsafe { trace_batch::<AtomicCells>(arena, batch, ALL_ROOTS) };
    if outcome != TraceOutcome::Complete {
        return Served::PostedUntraced;
    }

    // A root at count zero took no row and is not proposed; a root whose row
    // the scan raised to live is not either; the rest are the proposal
    // (`rfc/model/gc/rc-cycle.md`, "Speculative tracing and exact
    // validation").
    let proposed = batch.mark_proposed(|root| {
        let EdgeTarget::Tracked(key) = (unsafe { resolve_edge_target(root) }) else {
            return false;
        };

        match unsafe { find_initialized_row(key) } {
            Some(row) => shadow::color(unsafe { *row }) == Color::PotentiallyUnreachable,
            None => false,
        }
    });
    Served::Posted { roots, proposed }
}

#[cfg(test)]
mod tests;
