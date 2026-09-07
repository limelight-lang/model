//! The membership of a commit: which entities its teardown is about, and
//! whether the child an edge names is one of them.
//!
//! Two forms, because the two paths hold different things once the trace is
//! over (`rfc/model/gc/rc-cycle.md`, "When the arena goes back depends on why
//! the collection ran"). A collection an allocation failure started has
//! already given its blocks back and holds the list its sweep harvested
//! ([`crate::cycle::members`]); a collection off the poll keeps its rows and
//! derives no list at all, so its membership is the rows themselves.
//!
//! Everything downstream of the trace asks the same three questions of a
//! membership — how many members, each member once, and is this child one of
//! them — so the two forms answer those three and the exact validation, the
//! finalization and the teardown are written once
//! ([`crate::cycle::validation`], [`crate::cycle::finalization`],
//! [`crate::cycle::reclamation`]).
//!
//! # What it owns
//!
//! Nothing, on either arm. The listed form borrows the caller's slice, which
//! it sorts once at construction, because a membership test over it is a
//! binary search. The row form holds the head of the collection's touched list
//! — the arena's own memory, and a raw pointer rather than a borrow so that a
//! reader holding a membership can still hand the arena to a teardown that
//! needs it ([`crate::cycle::reclamation::reclaim`]).
//!
//! # Why the row form counts once, at construction
//!
//! Its count is a walk, and three readers ask for it: the sum the exact
//! validation compares, the counter the finalization chain balances its
//! commit against, and the teardown's own agreement check. Counting at
//! construction also puts the one failure this form has where a caller can
//! act on it: a row whose address cannot be recovered is a disagreement
//! between the array and a retained block's survivor list
//! ([`crate::cycle::row::entity_at`]), and a membership that cannot name every
//! member is refused whole rather than handed over short. A set the scan left
//! is closed under its in-edges, so a part of one is not a set that can be
//! torn down (`crate::cycle::members`).

use crate::cycle::arena::find_initialized_row;
use crate::cycle::row::{self, EdgeTarget, entity_at, resolve_edge_target};
use crate::cycle::shadow::{self, Color, RowArray};
use crate::refcount::RcHeader;

/// The entities one commit reads, in the form the path that produced them
/// holds.
#[derive(Clone, Copy)]
pub(crate) enum Membership<'a> {
    /// A list of members, sorted by address: the pressure path's harvest, and
    /// what every test of the teardown builds by hand.
    Listed(&'a [*mut RcHeader]),
    /// The rows a collection off the poll kept, which name their entities
    /// through the block each array was written for.
    Rows {
        /// The newest array of the arena's touched list; every array names the
        /// next.
        touched: *mut RowArray,
        /// Rows the scan left [`Color::PotentiallyUnreachable`], counted at
        /// construction.
        members: usize,
    },
}

impl<'a> Membership<'a> {
    /// Take a list of members, sorting it in place.
    ///
    /// **The caller's own order is gone when this returns**, so nothing may be
    /// held parallel to the slice by index: an array indexed after this names
    /// a different member (`rfc/model/gc/cycle/questions.md`, Y6).
    pub(crate) fn listed(members: &'a mut [*mut RcHeader]) -> Self {
        members.sort_unstable();
        Self::Listed(members)
    }

    /// Take the rows of a scanned collection as its membership, or `None`
    /// where one of them names no entity.
    ///
    /// `touched` is the arena's touched list
    /// ([`crate::cycle::arena::TraceScratchArena::touched_head`]) and this
    /// membership is valid for exactly as long as that list is: until the
    /// close sweeps it. An empty list is a membership of no members, which is
    /// the answer for a trace that met nothing.
    ///
    /// # Safety
    /// Every array of `touched` belongs to a collection whose scan has run,
    /// the blocks they were written for are live, and the arena that owns them
    /// is not swept before this value dies.
    pub(crate) unsafe fn rows(touched: *mut RowArray) -> Option<Self> {
        let mut members = 0;
        let placed = unsafe { walk_unreachable_rows(touched, |_| members += 1) };
        placed.then_some(Self::Rows { touched, members })
    }

    /// Members in this commit.
    pub(crate) fn len(&self) -> usize {
        match *self {
            Self::Listed(members) => members.len(),
            Self::Rows { members, .. } => members,
        }
    }

    /// Visit every member once, in address order on the listed form and in the
    /// touched list's own order on the row form. Neither order is a contract.
    ///
    /// # Safety
    /// As the constructor that made this value: for the row form the arena is
    /// unswept and its blocks are live.
    pub(crate) unsafe fn for_each(&self, mut visit: impl FnMut(*mut RcHeader)) {
        match *self {
            Self::Listed(members) => {
                for &member in members {
                    visit(member);
                }
            }
            Self::Rows { touched, .. } => {
                let placed = unsafe { walk_unreachable_rows(touched, visit) };
                debug_assert!(
                    placed,
                    "a row that named an entity at construction names none now"
                );
            }
        }
    }

    /// Whether `entity` is a member of this commit.
    ///
    /// The row form answers it out of the entity's own row, which costs one
    /// dispatch on its block against the binary search the listed form makes.
    /// An entity outside the GC heap, and one whose block this collection
    /// never touched, are both answered false without a row being read
    /// ([`resolve_edge_target`]).
    ///
    /// # Safety
    /// As [`Membership::for_each`], and `entity` is an entity header this
    /// thread may read.
    pub(crate) unsafe fn contains(&self, entity: *mut RcHeader) -> bool {
        match *self {
            Self::Listed(members) => members.binary_search(&entity).is_ok(),
            Self::Rows { .. } => {
                let EdgeTarget::Tracked(key) = (unsafe { resolve_edge_target(entity) }) else {
                    return false;
                };

                match unsafe { find_initialized_row(key) } {
                    Some(row) => shadow::color(unsafe { *row }) == Color::PotentiallyUnreachable,
                    None => false,
                }
            }
        }
    }
}

/// Visit the entity behind every row the scan left
/// [`Color::PotentiallyUnreachable`], over the whole touched list, and answer
/// **false where a row names no entity**.
///
/// The walk stops at that row rather than passing over it: a retained block
/// whose survivor list no longer holds the row's position is a disagreement
/// with the array, and the reading it belongs to is given up whole
/// ([`crate::cycle::row::entity_at`]).
///
/// # Safety
/// As [`Membership::rows`].
unsafe fn walk_unreachable_rows(
    touched: *mut RowArray,
    mut visit: impl FnMut(*mut RcHeader),
) -> bool {
    let mut array = touched;
    while !array.is_null() {
        let block = unsafe { (*array).block };
        let population = unsafe { (*array).population };
        let mut placed = true;
        unsafe {
            row::for_each_unreachable(array, block, population, |index| {
                match entity_at(block, population, index) {
                    Some(entity) => {
                        visit(entity);
                        true
                    }
                    None => {
                        placed = false;
                        false
                    }
                }
            })
        };

        if !placed {
            return false;
        }

        array = unsafe { (*array).next };
    }

    true
}

#[cfg(test)]
mod tests;
