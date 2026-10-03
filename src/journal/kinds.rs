//! The event vocabulary: what a record's `kind` means, what its three
//! payload words carry, and the two gates a site passes through
//! (`dev/design/debug-modes.md` §9.5).
//!
//! **A kind exists here only once a site writes it.** The on-demand set
//! §9.5 names — retain and release, store-barrier publishes, buffer chunk
//! allocation and free — has no number yet, and takes one when it takes a
//! site: a number handed out in advance is an arm with no producer, which
//! is what leaves a reader unable to tell "this never happened" from "this
//! was never built".
//!
//! **Two gates, and they answer different questions.** The
//! `debug-journal` feature decides whether the site is compiled at all,
//! so a runtime that does not want the journal carries no branch on its
//! allocation and death paths (§9.6). The enabled mask decides whether a
//! compiled site writes, so an investigator can leave the high-volume
//! kinds off and keep a ring's thousand records spent on what is being
//! hunted — a ring of K records says nothing about a window in which one
//! kind wrote K records by itself.
//!
//! Both gates are in [`journal_event!`](crate::journal::kinds::journal_event),
//! which is how a site is written.
//! The macro evaluates its payload arguments **after** the mask test, so
//! a disabled kind costs the load and the branch and nothing else: some
//! sites read a header or a block field to fill their words, and a
//! disabled site must not pay for a reading nobody will look at.

use std::sync::atomic::{AtomicU64, Ordering};

/// Birth of an entity: `subject` is its address, `a` its
/// [`crate::refcount::EntityKind`] as a number, `b` its
/// [`crate::refcount::MemoryCategory`]. Written where the header is
/// published, which is the one entry point every factory in the crate
/// goes through, `refcount::publish_header`.
pub const KIND_ENTITY_BIRTH: u32 = 1;

/// Death of an entity: `subject` is its address, `a` its entity kind, `b`
/// unused. Written at each kind's own teardown body rather than at the kind
/// switch above them, because an object reaches teardown by two entry points
/// and a nested array by neither.
pub const KIND_ENTITY_DEATH: u32 = 2;

/// A request arena's reset begins: `subject` is the arena's address, `a`
/// and `b` unused. The pair with [`KIND_ARENA_RESET_END`] brackets every
/// death the reset causes, which is what makes "did this reset free it"
/// answerable from one ring.
pub const KIND_ARENA_RESET_BEGIN: u32 = 3;

/// A request arena's reset ends: `subject` is the arena's address, `a`
/// the number of survivors promoted out of it, `b` the number of blocks
/// retained for them.
pub const KIND_ARENA_RESET_END: u32 = 4;

/// A block handed out by the pool: `subject` is the block's address, `a`
/// and `b` unused. The block's kind is not here — the pool hands out
/// blocks of no kind, and the consumer stamps one afterwards.
pub const KIND_BLOCK_COMMISSIONED: u32 = 5;

/// A block returned to the pool: `subject` is the block's address, `a`
/// the kind it arrived with, `b` unused.
///
/// **This is also §9.5's third block event.** A block leaves the set the
/// entity walk reaches by exactly one route — its kind stops being
/// `BLOCK_KIND_ENTITY`, and every path that does that hands the block
/// back here in the same breath — so the departure is this record with
/// `a == BLOCK_KIND_ENTITY` rather than a kind of its own that would fire
/// nowhere else.
pub const KIND_BLOCK_DECOMMISSIONED: u32 = 6;

/// A thread's runtime state is built: `subject`, `a` and `b` unused, the
/// thread being named by the ring the record lands in.
///
/// A thread whose *first* record is the one that initialises the runtime
/// raises this from inside the ring's own allocation, where §9.7 has it
/// dropped. What it announces is then announced by the ring's existence
/// instead: a ring is registered by that same initialisation.
pub const KIND_THREAD_START: u32 = 7;

/// A thread's runtime state goes away: `subject`, `a` and `b` unused.
/// Written at the head of the exit sequence, so every teardown record
/// below it is inside the same ring — which retires as the sequence's
/// last act.
pub const KIND_THREAD_EXIT: u32 = 8;

/// What a thread's exit collection left registered: `subject` is the
/// number of candidate registrations still standing, which is the bounded
/// leak the exit reports, `a` is why the rounds stopped
/// (`crate::cycle::collect::ExitEnding`, as its code) and `b` the entities
/// the rounds freed. Written once per exit, after the collection and before
/// the queue's segments go back (`crate::cycle::collect::collect_before_exit`).
pub const KIND_EXIT_RESIDUE: u32 = 9;

/// An edge a reset severed because the arena had no memory to record the
/// child it named: `subject` is the arena's address, `a` the survivor whose
/// slot was emptied and `b` the child that slot held. One record per edge,
/// inside the reset's own bracket, which is what lets a reading say which
/// graph the process lost rather than only how much of it
/// (`dev/DECISIONS.md`, "a survivor cell the pool cannot supply severs the
/// edge, and the reset finishes").
pub const KIND_ARENA_RESET_SEVERED_EDGE: u32 = 10;

/// A COW survivor whose count the reset could not capture, the manager
/// having refused the log the capture goes in: `subject` is the arena's
/// address, `a` the survivor and `b` the count it carried at its promotion,
/// which is the count the reconciliation would have replaced. The survivor
/// keeps the references its arena holders held, so this record is how a
/// reading tells that bounded leak from an ordinary live count
/// (`memory::reset_window::record_cow_capture`). One record per refused
/// capture, inside the reset's own bracket.
pub const KIND_ARENA_RESET_REFUSED_CAPTURE: u32 = 11;

/// A non-final decrement's candidate gate: `subject` is the entity, `a`
/// [`REGISTERED_NOW`] when this decrement wrote its entry into R, or
/// [`REGISTERED_ALREADY`] when `CANDIDATE_BIT` already stood and the gate's
/// other bits were clear, `b` unused. Written at `refcount::release_word`
/// alone: the overflow drain moves an entry already counted here.
pub const KIND_CANDIDATE_REGISTERED: u32 = 12;

/// A collector's batch took its roots and starts its trace: `subject` is the
/// mutator's record, `a` [`BATCH_AT_THE_THRESHOLD`] or
/// [`BATCH_OF_A_STANDING_RING`], `b` the roots taken.
/// A grant that takes no root writes [`KIND_GRANT_WITHOUT_BATCH`] instead.
pub const KIND_BATCH_START: u32 = 13;

/// A collector's batch ended its trace: `subject` is the mutator's record,
/// `a` the exit (the `BATCH_END_*` codes), `b` 1 when the mark's first
/// descent had ended, every root's first region expanded. A batch
/// that unwinds writes none; its roots' verdicts are still written.
pub const KIND_BATCH_END: u32 = 14;

/// A verdict the collector posted for one root: `subject` is the root, `a`
/// the verdict (the `VERDICT_*` codes), `b` unused.
pub const KIND_ROOT_VERDICT: u32 = 15;

/// A root put where only a turnover gives it back: `subject` is the root,
/// `a` where (the `DEFERRED_*` codes), `b` the lane's index under
/// `wait-by-readings` and 0 otherwise. A refused push writes nothing here:
/// the root is written back or kept, and that is what is recorded.
pub const KIND_ROOT_DEFERRED: u32 = 16;

/// A root of P's disposition written back into R as a registration is:
/// `subject` is the root, `a` the verdict its entry carried (the `VERDICT_*`
/// codes: a proposal refused or resurrected, a root read live the lane had
/// no block for, an unwalked root, a resurrected zero count), `b` unused.
pub const KIND_ROOT_WRITTEN_BACK: u32 = 17;

/// Deferred roots moved back to be read: `subject` is 0, `a` which move (the
/// `REOFFERED_*` codes), `b` the entries moved. One record per lane or
/// splice, so the roots are `b`'s sum.
pub const KIND_REOFFERED: u32 = 18;

/// A mutator's epoch cell advanced: `subject` is the record, `a` why (the
/// `TURNOVER_*` codes), `b` the turnovers after the advance. Written on the
/// thread that advances it, the collector's except by hand.
pub const KIND_TURNOVER: u32 = 19;

/// A set a collection's commit confirmed and tore down, which the commit
/// treats as one component (`crate::cycle::collect`, "The commit is one
/// component"): `subject` is its first member, `a` 0, `b` its member count,
/// so `b`'s sum is the members freed.
/// The members' slots come back through the ordinary death path, and a
/// registered member's through [`KIND_WITHHELD_SLOT_RETURNED`] later.
pub const KIND_COMPONENT_RECLAIMED: u32 = 20;

/// A completed death's withheld slot retired by a mutator's pass: `subject`
/// is the entity, `a` where its entry stood (the `SLOT_FROM_*` codes), `b`
/// unused.
pub const KIND_WITHHELD_SLOT_RETURNED: u32 = 21;

/// A grant a collector released with no batch: `subject` is the mutator's
/// record, `a` why (the `GRANT_*` codes), `b` unused.
pub const KIND_GRANT_WITHOUT_BATCH: u32 = 22;

/// [`KIND_CANDIDATE_REGISTERED`]: the decrement wrote the entry.
pub const REGISTERED_NOW: u64 = 0;
/// [`KIND_CANDIDATE_REGISTERED`]: the entity was a candidate already.
pub const REGISTERED_ALREADY: u64 = 1;

/// [`KIND_BATCH_START`]: a take of a ring standing below the threshold.
pub const BATCH_OF_A_STANDING_RING: u64 = 0;
/// [`KIND_BATCH_START`]: R read at the threshold.
pub const BATCH_AT_THE_THRESHOLD: u64 = 1;

/// [`KIND_BATCH_END`]: the mark and every scan ran to their end.
pub const BATCH_END_COMPLETE: u64 = 0;
/// [`KIND_BATCH_END`]: recalled in the pass over the roots before the trace.
pub const BATCH_END_RECALLED_IN_THE_PASS: u64 = 2;
/// [`KIND_BATCH_END`]: recalled inside the mark or the scan; the snapshot of
/// the zero rows was posted.
pub const BATCH_END_RECALLED_IN_THE_TRACE: u64 = 4;
/// [`KIND_BATCH_END`]: recalled after the trace completed, in the walk of the
/// collector's stamps (`crate::cycle::collector_stamps`).
pub const BATCH_END_RECALLED_AFTER_THE_TRACE: u64 = 5;
/// [`KIND_BATCH_END`]: an allocation refused inside the mark or the scan; the
/// snapshot of the zero rows was posted.
pub const BATCH_END_REFUSED_IN_THE_TRACE: u64 = 6;
/// [`KIND_BATCH_END`]: the mutator's withheld returns crossed their mark inside
/// the mark; the mark ended there, the scan ran to its end and the roots were
/// posted off its colours.
pub const BATCH_END_WOUND_DOWN: u64 = 7;
/// [`KIND_BATCH_END`]: wound down as [`BATCH_END_WOUND_DOWN`], and the scan
/// after it stopped by the mutator's recall at the stop level or by a refused
/// allocation; the snapshot was posted.
pub const BATCH_END_WOUND_DOWN_THEN_CUT: u64 = 8;

/// [`KIND_ROOT_VERDICT`] and [`KIND_ROOT_WRITTEN_BACK`]: the discriminants of
/// `cycle::queue::verdicts::Verdict`, which asserts them.
pub const VERDICT_PROPOSED: u64 = 0;
/// See [`VERDICT_PROPOSED`].
pub const VERDICT_READ_LIVE: u64 = 1;
/// See [`VERDICT_PROPOSED`].
pub const VERDICT_ZERO_COUNT: u64 = 2;
/// See [`VERDICT_PROPOSED`].
pub const VERDICT_UNWALKED: u64 = 3;

/// [`KIND_ROOT_DEFERRED`]: into the deferred lane from R's pass, a
/// collection over R having marked it.
pub const DEFERRED_FROM_R: u64 = 0;
/// [`KIND_ROOT_DEFERRED`]: into the deferred lane from P's disposition.
pub const DEFERRED_FROM_P: u64 = 1;

/// [`KIND_REOFFERED`]: the one deferred lane at a turnover.
pub const REOFFERED_AT_THE_TURN: u64 = 0;
/// [`KIND_REOFFERED`]: a lane whose wait passed, under `wait-by-readings`.
pub const REOFFERED_LANE_DUE: u64 = 1;
/// [`KIND_REOFFERED`]: every lane merged at once, before the pressure path
/// or the exit, or by a driver's hand.
pub const REOFFERED_EVERY_LANE: u64 = 2;

/// [`KIND_TURNOVER`]: the collector's work since the last advance reached
/// `SPENT_PER_PROOF` times what its stamps cost to prove. Before 2026-10-03
/// the code named the turn after 64 batches, which it replaced.
pub const TURNOVER_BY_PROOFS: u64 = 0;
/// [`KIND_TURNOVER`]: X of the collector's clock since the last advance.
pub const TURNOVER_BY_X: u64 = 1;
/// [`KIND_TURNOVER`]: the record's new life.
pub const TURNOVER_NEW_LIFE: u64 = 2;
/// [`KIND_TURNOVER`]: a test's advance by hand.
pub const TURNOVER_BY_HAND: u64 = 3;

/// [`KIND_WITHHELD_SLOT_RETURNED`]: the entry stood in P.
pub const SLOT_FROM_P: u64 = 1;
/// [`KIND_WITHHELD_SLOT_RETURNED`]: R's pass.
pub const SLOT_FROM_R: u64 = 2;
/// [`KIND_WITHHELD_SLOT_RETURNED`]: the run at R's front.
pub const SLOT_FROM_R_FRONT_RUN: u64 = 3;
/// [`KIND_WITHHELD_SLOT_RETURNED`]: the overflow buffer.
pub const SLOT_FROM_OVERFLOW: u64 = 4;
/// [`KIND_WITHHELD_SLOT_RETURNED`]: a deferred lane's sweep.
pub const SLOT_FROM_A_DEFERRED_LANE: u64 = 5;

/// [`KIND_GRANT_WITHOUT_BATCH`]: the recall stood before the batch.
pub const GRANT_RECALLED_BEFORE_THE_BATCH: u64 = 0;
/// [`KIND_GRANT_WITHOUT_BATCH`]: the pool refused the workspace.
pub const GRANT_WORKSPACE_REFUSED: u64 = 1;
/// [`KIND_GRANT_WITHOUT_BATCH`]: R gave no root, or P had no room.
pub const GRANT_NOTHING_TAKEN: u64 = 2;

/// The highest kind that has a site. The mask is a `u64`, so a kind past
/// 63 would shift out of it and enable the wrong one — a limit worth
/// failing the build over rather than discovering as a silent
/// misreading.
const HIGHEST_KIND: u32 = KIND_GRANT_WITHOUT_BATCH;

const _: () = assert!(
    HIGHEST_KIND < 64,
    "the enabled mask is one word, so a kind past 63 has no bit"
);

/// One bit per kind, `1 << kind`.
const fn bit(kind: u32) -> u64 {
    1u64 << kind
}

/// The kinds a site writes unless an investigator says otherwise — the
/// default set of §9.5, which is what the census hunt of 2026-08-06 had
/// to build by hand.
///
/// The collector's kinds are outside it, in [`COLLECTOR_KINDS`]: a
/// registration per non-final decrement evicts every default kind within
/// one request of a web load.
pub const DEFAULT_KINDS: u64 = bit(KIND_ENTITY_BIRTH)
    | bit(KIND_ENTITY_DEATH)
    | bit(KIND_ARENA_RESET_BEGIN)
    | bit(KIND_ARENA_RESET_END)
    | bit(KIND_BLOCK_COMMISSIONED)
    | bit(KIND_BLOCK_DECOMMISSIONED)
    | bit(KIND_THREAD_START)
    | bit(KIND_THREAD_EXIT)
    | bit(KIND_EXIT_RESIDUE)
    | bit(KIND_ARENA_RESET_SEVERED_EDGE)
    | bit(KIND_ARENA_RESET_REFUSED_CAPTURE);

/// The collector's operations, on demand (S67.8 in `dev/plans/S67.md`):
/// registrations, batches, verdicts, deferrals, write-backs, re-offers,
/// turnovers, reclaimed components and returned slots, and grants without a
/// batch.
pub const COLLECTOR_KINDS: u64 = bit(KIND_CANDIDATE_REGISTERED)
    | bit(KIND_BATCH_START)
    | bit(KIND_BATCH_END)
    | bit(KIND_ROOT_VERDICT)
    | bit(KIND_ROOT_DEFERRED)
    | bit(KIND_ROOT_WRITTEN_BACK)
    | bit(KIND_REOFFERED)
    | bit(KIND_TURNOVER)
    | bit(KIND_COMPONENT_RECLAIMED)
    | bit(KIND_WITHHELD_SLOT_RETURNED)
    | bit(KIND_GRANT_WITHOUT_BATCH);

/// The kinds whose `a` is a code below [`crate::journal::CODES`], which the
/// count beside the ring keeps apart; every other kind is counted whole.
/// Each collector kind is one.
const CODED_KINDS: u64 = COLLECTOR_KINDS;

/// Whether a record of `kind` is counted by its code `a`
/// ([`crate::journal::Counts`]).
#[inline]
pub(crate) fn is_coded(kind: u32) -> bool {
    kind < 64 && CODED_KINDS & bit(kind) != 0
}

/// Which kinds are written, process-wide.
///
/// One word every thread loads and nobody writes in steady state, rather
/// than a mask per ring: a per-ring mask would let an investigator
/// journal one suspect thread heavily and leave the rest cheap, and
/// nothing in §9 depends on the answer, so the cheaper read wins until
/// something does (§9.8 leaves the question open).
static ENABLED: AtomicU64 = AtomicU64::new(DEFAULT_KINDS);

/// Whether a site of this kind writes. One relaxed load and one test;
/// the branch is predictable, the line being written once per
/// investigation at most.
#[inline]
pub fn kind_enabled(kind: u32) -> bool {
    ENABLED.load(Ordering::Relaxed) & bit(kind) != 0
}

/// The kinds currently written, as a mask of `1 << kind`.
pub fn enabled_kinds() -> u64 {
    ENABLED.load(Ordering::Relaxed)
}

/// Write only these kinds from now on.
///
/// Takes effect on each thread whenever it next loads the word, which is
/// per site: an investigator narrowing the set mid-run gets no promise
/// about where in the ring the narrowing lands, and needs none — a window
/// is marked by cursors, and a kind that stopped being written is one
/// whose absence the mask explains.
pub fn set_enabled_kinds(mask: u64) {
    ENABLED.store(mask, Ordering::Relaxed);
}

/// Hold the runtime's own sites quiet, and serialize against every other
/// test that needs the same. Tests only.
///
/// A test that counts records, rings or blocks measures a world in which
/// it is the only thing journaling, and with the sites compiled in it is
/// not: a thread that allocates takes a ring, and a ring is a block. The
/// mask is process-wide, so quieting it is only sound while no other such
/// test runs — which is what the lock is for.
///
/// **Every test of the ring mechanism takes it**, rather than the ones
/// that were seen failing without it: which of them notices depends on
/// whether its runner thread had been initialised already, so the ones
/// that pass today are the same test on a different day. What does not
/// take it is the rest of the suite, which goes on exercising the sites
/// — that is where a site on a path §9.7 forbids is caught — and the
/// site tests below, which hold [`DEFAULT_KINDS`] instead.
#[cfg(test)]
pub(crate) fn disable_sites_for_test() -> SitesHeld {
    set_sites_for_test(0)
}

/// Write only `mask` while the guard lives, serialized against every
/// other test that sets one. Tests only, and the general form of
/// [`disable_sites_for_test`]: a test that needs a *particular* site to
/// be some thread's first record turns every earlier one off, and a test
/// that reads records holds [`DEFAULT_KINDS`] so that a quieting test
/// cannot turn them off underneath it.
///
/// **Taken before `block_pool::test_guard` wherever a test holds both.**
/// Two process-wide locks in two orders is a deadlock, and this one is
/// the outer.
#[cfg(test)]
pub(crate) fn set_sites_for_test(mask: u64) -> SitesHeld {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = SitesHeld(LOCK.lock().unwrap_or_else(|e| e.into_inner()));
    set_enabled_kinds(mask);
    guard
}

/// Holds the mask a test set, and restores the default set when it goes
/// out of scope. See [`set_sites_for_test`].
#[cfg(test)]
pub(crate) struct SitesHeld(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

#[cfg(test)]
impl Drop for SitesHeld {
    fn drop(&mut self) {
        set_enabled_kinds(DEFAULT_KINDS);
    }
}

/// Write one event, if this build has the sites and this kind is enabled.
///
/// `journal_event!(kind, subject, a, b)`. The payload expressions are
/// evaluated only when the record is really written, so a site may read
/// whatever it needs to fill its words without charging a disabled kind
/// for the reading.
///
/// Without the `debug-journal` feature it expands to nothing at all —
/// not to a call that returns early, and not to a branch. That is §9.6's
/// promise, and it is why this is a macro rather than an inline function.
macro_rules! journal_event {
    ($kind:expr, $subject:expr, $a:expr, $b:expr $(,)?) => {{
        #[cfg(feature = "debug-journal")]
        {
            if $crate::journal::kinds::kind_enabled($kind) {
                // `site` stays 0 until the debug ABI gives the compiler a
                // site table to stamp (`debug-modes.md` §4.1).
                $crate::journal::record($kind, 0, $subject, $a, $b);
            }
        }
    }};
}

pub(crate) use journal_event;

#[cfg(test)]
mod tests;
