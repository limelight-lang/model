//! Measurement probe, counts rather than clocks: what share of an epoch's
//! parked records the young-free exemption would recycle instead.
//!
//! While an epoch is in flight a free parks rather than recycling
//! (`crate::memory::deferred_free`), because an id must name one entity from
//! walk to drain. The exemption proposed in `rfc/model/gc/rc-walk.md`
//! recycles anyway when the dying entity's epoch byte reads zero or the
//! current number: that is the walk's own skip predicate
//! (`crate::collector::Epoch::walk_rows`) read backwards, so the byte answers
//! "was this entity enrolled" rather than standing in for the answer. Node C2
//! of `rfc/model/gc/walk/questions.md` asks what the exemption is worth.
//!
//! ## The window is two epochs wide, not one
//!
//! The byte reads zero from birth until a walk meets the slot, and that walk
//! writes the current number and skips the entity (allocate-black). So the
//! entity is exempt through the rest of the epoch it was born in **and**
//! through the whole of the next one, and it parks only from the epoch after
//! that, when its byte reads an older number and the walk enrols it. Stated
//! without the calendar: **an entity is exempt until the second walk that
//! meets it.**
//!
//! An arm that opens one epoch and churns after its walk therefore exercises
//! the zero disjunct alone and reports a floor. Each arm here runs
//! [`WARM_EPOCHS`] epochs of the same churn before the one it measures, so
//! the population's ages and stamps are whatever that history made them, and
//! the two disjuncts are counted apart to show both fire.
//!
//! ## What the share is a property of
//!
//! The workload's lifetimes against the epoch's duration, with nothing the
//! collector does moving it. The arms share one population and one death
//! count and differ only in which live entity a step kills:
//!
//! - **oldest** — a lifetime is exactly [`POPULATION`] steps. An entity dying
//!   at position `t` of the epoch was born `POPULATION` steps earlier, which
//!   is inside the previous epoch when `t > POPULATION - steps`, so the
//!   predicted count is `clamp(2 * steps - POPULATION, 0, steps)`.
//! - **uniform** — a uniformly chosen victim, so the age at death is
//!   geometric with mean [`POPULATION`] and the entity is exempt when that
//!   age is under `t + steps`. The predicted share is that probability
//!   averaged over the epoch's death positions.
//!
//! The prediction is the control on one arm and one cell: the two curves are
//! more than the assertion's tolerance apart only where the epoch runs about
//! as long as a lifetime, and the `oldest` arm is a step function its own
//! loop bound would reproduce. What the `oldest` arm is good for is the
//! negative half — the exemption removes nothing from a population that
//! outlives two epochs.
//!
//! ## The ceiling this measures, and what sits under it
//!
//! Only an entity slot carries a header, so only an entity's record can be
//! exempt at all. A dying out-of-line string parks its payload
//! (`crate::string::string_die`) and a dying array parks its table storage as
//! separate headerless records, and both park whatever the entity's age. This
//! class holds no payload, so its share is the exemption's ceiling; how far
//! under it a real heap sits is the payload-per-entity figure of node A6.
//!
//! ```
//! cargo test --release --lib -- --ignored measure_young_free_exemption --nocapture
//! ```

use std::collections::VecDeque;

use super::*;
use crate::memory::deferred_free::parked_count;
use crate::object::ll_object_die;
use crate::refcount::{EPOCH_BYTE_MASK, EPOCH_BYTE_SHIFT, header_flags, ll_release};

/// Live entities, and so the mean lifetime in churn steps: a step kills one
/// and allocates one, so the population is constant.
const POPULATION: usize = 10_000;

/// Deaths landed inside each epoch, chosen around the region where the
/// lifetime crosses one and two epochs.
const STEPS: [usize; 4] = [1_000, 6_000, 10_000, 30_000];

/// Epochs of the same churn run before the measured one. Two decide a stamp,
/// so three leaves one to spare and the `oldest` arm's exact prediction is
/// the assertion that says the population settled.
const WARM_EPOCHS: usize = 3;

/// Which live entity a churn step kills. The choice is the whole difference
/// between the arms.
#[derive(Clone, Copy)]
enum Victim {
    /// The oldest live entity: every lifetime is [`POPULATION`] steps.
    Oldest,
    /// A uniformly chosen live entity: the age at death is geometric with
    /// mean [`POPULATION`].
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

/// What one epoch of churn produced.
#[derive(Default)]
struct Arm {
    /// Records parked by the deaths this epoch landed.
    parked: usize,
    /// Deaths whose entity had never been met by a walk.
    exempt_zero: usize,
    /// Deaths whose entity this epoch's own walk stamped and skipped.
    exempt_current: usize,
    /// Entities the walk stamped and skipped, from `EpochStats`: the size of
    /// the class that makes [`Arm::exempt_current`] possible.
    stamped_new: usize,
}

/// xorshift64. A count instrument repeats exactly or it is not one, so the
/// victims come from a fixed seed rather than from the system.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// The share the arm's lifetime distribution predicts, from the arm alone:
/// no measured quantity enters either branch.
fn predicted(victim: Victim, steps: usize) -> f64 {
    match victim {
        Victim::Oldest => {
            let exempt = (2 * steps).saturating_sub(POPULATION).min(steps);
            exempt as f64 / steps as f64
        }
        Victim::Uniform => {
            let survives_a_step = 1.0 - 1.0 / POPULATION as f64;
            (1..=steps)
                .map(|t| 1.0 - survives_a_step.powi((t + steps - 1) as i32))
                .sum::<f64>()
                / steps as f64
        }
    }
}

/// One epoch: open it, walk, land `steps` deaths and as many births inside
/// it, then close and flush. `measure` decides whether the deaths are
/// classified or only performed.
fn one_epoch(
    ctx: &mut LLContext,
    cls: *const crate::class::Class,
    live: &mut VecDeque<*mut Object>,
    rng: &mut u64,
    victim: Victim,
    steps: usize,
    measure: bool,
) -> Arm {
    let mut arm = Arm::default();
    let mut e = Epoch::open();
    checkpoint();
    e.snapshot();
    e.walk();
    arm.stamped_new = e.stats.stamped_new;

    let before = parked_count();
    for _ in 0..steps {
        let dying = match victim {
            Victim::Oldest => live.pop_front().unwrap(),
            Victim::Uniform => {
                let i = (next(rng) % live.len() as u64) as usize;
                live.swap_remove_back(i).unwrap()
            }
        };

        if measure {
            // Read while the entity is whole. The releasing store is
            // counter-half only and teardown's flag writes preserve the byte,
            // so the answer would not change after the death, but nothing is
            // gained by reading it later.
            let flags = unsafe { header_flags(dying as *const RcHeader) };
            let stamp = ((flags & EPOCH_BYTE_MASK) >> EPOCH_BYTE_SHIFT) as u8;
            if stamp == 0 {
                arm.exempt_zero += 1;
            } else if stamp == e.number {
                arm.exempt_current += 1;
            }
        }

        unsafe {
            assert!(
                ll_release(dying as *mut RcHeader),
                "the probe holds the only reference"
            );
            ll_object_die(dying);
        }

        live.push_back(unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) });
    }

    arm.parked = parked_count() - before;

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
    arm
}

/// One arm: a population churned through [`WARM_EPOCHS`] epochs and measured
/// over the next.
fn one_arm(victim: Victim, steps: usize) -> Arm {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("YoungFreeExemption").build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let mut live: VecDeque<*mut Object> = (0..POPULATION)
        .map(|_| unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) })
        .collect();

    let mut rng = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..WARM_EPOCHS {
        one_epoch(
            &mut ctx, cls, &mut live, &mut rng, victim, steps, false,
        );
    }

    let arm = one_epoch(&mut ctx, cls, &mut live, &mut rng, victim, steps, true);

    for obj in live {
        unsafe {
            if ll_release(obj as *mut RcHeader) {
                ll_object_die(obj);
            }
        }
    }

    arm
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored"]
fn measure_young_free_exemption() {
    let mut zero_seen = 0usize;
    let mut current_seen = 0usize;

    for victim in [Victim::Oldest, Victim::Uniform] {
        for &steps in &STEPS {
            let arm = one_arm(victim, steps);
            let exempt = arm.exempt_zero + arm.exempt_current;
            let share = exempt as f64 / steps as f64;
            let prediction = predicted(victim, steps);
            zero_seen += arm.exempt_zero;
            current_seen += arm.exempt_current;

            println!(
                "young_free victim={} population={POPULATION} steps={steps} parked={} \
                 exempt={exempt} (zero {} + current {}) stamped_new={} share={share:.3} \
                 predicted={prediction:.3}",
                victim.name(),
                arm.parked,
                arm.exempt_zero,
                arm.exempt_current,
                arm.stamped_new
            );

            assert_eq!(
                arm.parked, steps,
                "one record per death: this class carries no payload"
            );

            match victim {
                // Exact rather than approximate: under a fixed lifetime the
                // exempt deaths are precisely those whose birth fell after
                // the previous epoch's walk. A warm-up that had not settled
                // would miss this.
                Victim::Oldest => assert_eq!(
                    exempt,
                    (2 * steps).saturating_sub(POPULATION).min(steps)
                ),
                Victim::Uniform => assert!(
                    (share - prediction).abs() < 0.03,
                    "share {share:.3} against the geometric prediction {prediction:.3}"
                ),
            }
        }
    }

    assert!(
        zero_seen > 0 && current_seen > 0,
        "both disjuncts of the exemption's predicate must fire, or the table \
         measures half of it: zero {zero_seen}, current {current_seen}"
    );
}
