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
//! Both gates are in [`journal_event!`], which is how a site is written.
//! The macro evaluates its payload arguments **after** the mask test, so
//! a disabled kind costs the load and the branch and nothing else: some
//! sites read a header or a block field to fill their words, and a
//! disabled site must not pay for a reading nobody will look at.

use std::sync::atomic::{AtomicU64, Ordering};

/// Birth of an entity: `subject` is its address, `a` its
/// [`crate::refcount::EntityKind`] as a number, `b` its
/// [`crate::refcount::MemoryCategory`]. Written where the header is
/// published, which is the one door every factory in the crate goes
/// through.
pub const KIND_ENTITY_BIRTH: u32 = 1;

/// Death of an entity: `subject` is its address, `a` its entity kind, `b`
/// unused. Written at each kind's own teardown body rather than at the
/// kind switch above them, because an object reaches teardown by two
/// doors and a nested array by neither.
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

/// A collector epoch opens (`rc-walk` builds): `a` is the epoch number,
/// `subject` and `b` unused. The number wraps at 256, which is the width
/// the header's epoch byte has; a window longer than 256 epochs is not
/// one the ring could hold anyway.
pub const KIND_EPOCH_BEGIN: u32 = 9;

/// A collector epoch closes (`rc-walk` builds): `a` is the epoch number,
/// `b` the components it confirmed, `subject` unused.
pub const KIND_EPOCH_END: u32 = 10;

/// The highest kind that has a site. The mask is a `u64`, so a kind past
/// 63 would shift out of it and enable the wrong one — a limit worth
/// failing the build over rather than discovering as a silent
/// misreading.
const HIGHEST_KIND: u32 = KIND_EPOCH_END;

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
/// Every kind that has a site is in it, because every kind that has a
/// site is a default one so far. That coincidence ends at the first
/// on-demand kind, and the constant is the place where it will show.
pub const DEFAULT_KINDS: u64 = bit(KIND_ENTITY_BIRTH)
    | bit(KIND_ENTITY_DEATH)
    | bit(KIND_ARENA_RESET_BEGIN)
    | bit(KIND_ARENA_RESET_END)
    | bit(KIND_BLOCK_COMMISSIONED)
    | bit(KIND_BLOCK_DECOMMISSIONED)
    | bit(KIND_THREAD_START)
    | bit(KIND_THREAD_EXIT)
    | bit(KIND_EPOCH_BEGIN)
    | bit(KIND_EPOCH_END);

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
mod tests {
    use super::*;

    /// The mask and the constants are one thing: a kind whose bit is not
    /// in [`DEFAULT_KINDS`] writes nothing, and adding a constant without
    /// adding its bit is the way to get a site that silently never fires.
    #[test]
    fn every_kind_with_a_site_is_in_the_default_set() {
        for kind in 1..=HIGHEST_KIND {
            assert_ne!(
                DEFAULT_KINDS & bit(kind),
                0,
                "kind {kind} has no bit in the default set"
            );
        }
        assert_eq!(
            DEFAULT_KINDS.count_ones(),
            HIGHEST_KIND,
            "the default set has a bit the kinds do not"
        );
    }

    /// The unset kind is what an unwritten slot reads as, so no site may
    /// have it and no mask may enable it.
    #[test]
    fn the_unset_kind_is_not_a_site() {
        assert_eq!(DEFAULT_KINDS & bit(super::super::KIND_UNSET), 0);
    }
}

/// What the sites record, and what they cost when the mask says nothing.
///
/// Only where the sites exist: without the feature there is nothing here
/// to test, which is the whole of §9.6's claim and is checked over the
/// emitted IR instead (`PLAN.md`, S5.2).
#[cfg(all(test, feature = "debug-journal"))]
mod site_tests {
    use super::*;
    use crate::journal::{Event, Window, between, mark, this_thread_identity};
    use crate::memory::block_pool::BlockPool;
    use crate::refcount::{EntityKind, MemoryCategory};

    /// Every event the answers carry, whichever ring it came from.
    fn events(windows: Vec<Window>) -> Vec<Event> {
        windows
            .into_iter()
            .flat_map(|window| match window {
                Window::Records(records) => records,
                _ => Vec::new(),
            })
            .collect()
    }

    /// The acceptance question of 2026-08-06 in miniature: which strings
    /// died inside this window, answered from the journal alone. The hunt
    /// itself is S5.3; what this pins is that the two sites carry the
    /// address and the kind it needs to ask.
    #[test]
    fn a_string_born_and_killed_inside_a_window_is_in_it_twice() {
        // The default set, held: a test that quiets the sites would
        // otherwise turn them off underneath this one, and the mask is
        // process-wide.
        let _sites = set_sites_for_test(DEFAULT_KINDS);
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

        let start = mark();
        let s = unsafe { crate::string::ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"hunt") };
        assert!(!s.is_null());
        let subject = s as u64;
        unsafe {
            assert!(crate::refcount::ll_release(
                s as *mut crate::refcount::RcHeader
            ));
            crate::object::ll_entity_die(s as *mut crate::refcount::RcHeader);
        }
        let end = mark();

        // By ring as well as by address: a slot is reused, and another
        // thread's entity born at this address inside the window would
        // answer under the same name.
        let ring = this_thread_identity();
        let mine: Vec<(u32, u64)> = events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == ring && event.subject == subject)
            .map(|event| (event.kind, event.a))
            .collect();
        assert_eq!(
            mine,
            vec![
                (KIND_ENTITY_BIRTH, EntityKind::String as u64),
                (KIND_ENTITY_DEATH, EntityKind::String as u64),
            ],
            "a string's life inside the window is its birth and its death"
        );
    }

    /// A block's two ends are one address in two records, and the
    /// decommission carries the kind the block arrived with — which is
    /// how §9.5's third block event is asked for: a block leaving the set
    /// the entity walk reaches is this record with an entity kind in it.
    #[test]
    fn a_block_round_trip_is_a_commission_and_a_decommission() {
        // The default set, held: a test that quiets the sites would
        // otherwise turn them off underneath this one, and the mask is
        // process-wide.
        let _sites = set_sites_for_test(DEFAULT_KINDS);
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();

        let start = mark();
        let block = pool.get();
        assert!(!block.is_null());
        pool.put(block);
        let end = mark();

        // By ring as well as by address. A block is process-global: the
        // one this thread is handed may have been returned by another
        // inside the same window, which reads as a decommission before
        // the commission — seen once in forty runs under contention.
        let ring = this_thread_identity();
        let mine: Vec<u32> = events(between(&start, &end))
            .into_iter()
            .filter(|event| event.thread == ring && event.subject == block as u64)
            .map(|event| event.kind)
            .collect();
        assert_eq!(
            mine,
            vec![KIND_BLOCK_COMMISSIONED, KIND_BLOCK_DECOMMISSIONED]
        );
    }

    /// A site on a thread with no ring yet allocates one, and that
    /// allocation runs back through the very pool the site sits on. The
    /// re-entry is refused rather than recursed into (§9.7), and the
    /// borrow the pool's thread cache takes is closed before the site —
    /// under a held borrow the re-entry finds the cell already borrowed
    /// and the thread dies where a release build would abort.
    ///
    /// Only the decommission kind is on, which is what makes `put`'s the
    /// **first** record on that thread: with commissioning enabled the
    /// ring is allocated by `get` long before, and the re-entry this is
    /// about never happens.
    #[test]
    fn a_pools_own_site_may_be_a_threads_first_record() {
        let _only = set_sites_for_test(bit(KIND_BLOCK_DECOMMISSIONED));
        let _g = crate::memory::block_pool::test_guard();

        let recorded = std::thread::spawn(|| {
            // No `ll_thread_init`: the first record does that too, and
            // this is the shape a site inside the allocator really has.
            let pool = BlockPool::global();
            let mut blocks = Vec::new();
            // Past the thread cache's capacity, so the overflow flush
            // runs — that is the push to the global list the record must
            // not be holding a borrow across.
            for _ in 0..12 {
                blocks.push(pool.get());
            }
            let start = mark();
            for block in blocks {
                pool.put(block);
            }
            let end = mark();
            let ring = this_thread_identity();
            events(between(&start, &end))
                .into_iter()
                .filter(|event| event.thread == ring && event.kind == KIND_BLOCK_DECOMMISSIONED)
                .count()
        })
        .join()
        .expect("a record on a ringless thread took the thread down");

        assert!(
            recorded > 0,
            "the thread journaled nothing, so the site was never reached"
        );
    }

    /// A kind the mask has turned off costs the load and the branch and
    /// writes nothing — which is what lets an investigator spend a ring
    /// on one question instead of on the loudest kind in the runtime.
    #[test]
    fn a_disabled_kind_writes_no_record() {
        let _quiet = disable_sites_for_test();
        let _g = crate::memory::block_pool::test_guard();
        let pool = BlockPool::global();
        // One record through the door that ignores the mask, so that this
        // thread has a ring to be silent in: with no ring the silence
        // below would be the wrong silence, and the test would pass on a
        // thread the journal never reached.
        crate::journal::record(KIND_BLOCK_COMMISSIONED, 0, 0, 0, 0);
        let ring = this_thread_identity();
        assert_ne!(ring, 0, "the thread has no ring, so it is silent anyway");

        let start = mark();
        let block = pool.get();
        pool.put(block);
        let end = mark();

        assert_eq!(
            events(between(&start, &end))
                .into_iter()
                .filter(|event| event.thread == ring && event.subject == block as u64)
                .count(),
            0,
            "a disabled kind reached the ring"
        );
    }
}
