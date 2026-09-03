//! The memory-manager allocation path for blocks owned directly by cycle
//! collection, and the return that ends that ownership.
//!
//! A pool block does not become GC memory merely because its kind keeps the
//! entity walker out. Ownership begins here, where the block is stamped and
//! counted, and ends here before it returns to the pool or the critical
//! reserve. Moving a queue segment from a spare cell to the write position
//! changes no ownership and therefore crosses no function in this module.
//!
//! What the count answers is "how much memory does collection hold", and the
//! block kind is what separates that memory from a request arena's or an
//! entity heap's. A split by use — queue against workspace — is not kept: no
//! reader needs it, and a measurement that wants it can be taken on the day
//! (`dev/DECISIONS.md`, 2026-09-01).
//!
//! Beside the blocks, one pair of logical figures: the bytes a structure has
//! taken into use inside them. A block is reserved whole and used in part, so
//! the block count alone cannot say whether collection is holding memory it
//! needs. The charge lands at a structural transition — a queue segment
//! leaving the write position, an overflow-buffer append, a queue-base control
//! line, a trace-scratch block leaving the bump, a withheld-return block
//! leaving the append position — never per grant, which is what keeps the
//! candidate-registration path and the free path free of it. Three residues
//! follow from that and are granularity rather than error: the write segment's
//! own fill, the block under the trace scratch arena's bump and the
//! withheld-return block still under the cursor. Each is entered in the
//! high-water figure by the transition that ends it, and by a mark rather than
//! a charge, the bytes standing there being released in the same breath — so
//! that figure is exact for one thread and can miss a maximum two threads
//! stood in together.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::memory::block_pool::{
    BLOCK_KIND_ARENA, BLOCK_KIND_FREE, BLOCK_KIND_GC_METADATA, BLOCK_SIZE, BlockHeader, BlockPool,
    load_block_kind, store_block_kind,
};

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static IN_USE: AtomicUsize = AtomicUsize::new(0);
static IN_USE_PEAK: AtomicUsize = AtomicUsize::new(0);

/// The four figures above, counted for one thread rather than for the process.
///
/// Signed, because nothing in this module makes a block the acquiring thread's
/// to return: a release on another thread lowers that thread's figure below
/// zero, and [`thread_stats`] refuses such a reading rather than wrapping it
/// into one that reads as the whole address space.
#[cfg(test)]
#[derive(Clone, Copy)]
struct ThreadFigures {
    current: isize,
    peak: isize,
    in_use: isize,
    in_use_peak: isize,
}

#[cfg(test)]
thread_local! {
    /// What this thread has taken and given back.
    ///
    /// `Cell` and `const`-initialised, on the terms
    /// `crate::test_support::allocation_probe` states for its own counters:
    /// the sites that move this one are inside the memory manager, so a
    /// counter that allocated to be read would re-enter it, and a
    /// `const` block with no destructor is still readable from the release
    /// that runs at thread exit.
    static THREAD_FIGURES: std::cell::Cell<ThreadFigures> = const {
        std::cell::Cell::new(ThreadFigures {
            current: 0,
            peak: 0,
            in_use: 0,
            in_use_peak: 0,
        })
    };
}

/// Move this thread's figures, reading and writing the cell once.
#[cfg(test)]
#[inline]
fn move_thread_figures(by: impl FnOnce(&mut ThreadFigures)) {
    THREAD_FIGURES.with(|figures| {
        let mut moved = figures.get();
        by(&mut moved);
        figures.set(moved);
    });
}

/// A non-transactional observation of what collection holds. The figures are
/// read independently, so a concurrent handoff may make them describe adjacent
/// instants; reservation bytes are derived from their own block count and can
/// never disagree with it.
///
/// **The two axes are not read together.** Bytes are read first and blocks
/// after, so a queue drained between the two loads leaves
/// [`current_bytes_in_use`](GcMemoryStats::current_bytes_in_use) standing above
/// [`current_bytes`](GcMemoryStats::current_bytes) — bytes charged against
/// blocks the later load no longer counts. What does hold is that neither
/// high-water figure reads below its own current one, which [`stats`]
/// establishes rather than the counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcMemoryStats {
    current: usize,
    peak: usize,
    in_use: usize,
    in_use_peak: usize,
}

impl GcMemoryStats {
    #[inline]
    pub fn current_blocks(self) -> usize {
        self.current
    }

    #[inline]
    pub fn peak_blocks(self) -> usize {
        self.peak
    }

    #[inline]
    pub fn current_bytes(self) -> usize {
        self.current * BLOCK_SIZE
    }

    #[inline]
    pub fn peak_bytes(self) -> usize {
        self.peak * BLOCK_SIZE
    }

    /// Bytes inside those blocks that a collection structure has taken into
    /// use, which is what says how much of the reservation is working memory.
    ///
    /// In use rather than reserved: a spare segment, a block header and an
    /// overflow buffer with no entry in it are outside this figure, so the gap
    /// to [`current_bytes`](Self::current_bytes) is bounded by nothing in
    /// particular. What is bounded is the figure's own lag — at most one write
    /// segment's fill per thread, and the consumption of one block under a
    /// trace scratch arena's bump plus one withheld-return block's per
    /// collection in flight, each entered by the transition that ends it.
    #[inline]
    pub fn current_bytes_in_use(self) -> usize {
        self.in_use
    }

    /// The most [`current_bytes_in_use`](Self::current_bytes_in_use) has stood
    /// at once since the process began, and never below it.
    ///
    /// Carries the residues the current figure lags by, each entered by the
    /// transition that ends it: a collection that held one block for two
    /// hundred bytes is in this figure at two hundred, a thread that filled a
    /// queue segment without growing the queue enters its fill when it
    /// releases its segments, and a collection enters the bump its reset
    /// rewinds and the block its withheld returns were being written into when
    /// it closes. **Entered at that
    /// transition and not while the residue stands**, so a residue that
    /// coexisted with another thread's maximum is in this figure only if it
    /// outlived it.
    #[inline]
    pub fn peak_bytes_in_use(self) -> usize {
        self.in_use_peak
    }
}

/// Observe the blocks cycle collection owns now and the most it has held.
pub fn stats() -> GcMemoryStats {
    let in_use = IN_USE.load(Ordering::Relaxed);
    let current = CURRENT.load(Ordering::Relaxed);
    // Both high-water figures are lifted to their own current one. Each is
    // raised by an add and then a separate maximum, so a reader landing
    // between the two would otherwise be told the most-ever figure stands
    // below the now figure.
    GcMemoryStats {
        current,
        peak: PEAK.load(Ordering::Relaxed).max(current),
        in_use,
        in_use_peak: IN_USE_PEAK.load(Ordering::Relaxed).max(in_use),
    }
}

/// Take `bytes` into use inside blocks this module already owns.
///
/// The caller charges at a transition that has one inverse — a segment leaving
/// the write position, an overflow-buffer append, a queue-base control line, a
/// trace-scratch block leaving the bump, a withheld-return block leaving the
/// append position — and never per grant (`dev/DECISIONS.md`, "the logical
/// charge lands at a structural transition, not at a grant").
pub(crate) fn charge(bytes: usize) {
    let in_use = IN_USE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    IN_USE_PEAK.fetch_max(in_use, Ordering::Relaxed);
    #[cfg(test)]
    move_thread_figures(|figures| {
        figures.in_use += bytes as isize;
        figures.in_use_peak = figures.in_use_peak.max(figures.in_use);
    });
}

/// Put `bytes` of use into the high-water figure without ever standing in the
/// current one.
///
/// The instrument for a residue the ledger carries deliberately: the bytes are
/// in use, the transition that ends them is releasing them, and a
/// charge-then-discharge pair would show a reader on another thread a current
/// figure that overstates what collection holds.
pub(crate) fn mark_peak(bytes: usize) {
    let in_use = IN_USE.load(Ordering::Relaxed) + bytes;
    IN_USE_PEAK.fetch_max(in_use, Ordering::Relaxed);
    #[cfg(test)]
    move_thread_figures(|figures| {
        figures.in_use_peak = figures.in_use_peak.max(figures.in_use + bytes as isize);
    });
}

/// End the use of `bytes` a [`charge`] took.
///
/// Fails rather than wraps when more is discharged than stands: the figure is
/// read as memory the process holds, and a wrapped one reads as a leak of the
/// whole address space.
pub(crate) fn discharge(bytes: usize) {
    IN_USE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |in_use| {
            in_use.checked_sub(bytes)
        })
        .expect("the GC byte ledger cannot underflow");
    #[cfg(test)]
    move_thread_figures(|figures| figures.in_use -= bytes as isize);
}

#[inline]
fn acquired(block: *mut BlockHeader, source_kind: u32) -> *mut BlockHeader {
    if block.is_null() {
        return block;
    }

    assert_eq!(
        unsafe { load_block_kind(&raw const (*block).kind) },
        source_kind,
        "adopting a block across the wrong ownership boundary"
    );
    // The count precedes the release publication of the kind, so a reader that
    // sees GC_METADATA sees a block already charged rather than one the
    // eventual releaser will have to account for.
    let current = CURRENT.fetch_add(1, Ordering::Relaxed) + 1;
    PEAK.fetch_max(current, Ordering::Relaxed);
    #[cfg(test)]
    move_thread_figures(|figures| {
        figures.current += 1;
        figures.peak = figures.peak.max(figures.current);
    });

    unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_GC_METADATA) };
    block
}

#[inline]
fn released(block: *mut BlockHeader) {
    let kind = unsafe { load_block_kind(&raw const (*block).kind) };
    assert_eq!(kind, BLOCK_KIND_GC_METADATA, "returning a non-GC block");

    CURRENT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_sub(1)
        })
        .expect("the GC block counter cannot underflow");
    #[cfg(test)]
    move_thread_figures(|figures| figures.current -= 1);
}

/// Draw ordinary pool memory and make it GC-owned.
pub(crate) fn acquire() -> *mut BlockHeader {
    acquired(BlockPool::global().get(), BLOCK_KIND_FREE)
}

/// Adopt a block lent by the critical reserve. Null remains null.
pub(crate) fn adopt(block: *mut BlockHeader) -> *mut BlockHeader {
    acquired(block, BLOCK_KIND_ARENA)
}

/// End GC ownership and return ordinary memory to the pool.
pub(crate) fn release(block: *mut BlockHeader) {
    if block.is_null() {
        return;
    }
    released(block);
    // End the GC population before crossing the manager boundary. `put`
    // rejects a still-GC-stamped block, making a direct bypass observable.
    unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_FREE) };
    BlockPool::global().put(block);
}

/// End GC ownership and return a block through the critical reserve.
pub(crate) fn release_to_critical(block: *mut BlockHeader) {
    if block.is_null() {
        return;
    }
    released(block);
    unsafe { store_block_kind(&raw const (*block).kind, BLOCK_KIND_ARENA) };
    crate::memory::critical::give_back(block);
}

/// Observe what this thread has taken and given back, in the figures [`stats`]
/// answers for the process.
///
/// Tests only, and the reading an exact assertion needs. The process figures
/// are moved by every thread the suite is running, so they answer "what did
/// this path do" only while no other test allocates — which is a property of
/// the run rather than of the path, and the suite reached the failing side of
/// it about once in twenty-five runs at sixteen threads
/// (`dev/POSTMORTEM.md`, "an exact assertion cannot be made against a
/// process-global ledger"). A thread's own figures are moved by its own work
/// alone.
///
/// The high-water pair is this thread's since
/// [`lower_thread_peak_to_current`] last lowered it, and never below the
/// current figure beside it — the same lift [`stats`] applies, for the same
/// reason.
#[cfg(test)]
pub(crate) fn thread_stats() -> GcMemoryStats {
    let figures = THREAD_FIGURES.with(|figures| figures.get());
    let taken_here = |figure: isize| {
        usize::try_from(figure)
            .expect("a charge or a block another thread made was ended on this one")
    };
    let current = taken_here(figures.current);
    let in_use = taken_here(figures.in_use);
    GcMemoryStats {
        current,
        peak: taken_here(figures.peak).max(current),
        in_use,
        in_use_peak: taken_here(figures.in_use_peak).max(in_use),
    }
}

/// Lower both of this thread's high-water figures to their current ones.
///
/// Tests only, and the instrument an exact assertion needs: a high-water
/// figure never falls, so a rise of a known size is otherwise absorbed by
/// whatever an earlier test on this thread reached. Both axes, because an
/// assertion over a whole [`GcMemoryStats`] is exact only when every field of
/// it can move down.
///
/// This thread's and not the process's: a store into the process figures is
/// itself what an assertion on another thread would then read as its own
/// path's work.
#[cfg(test)]
pub(crate) fn lower_thread_peak_to_current() {
    move_thread_figures(|figures| {
        figures.peak = figures.current;
        figures.in_use_peak = figures.in_use;
    });
}

#[cfg(test)]
mod tests;
