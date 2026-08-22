//! Measurement probe, counts rather than clocks: what share of an epoch's
//! parked records the young-free exemption would recycle instead.
//!
//! While an epoch is in flight a free parks rather than recycling
//! (`crate::memory::deferred_free`), because an id must name one entity
//! from walk to drain. The exemption proposed in `rfc/model/gc/rc-walk.md`
//! recycles anyway when the dying entity's epoch byte reads zero or the
//! current number: that is the walk's own skip predicate
//! (`crate::collector::Epoch::walk_rows`) read backwards, so the byte
//! answers "was this entity enrolled" rather than standing in for the
//! answer. Node C2 of `rfc/model/gc/walk/questions.md` asks what the
//! exemption is worth, and the answer is a share rather than a constant.
//!
//! ## What the share is a property of
//!
//! A record is exempt when its entity died younger than the epoch it died
//! in, so the share follows the workload's lifetimes against the epoch's
//! duration and nothing the collector does moves it. The two arms bracket
//! it with one population and one death count, differing only in which
//! live entity a step kills:
//!
//! - **oldest** — a lifetime is exactly [`POPULATION`] steps, so nothing
//!   dies young until the epoch outlives one. Predicted share
//!   `max(0, steps - POPULATION) / steps`.
//! - **uniform** — a lifetime is geometric with mean [`POPULATION`], so
//!   young deaths appear from the first step. Predicted share is the
//!   lifetime distribution averaged over the window,
//!   `mean(1 - (1 - 1/POPULATION)^t)` for `t` in `1..=steps`.
//!
//! The prediction is printed beside the count as the control: a probe
//! reading back the loop bound that produced it would match one curve at
//! most, and matching both is what makes the count a measurement of the
//! predicate rather than of the arm.
//!
//! Maturing the population through one whole epoch first is load-bearing.
//! An entity that has never been walked carries stamp zero, which the
//! exemption reads as young at any age, so a probe that skips the warm-up
//! reports one hundred per cent for every arm.
//!
//! ## The ceiling this measures, and what sits under it
//!
//! Only an entity slot carries a header, so only an entity's record can be
//! exempt at all. A dying out-of-line string parks its payload
//! (`crate::string::string_die`) and a dying array parks its table storage
//! as separate headerless records, and both park whatever the entity's
//! age. This class holds no payload, so its share is the exemption's
//! ceiling; how far under it a real heap sits needs the payload-per-entity
//! figure of node A6 and is not this probe's to give.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_young_free_exemption --nocapture
//! ```

use std::collections::VecDeque;

use super::*;
use crate::memory::deferred_free::parked_count;
use crate::object::ll_object_die;
use crate::refcount::{EPOCH_BYTE_MASK, EPOCH_BYTE_SHIFT, header_flags, ll_release};

/// Live entities, and so the mean lifetime in churn steps: a step kills
/// one and allocates one, so the population is constant.
const POPULATION: usize = 10_000;

/// Deaths landed inside the measured epoch, from a hundredth of the
/// population to four times it.
const STEPS: [usize; 4] = [100, 1_000, 10_000, 40_000];

/// Which live entity a churn step kills. The choice is the whole
/// difference between the arms.
#[derive(Clone, Copy)]
enum Victim {
    /// The oldest live entity: every lifetime is [`POPULATION`] steps.
    Oldest,
    /// A uniformly chosen live entity: lifetimes are geometric with mean
    /// [`POPULATION`].
    Uniform,
}

impl Victim {
    fn name(self) -> &'static str {
        match self {
            Victim::Oldest => "oldest",
            Victim::Uniform => "uniform",
        }
    }
}

/// xorshift64. A count instrument repeats exactly or it is not one, so
/// the victims come from a fixed seed rather than from the system.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// The share the arm's lifetime distribution predicts.
fn predicted(victim: Victim, steps: usize) -> f64 {
    match victim {
        Victim::Oldest => steps.saturating_sub(POPULATION) as f64 / steps as f64,
        Victim::Uniform => {
            let survives_a_step = 1.0 - 1.0 / POPULATION as f64;
            (1..=steps)
                .map(|t| 1.0 - survives_a_step.powi(t as i32))
                .sum::<f64>()
                / steps as f64
        }
    }
}

/// One arm: parked records landed inside the measured epoch, and how many
/// of them the exemption's predicate would have recycled.
fn one_arm(victim: Victim, steps: usize) -> (usize, usize) {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("YoungFreeExemption").build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let mut live: VecDeque<*mut Object> = (0..POPULATION)
        .map(|_| unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) })
        .collect();

    stepped_epoch(); // mature: an unwalked entity reads young at any age

    let mut e = Epoch::open();
    checkpoint();
    e.snapshot();
    e.walk();

    let before = parked_count();
    let mut exempt = 0usize;
    let mut rng = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..steps {
        let dying = match victim {
            Victim::Oldest => live.pop_front().unwrap(),
            Victim::Uniform => {
                let i = (next(&mut rng) % live.len() as u64) as usize;
                live.swap_remove_back(i).unwrap()
            }
        };

        // Read before the death: the releasing store is counter-half only
        // and teardown's flag writes preserve the byte, but the read is
        // cheapest where the entity is certainly whole.
        let flags = unsafe { header_flags(dying as *const RcHeader) };
        let stamp = ((flags & EPOCH_BYTE_MASK) >> EPOCH_BYTE_SHIFT) as u8;
        if stamp == 0 || stamp == e.number {
            exempt += 1;
        }

        unsafe {
            assert!(
                ll_release(dying as *mut RcHeader),
                "the probe holds the only reference"
            );
            ll_object_die(dying);
        }

        live.push_back(unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) });
    }

    let parked = parked_count() - before;

    e.judge();
    if e.stats.candidates > 0 {
        e.condemn();
        checkpoint();
        e.recheck_and_post();
        checkpoint();
    }

    let _ = e.close();
    checkpoint();
    assert_eq!(parked_count(), 0, "the post-epoch flush returns everything");

    for obj in live {
        unsafe {
            if ll_release(obj as *mut RcHeader) {
                ll_object_die(obj);
            }
        }
    }

    (parked, exempt)
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored"]
fn measure_young_free_exemption() {
    for victim in [Victim::Oldest, Victim::Uniform] {
        for &steps in &STEPS {
            let (parked, exempt) = one_arm(victim, steps);
            let share = exempt as f64 / steps as f64;
            let prediction = predicted(victim, steps);
            println!(
                "young_free victim={} population={POPULATION} steps={steps} parked={parked} \
                 exempt={exempt} share={share:.3} predicted={prediction:.3}",
                victim.name()
            );

            assert_eq!(
                parked, steps,
                "one record per death: this class carries no payload"
            );

            match victim {
                // Exact rather than approximate: under a fixed lifetime the
                // young deaths are precisely the replacements the epoch
                // outlived.
                Victim::Oldest => assert_eq!(exempt, steps.saturating_sub(POPULATION)),
                Victim::Uniform => assert!(
                    (share - prediction).abs() < 0.03,
                    "share {share:.3} against the geometric prediction {prediction:.3}"
                ),
            }
        }
    }
}
