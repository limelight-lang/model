//! The maturation stamps a collector's completed batch writes itself, under
//! its grant and before the release (`dev/DECISIONS.md`, "the collector writes
//! the maturation stamps itself, and the live list goes"; `dev/plans/S67.md`,
//! S67.9, revision 3's (2′) and (3′)).
//!
//! # What is stamped
//!
//! A batch whose trace completed has read a live core, and a stamp on its
//! members is what the next batch's mark stops at (`crate::cycle::mark`). Not
//! every live row is stamped: only those of the arrays first touched in the
//! mark's final drain, the blocks the walk entered past a held registered
//! target that no pass brought to zero — on a web load, mostly the state. A
//! live request a batch boundary cut is walked in the first regions and the
//! passes and stays unstamped, so that once it dies the next batch walks it
//! rather than pruning at it for the rest of the epoch (the Critic of
//! 2026-09-30, round 2). The rule is by block, and it is not exact: a request
//! entered through a registered member the batch does not hold as a root, its
//! interior in blocks of its own, is walked in the final drain and stamped,
//! and so is garbage a batch boundary read live behind such a member; either
//! waits for the turn (`dev/plans/S67.md`, S67.9, the Critic of the build of
//! step (f), findings 1 and 3). The stamp is `{e, 1}` in the epoch the batch's arena read, by
//! [`crate::refcount::stamp_as_read_live`]'s rule: at the traversal threshold
//! of one it prunes what a stamp per strongly connected component would
//! (`crate::cycle::maturation`), so no component is computed and nothing is
//! descended.
//!
//! The arrays are walked oldest first, so that a walk cut short stamps the
//! blocks the final drain entered first, the state's entry points. The touched
//! list is newest first, so the final drain's run of it is reversed in place
//! for the walk and put back after it, on the unwind too ([`Reversed`]).
//!
//! # Why the collector may write byte 6
//!
//! Byte 6 is written only by the holder of the mutator's token: the owner
//! under its own claim, a collector under its grant. Every side reads and
//! writes it as a byte-wide relaxed atomic, the wide header writes touch only
//! unpublished slots, and every death, block and run the mutator would give
//! back is withheld while the grant stands (`crate::cycle::deferred_slot_reuse`),
//! so a stamp lands in the thread's own memory however the member fared since
//! its row was met. A member whose slot no longer reads live is skipped.
//!
//! # What a recall costs
//!
//! The walk reads the recall every `RECALL_STRIDE` rows and stops at the
//! stop level alone, as the scan does, keeping what it wrote: stamps on live
//! rows of a completed trace are as sound as a whole walk's, and keeping them
//! puts nothing between the reading and the release. A wind-down asks for
//! what the walk already does, an end on posts: a batch that dropped its
//! stamps at one would leave the state unstamped for the next batch to walk
//! whole again, which is what raised the wind-down.

use std::ops::ControlFlow;

use crate::cycle::arena::TraceScratchArena;
use crate::cycle::row;
use crate::cycle::shadow::RowArray;
use crate::refcount::{SlotState, slot_state, stamp_as_read_live};

/// Stamp `{e, 1}`, `e` the arena's epoch, on every entity a completed trace's
/// scan left live in an array first touched in the mark's final drain, the
/// oldest array first, reading the recall every stride of rows: `Break` where
/// it stood, the stamps written before the reading kept. Nothing where no
/// final drain ran.
///
/// # Safety
/// The trace completed on this thread under the traced mutator's token, which
/// the thread still holds, and its rows still stand: after the scan and before
/// the arena's reset.
pub(crate) unsafe fn stamp_the_final_drain(arena: &mut TraceScratchArena) -> ControlFlow<()> {
    let Some(stop) = arena.final_drain_from() else {
        return ControlFlow::Continue(());
    };

    #[cfg(test)]
    let (from, mut stamped, mut raised_at) = (std::time::Instant::now(), 0, None);
    let epoch = arena.epoch();
    let run = unsafe { Reversed::new(arena.touched_head(), stop) };
    let mut array = run.head;
    let mut walked = ControlFlow::Continue(());
    while array != stop && walked.is_continue() {
        let (block, population) = unsafe { ((*array).block, (*array).population) };
        walked = unsafe {
            row::for_each_live_met(array, block, population, |index| {
                if arena.inspect_position().is_break() {
                    return ControlFlow::Break(());
                }

                // A row whose address cannot be recovered is left out, as the
                // commit's own walk leaves it (`crate::cycle::maturation`).
                let Some(entity) = row::entity_at(block, population, index) else {
                    return ControlFlow::Continue(());
                };
                if slot_state(entity) != SlotState::Live {
                    return ControlFlow::Continue(());
                }

                stamp_as_read_live(entity, epoch);
                #[cfg(test)]
                {
                    stamped += 1;
                    if testing::note_stamped(entity) {
                        raised_at = Some(arena.positions_inspected());
                    }
                }
                ControlFlow::Continue(())
            })
        };
        array = unsafe { (*array).next };
    }

    drop(run);
    #[cfg(test)]
    {
        testing::note_stamping(stamped, from.elapsed());
        if let Some(raised_at) = raised_at {
            crate::cycle::worker::testing::note_positions_after_the_hook(
                arena.positions_inspected() - raised_at,
            );
        }
    }
    walked
}

/// A run of the touched list, from its head up to the array `stop`, reversed
/// in place for as long as the guard lives: its oldest array first, the last
/// naming `stop`. The drop reverses it back, so the list is whole again for
/// the arena's sweep however the walk ended.
struct Reversed {
    head: *mut RowArray,
    stop: *mut RowArray,
}

impl Reversed {
    /// # Safety
    /// `head` is the touched list's head and `stop` an array of that list or
    /// null, and nothing walks the list until the guard drops.
    unsafe fn new(head: *mut RowArray, stop: *mut RowArray) -> Self {
        Self {
            head: unsafe { reverse(head, stop) },
            stop,
        }
    }
}

impl Drop for Reversed {
    fn drop(&mut self) {
        unsafe { reverse(self.head, self.stop) };
    }
}

/// Reverse the run from `head` up to `stop` in place and answer its new head;
/// its old head then names `stop`. A second call on the answer puts it back.
///
/// # Safety
/// As [`Reversed::new`].
unsafe fn reverse(head: *mut RowArray, stop: *mut RowArray) -> *mut RowArray {
    let mut previous = stop;
    let mut array = head;
    while array != stop {
        let next = unsafe { (*array).next };
        unsafe { (*array).next = previous };
        previous = array;
        array = next;
    }

    previous
}

#[cfg(test)]
pub(crate) mod testing;
