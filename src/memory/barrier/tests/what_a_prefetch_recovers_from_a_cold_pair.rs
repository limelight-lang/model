//! Measurement probe, not a correctness test: how much of a cold counted
//! pair a software prefetch recovers.
//!
//! [`super::what_a_counted_pair_costs_when_headers_miss`] measured the pair
//! at 2.9 ns with both foreign headers warm and 33 ns at a population of a
//! million. The store itself is inside the 2.9; the other thirty are the two
//! misses. Node A5 of `rfc/model/gc/walk/questions.md` asks what could
//! reach them, and a narrower count word cannot — it makes the store
//! cheaper, and the store is not what costs. The barrier knows both
//! addresses before it needs either header, so the misses are prefetchable
//! in principle. This probe says whether that principle pays.
//!
//! ## Method
//!
//! Two arms, both counted, the working set the only variable.
//!
//! - [`super::what_a_counted_pair_costs_when_headers_miss::counted_round`]
//!   unchanged: read the displaced value, `store_box` the new one,
//!   `drop_ref` the displaced one.
//! - [`prefetched_round`] does the same and, [`DISTANCE`] iterations ahead
//!   of each store, issues a read prefetch for the two headers that store
//!   will touch — the header of the value it will retain, and the header of
//!   the value it will displace, which is reachable through the owner slot
//!   the same cursor selects.
//!
//! The prefetched arm therefore pays everything the counted arm pays and
//! the prefetches besides. The difference is what the prefetch is worth,
//! and it can come out negative: three extra address computations and two
//! instructions per store are real work, and at a narrow working set they
//! buy nothing because nothing misses.
//!
//! **Two counted populations, not one.** Both arms retain and release, so
//! both need slots the setup counted; sharing one population would let each
//! arm warm the headers the other is about to read, which is the confound
//! the neighbouring probe was corrected for.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_prefetch_recovery --nocapture
//! ```
//!
//! The figures obey `dev/BENCHMARKS.md`'s Method.

use std::time::{Duration, Instant};

use super::*;
use crate::object::Object;
use crate::refcount::header_refcount;

use super::what_a_counted_pair_costs_when_headers_miss::{
    ArmStats, ROUNDS, SETS, STORES, children, counted_round, cursor, holder_class, leaf_class,
    owners, prefill, reduce, trip,
};

/// Stores between a prefetch and the store it was issued for.
///
/// Far enough that a miss has time to return, near enough that the line is
/// not evicted before use. Eight is the usual starting point for a scattered
/// access pattern and is not tuned here: tuning it is a second experiment,
/// and this one asks whether the direction is worth that experiment.
const DISTANCE: usize = 8;

/// Issue a read prefetch for the header at the front of `entity`.
///
/// A null pointer is skipped rather than prefetched: the address is
/// dereferenced by the hardware only for a real line, but computing it from
/// null is still a page-zero touch on some models and says nothing useful.
///
/// # Safety
/// `entity` is null or an address whose first cache line is mapped.
#[inline(always)]
unsafe fn prefetch_header(entity: *mut RcHeader) {
    if entity.is_null() {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_mm_prefetch(entity as *const i8, core::arch::x86_64::_MM_HINT_T0);
    }

    // Nothing to do on a target without a prefetch intrinsic here: the arm
    // then measures the extra address computation alone, which is the
    // honest answer for that target.
    #[cfg(not(target_arch = "x86_64"))]
    let _ = entity;
}

/// One round of the counted store with both foreign headers prefetched
/// [`DISTANCE`] stores ahead.
///
/// The body is [`counted_round`]'s, with the prefetches added and nothing
/// removed, so the difference between the two arms is the prefetches and
/// the addresses they need.
///
/// # Safety
/// As [`counted_round`].
unsafe fn prefetched_round(
    arena: *mut Arena,
    owners: &[*mut Object],
    values: &[Value],
    mask: usize,
    round: usize,
) -> Duration {
    unsafe {
        let start = Instant::now();
        for i in 0..trip(STORES) {
            // The store `DISTANCE` iterations from now: the value it will
            // retain, and the value it will displace, which is whatever the
            // owner slot holds now.
            let ahead = i + DISTANCE;
            let ahead_owner = owners[cursor(round, ahead, mask)];
            let ahead_slot = Object::prop_at(ahead_owner, 16);
            prefetch_header(std::ptr::read(ahead_slot).entity_ptr());
            prefetch_header(values[cursor(round, ahead + STORES, mask)].entity_ptr());

            let slot = Object::prop_at(owners[cursor(round, i, mask)], 16);
            let old = std::ptr::read(slot);
            assert!(store_box(
                arena,
                MemoryCategory::GcHeap,
                slot,
                values[cursor(round, i + STORES, mask)]
            ));
            if old.is_refcounted() {
                drop_ref(MemoryCategory::GcHeap, old.entity_ptr());
            }
        }

        start.elapsed()
    }
}

/// Run both arms at one working set, interleaved round by round with the
/// starting arm rotating, and return them in the order plain-counted,
/// prefetched.
///
/// Interleaving makes machine drift common mode between the two figures the
/// difference is taken from; rotation keeps one arm from always inheriting
/// the other's cache state.
///
/// # Safety
/// As [`counted_round`] and [`prefetched_round`].
unsafe fn arms_at(
    arena: *mut Arena,
    bare_owners: &[*mut Object],
    prefetched_owners: &[*mut Object],
    values: &[Value],
    mask: usize,
) -> (ArmStats, ArmStats) {
    let mut bare: Vec<f64> = Vec::with_capacity(ROUNDS);
    let mut prefetched: Vec<f64> = Vec::with_capacity(ROUNDS);

    for round in 0..=ROUNDS {
        let per_store = |elapsed: Duration| elapsed.as_nanos() as f64 / STORES as f64;
        let (b, p) = if round % 2 == 0 {
            let b = unsafe { counted_round(arena, bare_owners, values, mask, round) };
            let p = unsafe { prefetched_round(arena, prefetched_owners, values, mask, round) };
            (b, p)
        } else {
            let p = unsafe { prefetched_round(arena, prefetched_owners, values, mask, round) };
            let b = unsafe { counted_round(arena, bare_owners, values, mask, round) };
            (b, p)
        };

        if round > 0 {
            bare.push(per_store(b));
            prefetched.push(per_store(p));
        }
    }

    (reduce(bare), reduce(prefetched))
}

/// Release what every slot of both counted populations holds, then give
/// every child and owner its creation reference back.
///
/// Both populations are counted here, unlike the neighbouring probe's,
/// whose second population was never counted at all.
///
/// # Safety
/// Every argument is live; no slot outside these populations names any of
/// them.
unsafe fn teardown(
    bare_owners: &[*mut Object],
    prefetched_owners: &[*mut Object],
    values: &[Value],
) {
    for owner in bare_owners.iter().chain(prefetched_owners) {
        unsafe {
            let slot = Object::prop_at(*owner, 16);
            let held = std::ptr::read(slot);
            std::ptr::write(slot, Value::null());
            if held.is_refcounted() {
                drop_ref(MemoryCategory::GcHeap, held.entity_ptr());
            }
        }
    }

    for (index, value) in values.iter().enumerate() {
        let entity = value.entity_ptr();
        assert_eq!(
            unsafe { header_refcount(entity) },
            1,
            "child {index} was left holding a count the rounds did not intend"
        );

        unsafe { drop_ref(MemoryCategory::GcHeap, entity) };
    }

    for owner in bare_owners.iter().chain(prefetched_owners) {
        unsafe { drop_ref(MemoryCategory::GcHeap, *owner as *mut RcHeader) };
    }
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_prefetch_recovery() {
    let _g = crate::memory::block_pool::test_guard();
    let holder = holder_class("PrefetchOwner");
    let leaf = leaf_class("PrefetchChild");
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    // The first measurement a process takes is systematically slow, so one
    // narrow set runs and its answer is dropped (`dev/BENCHMARKS.md`,
    // Method, "Throw away the first run").
    for (set, report) in [(64usize, false)]
        .into_iter()
        .chain(SETS.map(|set| (set, true)))
    {
        assert!(
            set.is_power_of_two(),
            "the cursor masks, so a set is a power of two"
        );

        unsafe {
            let bare_owners = owners(context_ptr, holder, set);
            let prefetched_owners = owners(context_ptr, holder, set);
            let values = children(context_ptr, leaf, set);
            prefill(arena_ptr, &bare_owners, &values);
            prefill(arena_ptr, &prefetched_owners, &values);

            let (bare, prefetched) =
                arms_at(arena_ptr, &bare_owners, &prefetched_owners, &values, set - 1);

            if report {
                println!(
                    "prefetch working_set={set} bare={:.3} ns/store min={:.3} max={:.3}",
                    bare.median, bare.minimum, bare.maximum
                );
                println!(
                    "prefetch working_set={set} prefetched={:.3} ns/store min={:.3} max={:.3}",
                    prefetched.median, prefetched.minimum, prefetched.maximum
                );
                println!(
                    "prefetch working_set={set} recovered={:.3} ns/store",
                    bare.median - prefetched.median
                );
            }

            teardown(&bare_owners, &prefetched_owners, &values);
        }
    }
}
