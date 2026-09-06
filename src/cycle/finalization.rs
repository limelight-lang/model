//! Cycle finalization: the guard references a validated component takes, the
//! weak cells naming its members, and the destructors that run behind both.
//!
//! The exact validation answers about one component ([`crate::cycle::validation`]);
//! what this module adds is the writes that answer stands for and the user
//! code they make safe. Every
//! member takes a guard reference, so a release from inside a destructor stops
//! at the guard rather than at zero and no member starts ordinary teardown
//! mid-finalization. Then every weak cell naming a member is nulled, because a
//! weak load is the one channel that can hand a destructor a reference the
//! counts do not account for. Then each member that owes a `__destruct` runs
//! it once, and where any ran anywhere the exact validation is taken again
//! with the guard reference subtracted, because step 4 handed user code
//! `$this` (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation",
//! steps 2 to 5).
//!
//! # The order is the type's rather than the caller's
//!
//! [`Finalization::confirm`] performs the exact validation, the guards and the
//! invalidation as one act, and [`Finalization::seal`] is the only source of
//! an [`Invalidated`], which [`Invalidated::destructors`] takes by value. A
//! destructor of one component therefore cannot run over a member of
//! another whose cell still resolves, which is what step 3 asks for: a weak
//! cell naming one component is loadable from the destructor of another.
//!
//! Each value below consumes the one before it, and the chain is the protocol:
//! [`Finalization`] takes components, [`Invalidated`] is the reading that every
//! cell of every one of them is null, [`DestructorPass`] runs the pending
//! destructors, and [`Revalidation`] reads each component again. So the last
//! destructor of the commit runs before the first component is read again,
//! which is what step 5's "a destructor ran **anywhere**" asks for: one
//! reading for the whole commit rather than one per component.
//!
//! **User code runs at a second site, and it is inside the revalidation.** A
//! component read as externally referenced has its guards taken off through
//! the counted release, a member whose guard was its last reference dies
//! there, and that death drops the member's external children — whose own
//! `__destruct` bodies are user code again ([`release_guards`]). That code
//! **can** reach a member of a component this finalization has not read again:
//! step 4 may have published one, through a root or through a weak cell it
//! created, and this code can retain what it finds. Two things keep the
//! component whole. It cannot be freed under this teardown, every member of it
//! carrying its guard. And whatever this code did to its counts is read before
//! it is torn down, because its own revalidation is ordered after this
//! teardown — which is the adjacency and the reason for the borrow below
//! (`dev/DECISIONS.md`, "the revalidation of a component and its teardown are
//! adjacent"). A weak reference taken to such a member is nulled at that
//! member's free rather than by step 3, the clause that covers a cell a
//! destructor of step 4 creates (`rfc/model/gc/rc-cycle.md`, "Cycle
//! finalization and reclamation", the two consequences after step 6).
//!
//! # The revalidation of a component and its teardown are adjacent
//!
//! [`Revalidation::revalidate`] answers with a [`GuardedComponent`] that
//! borrows the revalidation, which refuses a second reading while one
//! component stands unfinished; that the sever and the free of that component
//! run inside the same window is the caller's, stated at
//! [`GuardedComponent::guards_released`]. The design's sequence is per
//! component and only its steps 2, 3 and 4 carry a batch-wide qualifier; what
//! the adjacency prevents is a cell created after the invalidation. A
//! destructor may take a weak reference to a member of a component not yet
//! read again, and the external children another component's teardown drops
//! run destructors of their own, one of which can load that cell and store
//! what it resolves into a root — leaving the sever to null the fields of an
//! entity a root holds (`dev/DECISIONS.md`, "the revalidation of a component
//! and its teardown are adjacent").
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
//! Four counters, three on the value the driver holds and one on the answer a
//! component's reading gives it. No pointer and no thread-local. **This
//! module's own writes ask the memory manager for nothing**: a guard is a
//! counter store, and `weak::table::remove` closes the gap a row leaves in
//! place. A debug build is the exception, and the allocation is the exact
//! validation's own premise check rather than this module's — taken at the
//! guards, and a second time only for a component the revalidation reads.
//! What outlives the values is
//! written into the members — the guard reference, until the counted release
//! this module makes over a component it reads as externally referenced or the
//! caller's sever takes it off (`PLAN.md` S36.5), and a nulled cell, which is
//! irrevocable by design (`rfc/model/weak-references.md`, "Death
//! notification").
//!
//! **What the paths through this module reach is another matter, and two of
//! them are unbounded.** A destructor is user code and may allocate whatever
//! it likes. And the counted release [`Revalidation::revalidate`] makes over a
//! component it reads as externally referenced is an ordinary decrement: one
//! the candidate gate admits writes a queue entry, which the narrow counter
//! twin of the guard would not, and a full write segment there draws a spare,
//! then the critical reserve, then appends to the overflow buffer whose own
//! bound ends the process (`crate::cycle::queue`, `register_candidate`). A
//! release that frees a member reaches the whole ordinary death path with it.
//!
//! No answer here is a refusal of the caller: the paths above answer with a
//! value or end the process, and neither reports memory it could not get. The
//! one "no" is the exact validation's,
//! answered before the first write, and a component it refuses keeps its
//! counts, its flags and its cells. What a driver does with either answer is
//! its own: `ExternallyReferenced` leaves the candidate bits standing for a
//! later trace, and what becomes of the other roots of a `ZeroCountMember`
//! component is open (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and
//! reclamation", step 1, which names `rfc`'s `dev/ALGORITHM-AUDIT.md`, issue
//! B1).
//!
//! **What the values do refuse is being abandoned.** Each one carries a drop
//! that fails the run, and each close matches the members it was handed
//! against the members the finalization guarded: a component left out of the
//! destructor pass would reach the sever with its destructor unrun and run it
//! at the free with every field already null, and a component left out of the
//! revalidation would be torn down on a reading taken before user code ran.

use std::marker::PhantomData;

use crate::cycle::validation::{ValidationResult, validate_component};
use crate::object::{Object, ll_entity_die, run_user_destructor};
use crate::refcount::{
    RcHeader, carries_a_class_word, ll_release, mutator_flags, mutator_guard_retain,
};
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
    /// **What this writes cannot be undone by the value that wrote it.** No
    /// type of this chain holds a member list, so the guards come off where a
    /// member list exists: the caller's sever (`PLAN.md` S36.5), or
    /// [`release_guards`] over a component the revalidation reads as
    /// externally referenced. An unwind out of this call reaches neither and
    /// strands the guards it has already written. Two debug assertions stand inside it: the exact
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
            taken: false,
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
/// [`Invalidated::destructors`] takes this by value, which is what keeps the
/// invalidation ahead of the first destructor of the finalization.
///
/// It carries the guards with it, so the pass is what discharges it and
/// nothing else: dropping it is the same stranding [`Finalization`]'s own drop
/// refuses, one call later.
#[must_use = "the guards outlive the seal, and the members still owe their destructors"]
pub(crate) struct Invalidated {
    members: usize,
    /// Whether [`Invalidated::destructors`] took this value's guards.
    taken: bool,
    _not_send: PhantomData<*mut ()>,
}

impl Invalidated {
    /// Members guarded, over every component the finalization confirmed.
    pub(crate) fn members(&self) -> usize {
        self.members
    }

    /// Open the destructor pass over the whole finalization, which is where
    /// user code runs (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and
    /// reclamation", step 4).
    pub(crate) fn destructors(mut self) -> DestructorPass {
        self.taken = true;
        DestructorPass {
            guarded: self.members,
            members_run: 0,
            any_destructor_ran: false,
            closed: false,
            _not_send: PhantomData,
        }
    }
}

impl Drop for Invalidated {
    /// The same refusal [`Finalization`]'s drop makes, on the same members:
    /// past the seal they still carry their guard references, and a value
    /// dropped instead of handed to the destructor pass strands every one of
    /// them.
    fn drop(&mut self) {
        if self.taken || self.members == 0 {
            return;
        }

        if std::thread::panicking() {
            return;
        }

        panic!("a sealed finalization was dropped instead of running its destructors");
    }
}

/// The destructor pass of one finalization: every member that owes a
/// `__destruct` runs it once, and no component is read again until all of them
/// have (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation",
/// step 4).
///
/// What it carries past [`DestructorPass::close`] is the one reading step 5
/// gates on — whether a destructor ran anywhere in the commit.
#[must_use = "the guards outlive the pass, and the members still owe their destructors"]
pub(crate) struct DestructorPass {
    /// Members the finalization guarded, and therefore the members this pass
    /// is owed.
    guarded: usize,
    /// Members handed to [`DestructorPass::run`] so far.
    members_run: usize,
    /// Whether any member of any component ran a destructor. One reading for
    /// the whole commit, so the skip at step 5 owes nothing to any reasoning
    /// about what a destructor in one component can reach in another.
    any_destructor_ran: bool,
    /// Whether [`DestructorPass::close`] took this value's answer.
    closed: bool,
    _not_send: PhantomData<*mut ()>,
}

impl DestructorPass {
    /// Run the pending `__destruct` of every member of one component.
    ///
    /// `members` is the component's whole membership, and the order inside it
    /// is unread: the guards and the nulled cells are whole over every
    /// component before the first destructor of any of them
    /// ([`Finalization::confirm`]).
    ///
    /// **A member carrying no class word is passed over**, and the gate is
    /// `refcount::carries_a_class_word` rather than a test for the object
    /// kind: the kinds it admits are the two that carry a class pointer at
    /// `+8`, which are the two `object::ll_entity_die` sends to
    /// `ll_object_die`. Change either set and change this one — nothing but
    /// this sentence holds them together, and a kind admitted here but not
    /// there would have its class word read out of a field that is not one.
    /// What runs is phase 1 alone (`object::run_user_destructor`) — the child
    /// releases and the free are the sever's, `PLAN.md` S36.5 — and "exactly
    /// once" is the header's `DESTRUCTOR_RAN`.
    ///
    /// **User code runs here.** It may store, release, allocate or resurrect,
    /// and what it cannot do is take a member's count to zero: the guard
    /// stands under every member of the finalization until the revalidation.
    ///
    /// # Safety
    /// Every member is an entity header of this thread's GC heap guarded by
    /// the finalization this pass came from, offered once, and the call runs on
    /// the owning thread.
    pub(crate) unsafe fn run(&mut self, members: &[*mut RcHeader]) {
        for &member in members {
            self.members_run += 1;
            if !carries_a_class_word(unsafe { mutator_flags(member) }) {
                continue;
            }

            if unsafe { run_user_destructor(member as *mut Object) } {
                self.any_destructor_ran = true;
            }
        }
    }

    /// Close the pass: every member has run whatever destructor it owed, and
    /// the components may be read again.
    ///
    /// **Refuses a pass whose members do not add up to the members the
    /// finalization guarded**, in every build rather than in a debug one. It
    /// is a count and not a set: a component left out is caught, and a
    /// component offered twice in place of another is not — the second offer
    /// runs no destructor, `DESTRUCTOR_RAN` standing, and the count still
    /// reaches the total. What the sum catches is a driver that stopped short;
    /// that its partition covers the commit once is the driver's own
    /// (`PLAN.md` S36.7).
    pub(crate) fn close(mut self) -> Revalidation {
        assert_eq!(
            self.members_run, self.guarded,
            "every guarded member runs its destructor before any component is read again"
        );

        self.closed = true;
        Revalidation {
            guarded: self.guarded,
            members_revalidated: 0,
            members_released: 0,
            any_destructor_ran: self.any_destructor_ran,
            closed: false,
            _not_send: PhantomData,
        }
    }
}

impl Drop for DestructorPass {
    /// A pass abandoned mid-commit leaves what [`Finalization::confirm`]
    /// wrote standing, and the members it did reach have their destructors
    /// behind them — so it fails the run for the reason [`Finalization`]'s
    /// own drop does, and silently while another panic unwinds.
    fn drop(&mut self) {
        if self.closed || self.guarded == 0 {
            return;
        }

        if std::thread::panicking() {
            return;
        }

        panic!("a destructor pass holding guarded members was dropped instead of closed");
    }
}

/// Step 5: each component read again against its members' current fields, with
/// the one guard reference per member subtracted.
///
/// Without it the guards would leave every component externally referenced and
/// nothing would ever be freed; with it a component a destructor left a
/// reference to reads as externally referenced, and its members survive except
/// where the guard was the last reference one of them had
/// (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 5).
#[must_use = "the guards outlive the pass, and no component is torn down unread"]
pub(crate) struct Revalidation {
    /// Members the finalization guarded, and therefore the members this
    /// revalidation is owed and whose guards it must see off.
    guarded: usize,
    /// Members handed to [`Revalidation::revalidate`] so far.
    members_revalidated: usize,
    /// Members whose guard reference has come off, over both answers.
    members_released: usize,
    any_destructor_ran: bool,
    /// Whether [`Revalidation::close`] took this value's answer.
    closed: bool,
    _not_send: PhantomData<*mut ()>,
}

impl Revalidation {
    /// Read one component again, and where a destructor left a reference to it
    /// outside itself, give every member its true count back.
    ///
    /// `members` is the membership [`Finalization::confirm`] took, and **the
    /// slice is sorted in place** again by the exact validation.
    ///
    /// [`Revalidated::Unreachable`] carries the component's guards, which the
    /// sever, the free and [`GuardedComponent::guards_released`] take off
    /// (`PLAN.md` S36.5); [`Revalidated::ExternallyReferenced`] has taken them
    /// off already, so its component is the driver's to forget — **and the
    /// slice may name a freed entity when it does**, a member whose guard was
    /// its last reference having died in the release.
    ///
    /// **A commit no destructor ran in is not read again at all**, the answer
    /// being the one the exact validation already gave (step 5). What makes
    /// that sound under the per-component order is an induction rather than
    /// the absence of user code: an external child of an earlier component's
    /// teardown may still own a `__destruct`, and running it does not set this
    /// flag. But with no member destructor nothing published a member of any
    /// component, so no such child can name one, so none can publish one
    /// either — and the only channel left, a counted reference from outside,
    /// is what the exact validation read. The induction is also where the
    /// skip stops being sound: anything else that can publish a member between
    /// step 3 and step 5 breaks it. The slice is sorted here all the same, so
    /// the answer carries the same order on both paths.
    ///
    /// # Safety
    /// As [`Finalization::confirm`], and every member is one this finalization
    /// guarded, offered once.
    pub(crate) unsafe fn revalidate(&mut self, members: &mut [*mut RcHeader]) -> Revalidated<'_> {
        self.members_revalidated += members.len();
        if !self.any_destructor_ran {
            members.sort_unstable();
            return Revalidated::Unreachable(GuardedComponent::over(members.len(), self));
        }

        match unsafe { validate_component(members, 1) } {
            ValidationResult::Unreachable => {
                Revalidated::Unreachable(GuardedComponent::over(members.len(), self))
            }
            ValidationResult::ExternallyReferenced => {
                unsafe { release_guards(members) };
                self.members_released += members.len();
                Revalidated::ExternallyReferenced
            }
            // The exact validation answers this from a pass it takes only
            // while no guard is outstanding, and every member here carries
            // one.
            ValidationResult::ZeroCountMember => unreachable!(
                "a guarded member cannot read zero: the guard is a reference of its own"
            ),
        }
    }

    /// Close the finalization: every component was read again, and every guard
    /// the confirm wrote has come off.
    ///
    /// **Refuses in every build** a revalidation whose members do not add up
    /// to the members the finalization guarded: a component never read again
    /// would be torn down on a reading taken before user code ran. It is the
    /// same count [`DestructorPass::close`] makes and it sees the same half —
    /// a driver that stopped short, and not one that offered a component
    /// twice. That every guard has come off is [`GuardedComponent`]'s own
    /// refusal rather than this one.
    pub(crate) fn close(mut self) {
        assert_eq!(
            self.members_revalidated, self.guarded,
            "every guarded member is read again before the finalization ends"
        );

        self.closed = true;
    }
}

impl Drop for Revalidation {
    /// The refusal the two values before it make, over whatever is left: a
    /// component unread keeps its guards, and nothing else takes them off.
    fn drop(&mut self) {
        if self.closed || self.members_released == self.guarded {
            return;
        }

        if std::thread::panicking() {
            return;
        }

        panic!("a revalidation holding guarded members was dropped instead of closed");
    }
}

/// What [`Revalidation::revalidate`] read about one component.
#[must_use = "an unread answer tears down a component a destructor may have resurrected"]
pub(crate) enum Revalidated<'a> {
    /// No reference to any member exists outside the component, so the
    /// teardown proceeds. The guards are still on, and the value names how
    /// many.
    Unreachable(GuardedComponent<'a>),
    /// A destructor left a reference to a member outside the component. Every
    /// guard is off and every surviving member carries its true count, with
    /// its destructor already behind it; the weak cells nulled at step 3 stay
    /// null, which is where this design parts from PHP
    /// (`rfc/model/weak-references.md`, "Death notification").
    ///
    /// **A member the guard was the last reference of is freed here**, so the
    /// slice the caller passed can name an entity whose slot is back with the
    /// allocator. Nothing may read it again.
    ExternallyReferenced,
}

/// One component the revalidation kept, holding the revalidation until its
/// guards come off.
///
/// The borrow refuses a second reading while this one stands, which is half of
/// the order the design asks for; the other half is the caller's, and
/// [`GuardedComponent::guards_released`] is where it is stated
/// (`dev/DECISIONS.md`, "the revalidation of a component and its teardown are
/// adjacent").
#[must_use = "the guards are still on, and nothing but the release takes them off"]
pub(crate) struct GuardedComponent<'a> {
    revalidation: &'a mut Revalidation,
    members: usize,
    released: bool,
}

impl<'a> GuardedComponent<'a> {
    fn over(members: usize, revalidation: &'a mut Revalidation) -> Self {
        Self {
            revalidation,
            members,
            released: false,
        }
    }

    /// Members of this component, each carrying its guard reference.
    pub(crate) fn members(&self) -> usize {
        self.members
    }

    /// The caller has severed the component, freed what reached zero and taken
    /// the guard reference off every member, which ends this component's
    /// finalization.
    ///
    /// The sever and the free are `PLAN.md` S36.5's — they walk the same
    /// members, null the internal edges and let each member reaching zero die
    /// through the ordinary death path. This is the statement that it
    /// happened, and the only thing that lets the value go quietly.
    ///
    /// # Safety
    /// The teardown of this component is complete before this returns, and no
    /// other component of the finalization is read again before it. A caller
    /// that states the release and defers the teardown past the next
    /// [`Revalidation::revalidate`] reopens the window the adjacency closes: a
    /// destructor run by the deferred teardown's external children can store a
    /// member of the next component in a root, and that component is then
    /// severed under the root (`dev/DECISIONS.md`, "the revalidation of a
    /// component and its teardown are adjacent"). Nothing here can check it —
    /// the value holds a count and no member identity.
    pub(crate) unsafe fn guards_released(mut self) {
        self.revalidation.members_released += self.members;
        self.released = true;
    }
}

impl Drop for GuardedComponent<'_> {
    /// The refusal [`Finalization`]'s drop makes, one component wide: the
    /// members carry their guards, this value is what says they came off, and
    /// nothing here can take them off on its own — the sever that owes the
    /// walk is the caller's.
    fn drop(&mut self) {
        if self.released || self.members == 0 {
            return;
        }

        if std::thread::panicking() {
            return;
        }

        panic!("a component read as unreachable was dropped with its guards on");
    }
}

/// Take the teardown guard off every member through the counted release, and
/// let a member whose guard was its last reference die the ordinary death.
///
/// **The counted release rather than `refcount::mutator_unguard_release`**:
/// past its finalization a member is an ordinary entity again, so a decrement
/// the candidate gate admits registers it for a later trace, and a member
/// nothing else names dies here rather than standing at zero. What that costs
/// over the narrow twin is a flags load and a queue entry per member the gate
/// admits (`crate::cycle::queue`).
///
/// A member still carrying its guard cannot be freed by a death this loop
/// starts, its count being at least the guard, so no iteration reads a member
/// an earlier one freed. A member already past its own release can be, and
/// the caller's slice names it afterwards.
///
/// **User code runs here**, once a member dies: its teardown drops the
/// external children it held, and their destructors are the mutator's own
/// (`object::ll_default_dispose`, phase 2).
///
/// # Safety
/// Every member is a live entity of this thread's GC heap carrying exactly one
/// guard reference, each named once, and the call runs on the owning thread.
pub(crate) unsafe fn release_guards(members: &[*mut RcHeader]) {
    for &member in members {
        if unsafe { ll_release(member) } {
            unsafe { ll_entity_die(member) };
        }
    }
}

#[cfg(test)]
mod tests;
