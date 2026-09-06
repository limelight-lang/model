//! Cycle finalization: the guard references a validated component takes, and
//! the weak cells naming its members, nulled before any user code runs.
//!
//! The exact validation answers about one component ([`crate::cycle::validation`]);
//! what this module adds is the two writes that answer stands for. Every
//! member takes a guard reference, so a release from inside a destructor stops
//! at the guard rather than at zero and no member starts ordinary teardown
//! mid-finalization. Then every weak cell naming a member is nulled, because a
//! weak load is the one channel that can hand a destructor a reference the
//! counts do not account for (`rfc/model/gc/rc-cycle.md`, "Cycle finalization
//! and reclamation", steps 2 and 3).
//!
//! # The order is the type's rather than the caller's
//!
//! [`Finalization::confirm`] performs the exact validation, the guards and the
//! invalidation as one act, and [`Finalization::seal`] is the only source of
//! an [`Invalidated`], which the destructor pass takes by value (`PLAN.md`
//! S36.4). A destructor of one component therefore cannot run over a member of
//! another whose cell still resolves, which is what step 3 asks for: a weak
//! cell naming one component is loadable from the destructor of another.
//!
//! Nothing between the guards and the nulling runs user code: the exact
//! validation reads fields, `refcount::mutator_guard_retain` writes the
//! counter half of a header and `weak::notify_death` the flags half, and
//! neither the weak table nor a nulled cell is read by a later exact
//! validation — `cells::trace_cells` leaves a weak cell's target uncounted. So
//! the order fixes the state the first destructor meets rather than an
//! ordering another thread could observe.
//!
//! Both passes run on the owning thread after the trace token is released
//! (`rfc/model/gc/rc-cycle.md`, "Concurrency"). That releases the right to
//! trace and not every other thread: a collector's byte store into the flags
//! half may still be concurrent, which is why each access here stays narrow
//! and goes through `refcount`'s accessors rather than through a header word.
//!
//! **What the type does not establish is that one commit uses one
//! finalization.** A driver that sealed per component and ran that component's
//! destructors before validating the next would satisfy every obligation here
//! and still interleave the two steps the design orders. The step that opens
//! exactly one finalization over a commit is `PLAN.md` S36.7's driver.
//!
//! # The unit is the caller's
//!
//! [`Finalization::confirm`] takes what the exact validation takes: one
//! component's whole membership, as a slice it sorts. A batch validated as its
//! union has the same shape, and which of the two a driver hands over is
//! `PLAN.md` S36.7's to choose — the trace proposes rows and nothing in the
//! crate partitions them into components yet.
//!
//! **A slice is what only one of the two production paths has.** The pressure
//! path's harvest is a list already, and what stands between it and this
//! signature is a mutable view: `cycle::members::StandingMembers` answers a
//! shared slice, and a sort would cost the order it documents. The path off
//! the poll holds its rows through the teardown and derives no member list at
//! all (`rfc/model/gc/rc-cycle.md`, "Concurrency"), so what serves it is
//! unbuilt and unowned — `PLAN.md` S36.7 is where both are answered.
//!
//! # What it holds, and what it can refuse
//!
//! A counter on the driver's frame, no pointer and no thread-local. The
//! memory manager is asked for nothing on this path: a guard is a counter
//! store, and `weak::table::remove` closes the gap a row leaves in place. A
//! debug build is the exception, and the allocation is the exact validation's
//! own premise check rather than this module's. What outlives the value is
//! written into the members — the guard reference until the counted release of
//! `PLAN.md` S36.5, and a nulled cell, which is irrevocable by design
//! (`rfc/model/weak-references.md`, "Death notification").
//!
//! There is no refusal of its own. The one "no" is the exact validation's,
//! answered before the first write, and a component it refuses keeps its
//! counts, its flags and its cells. What a driver does with either answer is
//! its own: `ExternallyReferenced` leaves the candidate bits standing for a
//! later trace, and what becomes of the other roots of a `ZeroCountMember`
//! component is open (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and
//! reclamation", step 1, which names `rfc`'s `dev/ALGORITHM-AUDIT.md`, issue
//! B1).

use std::marker::PhantomData;

use crate::cycle::validation::{ValidationResult, validate_component};
use crate::refcount::{RcHeader, mutator_guard_retain};
use crate::weak;

/// One thread's cycle finalization, open from [`Finalization::begin`] until
/// the seal that admits the destructor pass.
#[must_use = "a finalization nothing is confirmed into validates no component"]
pub(crate) struct Finalization {
    /// Members guarded so far, counted by the guard loop itself so that the
    /// figure cannot outlive the write it stands for.
    members: usize,
    /// Whether [`Finalization::seal`] has taken this value's answer, which is
    /// what the drop below distinguishes from a finalization abandoned with
    /// guards outstanding.
    sealed: bool,
    /// The counts and the cells are the owning thread's to write, and the
    /// exact validation reads fields no other thread may read
    /// ([`validate_component`]).
    _not_send: PhantomData<*mut ()>,
}

impl Finalization {
    /// Open a finalization holding no component.
    pub(crate) fn begin() -> Self {
        Self {
            members: 0,
            sealed: false,
            _not_send: PhantomData,
        }
    }

    /// Validate one candidate component and, where the exact validation confirms it,
    /// guard every member and null every weak cell naming one.
    ///
    /// `members` is the component's whole membership, each member once, and
    /// **the slice is sorted in place** by the exact validation, so the caller's own
    /// order is gone when this returns ([`validate_component`]).
    ///
    /// The answer is the exact validation's, unchanged. On
    /// [`ValidationResult::Unreachable`] the component belongs to this
    /// finalization and its members carry a guard reference each; on either
    /// other answer nothing is written at all.
    ///
    /// **A member confirmed once must not be offered again.** The exact validation
    /// is given `guard_refs_per_member` of zero here, so a member that already
    /// carries a guard reference reads as externally referenced, and a second
    /// guard on it would be released once (`PLAN.md` S36.5).
    ///
    /// **What this writes cannot be undone by this module.** Past a confirmed
    /// component the guards come off through the counted release of `PLAN.md`
    /// S36.5 and nowhere else, so an unwind out of this call strands the guards
    /// it has already written. Two debug assertions stand inside it: the exact
    /// validation's own, which raise before the first guard, and
    /// `weak::notify_death`'s, on a member whose gate bit stands with no table
    /// row, which raises after all of them.
    ///
    /// # Safety
    /// As [`validate_component`]: every member is an entity header of this
    /// thread's GC heap whose slot is still its own, and the call runs on the
    /// owning thread with no mutator beside it. The invalidation reads the
    /// same headers under the same rule.
    pub(crate) unsafe fn confirm(&mut self, members: &mut [*mut RcHeader]) -> ValidationResult {
        let result = unsafe { validate_component(members, 0) };
        if result != ValidationResult::Unreachable {
            return result;
        }

        for &member in members.iter() {
            unsafe { mutator_guard_retain(member) };
            self.members += 1;
        }

        unsafe { weak::notify_members(members) };
        result
    }

    /// Close the finalization: no component joins it after this, and the
    /// answer is what the destructor pass takes (`PLAN.md` S36.4).
    pub(crate) fn seal(mut self) -> Invalidated {
        self.sealed = true;
        Invalidated {
            members: self.members,
            released: false,
            _not_send: PhantomData,
        }
    }
}

impl Drop for Finalization {
    /// A finalization abandoned with members in it fails loudly — the process
    /// where `panic = "abort"` is set, the run otherwise — because nothing can
    /// undo what [`Finalization::confirm`] wrote: the value holds no member
    /// list, so the guards cannot be released here, and a nulled cell never
    /// resolves again. What such a member costs while the process lives is
    /// every later collection — its guard reads as a reference from outside,
    /// so no trace can propose it again.
    fn drop(&mut self) {
        if self.sealed || self.members == 0 {
            return;
        }

        // Silent while another panic is unwinding, as
        // `crate::cycle::queue::InFlightBatch`'s drop is and for the same
        // reason: this one would be the second panic, and it would end the
        // process without the message that says what went wrong. The guards
        // such an unwind leaves are stranded all the same, and the message
        // that carries the reason is the first panic's.
        if std::thread::panicking() {
            return;
        }

        panic!("a finalization holding guarded members was dropped instead of sealed");
    }
}

/// At the seal that produced it, every weak cell naming a member of the
/// finalization read null and every member carried its guard reference.
///
/// It is a reading of that instant rather than an invariant the value keeps:
/// the destructor pass holds it while user code runs, and a destructor may
/// create a weak reference to a member whose gate bit the invalidation cleared
/// (`rfc/model/weak-references.md`, "Death notification"). Such a cell is
/// nulled by the free-time notification instead, which is `PLAN.md` S36.5's.
///
/// The destructor pass takes this by value, which is what keeps the
/// invalidation ahead of the first destructor of the finalization.
///
/// It carries the guards with it, so it is discharged by
/// [`Invalidated::guards_released`] and by nothing else: dropping it is the
/// same stranding [`Finalization`]'s own drop refuses, one call later.
#[must_use = "the guards outlive the seal, and nothing but the release takes them off"]
pub(crate) struct Invalidated {
    members: usize,
    released: bool,
    _not_send: PhantomData<*mut ()>,
}

impl Invalidated {
    /// Members guarded, over every component the finalization confirmed.
    pub(crate) fn members(&self) -> usize {
        self.members
    }

    /// The caller has taken the guard reference off every member, which ends
    /// the finalization.
    ///
    /// The release itself is `PLAN.md` S36.5's — it walks the same members,
    /// severs the internal edges and lets each member reaching zero die
    /// through the ordinary death path. This is the statement that it
    /// happened, and the only thing that lets the value go quietly.
    pub(crate) fn guards_released(mut self) {
        self.released = true;
    }
}

impl Drop for Invalidated {
    /// The same refusal [`Finalization`]'s drop makes, on the same members:
    /// past the seal they still carry their guard references, and a value
    /// dropped instead of released strands every one of them.
    fn drop(&mut self) {
        if self.released || self.members == 0 {
            return;
        }

        if std::thread::panicking() {
            return;
        }

        panic!("a sealed finalization was dropped instead of released");
    }
}

#[cfg(test)]
mod tests;
