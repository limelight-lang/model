//! The split of a set the Δ-test proved (`dev/design/recycler-over-counts.md`,
//! §5a, S68.6c, as the Sage ruled it in item 8): U, what the touched members'
//! recorded edges reach, refused; S, what the members the collector cannot
//! free reach, the owner's; C, the rest, the collector's to free.
//!
//! **One mark for both closures.** A potentially unreachable row's payload,
//! after a scan over the record, is the run index its first pass left — zero
//! for a leaf — and its top bit is free ([`SPLIT_MARK`]). The Δ-test marks
//! the touched members; [`close_over_the_record`] marks what their runs
//! reach; [`refuse_the_marked`] recolours those rows `Unclassified`, which
//! no append lists, no stamp reads and no membership test counts, and clears
//! the mark; then [`mark_the_seeds`] marks the members the collector cannot
//! free and a second closure marks what they reach. What stands marked is S,
//! what stands unmarked C.
//!
//! **Why only what a touch reaches.** A touched member is made live only by
//! a count write, which tags it, or by a count-free move out of a tagged
//! holder; so only its recorded successors are suspect, and an edge into U
//! from the rest is a drop like any edge out of C. U is closed under
//! successors, so S has no edge into it. **Why S closes over successors**:
//! no destructor and no weak upgrade in S may reach C, which the collector
//! frees.
//!
//! **The closures read the record, not the heap.** Every met entity that is
//! not a leaf was expanded once by a completed mark, and an edge out of a
//! member never expanded was never subtracted, so its target is not in W.

use std::ops::ControlFlow;

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mutator_record::MutatorRecord;
use crate::cycle::row::{self, Population};
use crate::cycle::shadow::{self, Color, SPLIT_MARK};
use crate::cycle::stack::WorklistEntry;

/// Whether `word` is a potentially unreachable row carrying the mark.
#[inline]
pub(crate) fn is_marked(word: u32) -> bool {
    shadow::color(word) == Color::PotentiallyUnreachable && word & SPLIT_MARK != 0
}

/// The run index of a potentially unreachable row: zero for a leaf.
#[inline]
fn run_of(word: u32) -> u32 {
    shadow::count(word) & !SPLIT_MARK
}

/// Mark `row`, a potentially unreachable row of a completed scan over the
/// record.
///
/// # Safety
/// `row` is a met row of the arena whose scan completed, still standing.
#[inline]
pub(crate) unsafe fn mark(row: *mut u32) {
    let word = unsafe { *row };
    debug_assert_eq!(shadow::color(word), Color::PotentiallyUnreachable);
    unsafe { row.write(word | SPLIT_MARK) };
}

/// Visit every potentially unreachable row, marked or not, with its member
/// where the address can be recovered.
///
/// # Safety
/// The rows of a completed scan over the record stand, under the grant.
unsafe fn for_each_member_row(
    arena: &TraceScratchArena,
    mut visit: impl FnMut(*mut u8, Population, *mut u32, Option<*mut crate::refcount::RcHeader>),
) {
    let mut array = arena.touched_head();
    while !array.is_null() {
        let (block, population) = unsafe { ((*array).block, (*array).population) };
        let _ = unsafe {
            row::for_each_proposable_met(array, block, population, |index| {
                visit(
                    block,
                    population,
                    row::row_at(array, block, population, index),
                    row::entity_at(block, population, index),
                );
                ControlFlow::Continue(())
            })
        };
        array = unsafe { (*array).next };
    }
}

/// Mark every potentially unreachable row the runs of the marked rows reach
/// over the record. `Break` at a recall at the stop level or where both
/// allocation paths refused the worklist, the marks then partial.
///
/// # Safety
/// As [`mark`], every row of the arena.
pub(crate) unsafe fn close_over_the_record(arena: &mut TraceScratchArena) -> ControlFlow<()> {
    // The walk reads the touched list while the worklist grows in the same
    // arena; the two share no storage.
    let arena_ptr: *mut TraceScratchArena = arena;
    let mut refused = false;
    unsafe {
        for_each_member_row(&*arena_ptr, |_, _, row, _| {
            let word = *row;
            if !refused && word & SPLIT_MARK != 0 && run_of(word) != 0 {
                refused = !push(&mut *arena_ptr, row);
            }
        })
    };
    if refused {
        arena.drop_the_work();
        return ControlFlow::Break(());
    }

    let entries = arena.recorded_edges().len();
    while let Some(popped) = arena.pop_work() {
        let mut index = run_of(unsafe { *popped.row }) as usize;
        while index < entries {
            let entry = unsafe { arena.recorded_edges().entry(index) };
            if entry & crate::cycle::recorded_edges::RUN != 0 {
                break;
            }

            if arena.inspect_position().is_break() {
                arena.drop_the_work();
                return ControlFlow::Break(());
            }

            let target = entry as *mut u32;
            let word = unsafe { *target };
            if shadow::color(word) == Color::PotentiallyUnreachable && word & SPLIT_MARK == 0 {
                unsafe { target.write(word | SPLIT_MARK) };
                if run_of(word) != 0 && !push(arena, target) {
                    arena.drop_the_work();
                    return ControlFlow::Break(());
                }
            }
            index += 1;
        }
    }
    ControlFlow::Continue(())
}

/// Queue a marked row whose run the closure reads; false where both
/// allocation paths refused.
fn push(arena: &mut TraceScratchArena, row: *mut u32) -> bool {
    arena.push_work(WorklistEntry {
        entity: std::ptr::null_mut(),
        row,
    })
}

/// Recolour every marked row `Unclassified`, its mark cleared: U, refused,
/// out of W for everything that follows.
///
/// # Safety
/// As [`close_over_the_record`].
pub(crate) unsafe fn refuse_the_marked(arena: &TraceScratchArena) {
    unsafe {
        for_each_member_row(arena, |_, _, row, _| {
            let word = *row;
            if word & SPLIT_MARK != 0 {
                row.write(shadow::compose(Color::Unclassified, run_of(word)));
            }
        })
    };
}

/// Clear every mark: the split dropped, W's potentially unreachable rows
/// whole again.
///
/// # Safety
/// As [`close_over_the_record`].
pub(crate) unsafe fn clear_the_marks(arena: &TraceScratchArena) {
    unsafe {
        for_each_member_row(arena, |_, _, row, _| {
            let word = *row;
            if word & SPLIT_MARK != 0 {
                row.write(word & !SPLIT_MARK);
            }
        })
    };
}

/// Mark every member the collector cannot free itself: one in a block that is
/// not a slotted block of the granting mutator's heap, one whose address
/// cannot be recovered, and one `collector_frees` finds ineligible — a
/// destructor, a dispose of its own, outside cells, weak references, a
/// category other than the heap's, a kind it does not free.
///
/// # Safety
/// As [`close_over_the_record`], under `mutator`'s grant.
pub(crate) unsafe fn mark_the_seeds(mutator: &MutatorRecord, arena: &TraceScratchArena) {
    let owner = mutator.owner_heap();
    unsafe {
        for_each_member_row(arena, |block, population, row, member| {
            let word = *row;
            if word & SPLIT_MARK != 0 {
                return;
            }

            let seed = population != Population::Slotted
                || owner.is_null()
                || crate::memory::heap::Heap::owner_of_the_block(block) != owner
                || member.is_none_or(|member| {
                    !crate::cycle::collector_frees::eligible(
                        member,
                        crate::cells::entity_kind(member),
                        crate::refcount::mutator_flags(member),
                    )
                });
            if seed {
                row.write(word | SPLIT_MARK);
            }
        })
    };
}

/// Whether any potentially unreachable row stands unmarked: C is not empty.
///
/// # Safety
/// As [`close_over_the_record`].
pub(crate) unsafe fn any_unmarked(arena: &TraceScratchArena) -> bool {
    let mut any = false;
    unsafe {
        for_each_member_row(arena, |_, _, row, _| {
            any |= *row & SPLIT_MARK == 0;
        })
    };
    any
}

/// The recorded edges into a marked row, from any source: what the counts of
/// S sum to at the owner's reading while C's drops into S stand held — the
/// edges between two members of S and C's edges into S, nothing outside W
/// naming S and U having no edge into it (the Sage's ruling, item 8(d)).
///
/// # Safety
/// As [`close_over_the_record`].
pub(crate) unsafe fn edges_into_the_marked(arena: &TraceScratchArena) -> usize {
    let record = arena.recorded_edges();
    (0..record.len())
        .map(|index| unsafe { record.entry(index) })
        .filter(|&entry| {
            entry & crate::cycle::recorded_edges::RUN == 0
                && is_marked(unsafe { *(entry as *const u32) })
        })
        .count()
}

/// What the split counts, for the runs (`split_counts`).
#[derive(Clone, Copy)]
pub(crate) enum Counted {
    /// A set split into C and S, C not empty.
    Split,
    /// U refused, its roots sent back to R.
    Requeued,
    /// U refused a second time, its roots read live.
    SecondRefusal,
    /// The split dropped: a closure stopped, or the preparation refused.
    Dropped,
    /// U kept as a seed of S at a second refusal, a member's address
    /// unreadable.
    Unreadable,
    /// A root of U read live at a second refusal, one a root.
    RootReadLiveAgain,
}

static COUNTS: [std::sync::atomic::AtomicUsize; 6] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 6];

/// Count one `what`.
pub(crate) fn note(what: Counted) {
    COUNTS[what as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// The split's counts since the process started, in [`Counted`]'s order.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn split_counts() -> [usize; 6] {
    std::array::from_fn(|what| COUNTS[what].load(std::sync::atomic::Ordering::Relaxed))
}
