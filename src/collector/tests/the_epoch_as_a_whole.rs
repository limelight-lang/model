//! An epoch abandoned in flight strands nothing: condemnation is
//! private state that dies with it, and `Drop` releases the deferral
//! window that a stuck activity bit would leave parking every free.
//! The other two run the real thing — a collector thread against a
//! mutator that only reaches checkpoints, and a free-running mutator
//! churning garbage through whole epochs — where the races are the
//! design's accepted ones. `measure_epoch_cost` is a probe rather
//! than a test and is ignored.

use super::*;

/// Since the eager-death amendment a collector that condemns and
/// dies before posting strands nothing: condemnation is private
/// state and dies with the epoch — no stranded bytes, no owed
/// acquittals. `Drop` releases the deferral window (a stuck
/// activity bit would park every free forever) and the ring is
/// collected cleanly next epoch.
#[test]
fn an_abandoned_epoch_strands_nothing() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("CollectorAbandoned")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        tie(a, 16, b);
        tie(b, 16, a);
    }

    stepped_epoch(); // mature

    let mut e = Epoch::open();
    checkpoint();
    e.snapshot();
    e.walk();
    e.judge();
    assert_eq!(e.stats.candidates, 1);
    e.condemn();
    checkpoint(); // ack
    drop(e); // the collector dies before recheck_and_post

    assert_eq!(
        crate::epoch::outstanding_verdicts(),
        0,
        "nothing was posted, nothing is owed"
    );
    assert!(
        !crate::memory::deferred_free::active(),
        "the deferral window was released by the unwind"
    );
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "the ring lives");

    let stats = stepped_epoch();
    assert_eq!(stats.confirmed, 1, "and is collected cleanly next epoch");
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
    arena.reset(|_| {});
}

/// The production shape: a real collector thread runs a full epoch
/// while the mutator thread does nothing but reach checkpoints.
#[test]
#[cfg_attr(
    miri,
    ignore = "spin-waits against a live thread; the stepped tests cover the logic under Miri"
)]
fn a_threaded_epoch_collects_a_mature_ring() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("CollectorThreaded")
        .prop("child", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    unsafe {
        tie(a, 16, b);
        tie(b, 16, a);
    }

    stepped_epoch(); // mature quietly first

    let collector = std::thread::spawn(run_epoch);
    // The mutator: checkpoints until the collector is done. The
    // entities belong to this thread; the drain runs here.
    let stats = loop {
        checkpoint();
        if collector.is_finished() {
            break collector.join().unwrap();
        }

        std::thread::yield_now();
    };

    checkpoint(); // flush parked memory

    assert_eq!(stats.confirmed, 1);
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
    let seen = walked_addresses();
    assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
    arena.reset(|_| {});
}

/// Free-running concurrency: the collector loops whole epochs while
/// the mutator churns — allocating garbage rings, dropping them,
/// checkpointing only as a side effect of its own allocations. The
/// races this executes (collector byte stores against mutator word
/// stores and field stores) are the design's accepted ones, now all
/// through relaxed atomics; the assertions are the invariants that
/// must hold whatever the interleaving: the live ring survives,
/// every garbage destructor runs exactly once, nothing crashes.
#[test]
#[cfg_attr(
    miri,
    ignore = "free-running mixed-size atomic races are the design's accepted model gap; Miri rejects them and cannot pace live threads — the stepped tests carry Miri coverage"
)]
fn a_free_running_mutator_survives_concurrent_epochs() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("CollectorStress")
        .prop("child", true)
        .prop("link", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let mk = |ctx: &mut LLContext| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };

    // The canary: a frame-held ring that must survive every epoch.
    let (k1, k2) = (mk(&mut ctx), mk(&mut ctx));
    unsafe {
        tie(k1, 16, k2);
        tie(k2, 16, k1);
        ll_retain(k1 as *mut RcHeader);
    }

    let collector = std::thread::spawn(|| {
        let mut stats = Vec::new();
        for _ in 0..4 {
            stats.push(run_epoch());
        }

        stats
    });

    // The mutator: garbage rings and short-lived holders. Bounded —
    // an unthrottled loop out-produces the epochs and the deferral
    // window by orders of magnitude (measured: gigabytes of parked
    // garbage) — and after the bound it just keeps checkpointing.
    // Its own entity allocations are checkpoints too.
    let mut rings = 0usize;
    while !collector.is_finished() {
        if rings < 10_000 {
            let (a, b) = (mk(&mut ctx), mk(&mut ctx));
            unsafe {
                tie(a, 16, b);
                tie(b, 16, a);
                tie(a, 32, k1); // an edge into the live ring, retained
                ll_retain(k1 as *mut RcHeader);
                // Both frame references gone: the ring floats, cyclic.
            }

            rings += 1;
            let holder = mk(&mut ctx);
            unsafe {
                // A fresh holder is never condemned (allocate-black),
                // so its release always reports the death.
                assert!(ll_release(holder as *mut RcHeader));
                crate::object::ll_object_die(holder);
            }
        }

        checkpoint();
        std::hint::spin_loop();
    }

    let epoch_stats = collector.join().unwrap();
    assert_eq!(epoch_stats.len(), 4);

    // Quiesce and sweep what the concurrent epochs did not reach:
    // rings born after the last snapshot need one epoch to mature
    // and one to die.
    checkpoint();
    let seen = walked_addresses();
    assert!(
        seen.contains(&(k1 as usize)) && seen.contains(&(k2 as usize)),
        "the live ring survived every interleaving"
    );
    stepped_epoch();
    stepped_epoch();
    stepped_epoch();
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed) as usize,
        2 * rings + rings, // two ring members and one holder per round
        "every garbage entity destructed exactly once"
    );
    let seen = walked_addresses();
    assert!(seen.contains(&(k1 as usize)) && seen.contains(&(k2 as usize)));

    // The canary dies only when the frame lets go — with the ring
    // references the garbage rings piled on it all dropped.
    assert_eq!(
        unsafe { (*k1).rc.refcount },
        2, // frame + k2's slot: every ring's retain was dropped
        "each collected ring released its edge into the live ring"
    );
    assert!(!unsafe { ll_release(k1 as *mut RcHeader) });
    stepped_epoch();
    stepped_epoch();
    let seen = walked_addresses();
    assert!(!seen.contains(&(k1 as usize)) && !seen.contains(&(k2 as usize)));
    arena.reset(|_| {});
}

/// Measurement probe, not a correctness test: collector-side cost
/// of one epoch by live-set size and shape (`dev/BENCHMARKS.md`,
/// 2026-07-27 session). Ignored so ordinary runs stay fast; run
/// explicitly, release mode:
///
/// ```
/// cargo test --release --lib -- --ignored measure_epoch_cost --nocapture
/// ```
///
/// Two shapes per size: `singletons` (rows only, no edges) and
/// `chain` (one traced edge per entity, every node externally
/// retained so nothing is condemned). Four epochs each; the first
/// is warm-up — read the later ones.
#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_epoch_cost() {
    use std::time::Instant;
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("EpochCost").prop("child", true).build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    for &n in &[1_000usize, 10_000, 100_000] {
        for chained in [false, true] {
            let mut objects: Vec<*mut Object> = Vec::with_capacity(n);
            for i in 0..n {
                let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
                if chained {
                    // Slot ref + our handle: rc 2, externally live,
                    // never condemned; the walk still chases the edge.
                    unsafe { ll_retain(obj as *mut RcHeader) };
                    if i > 0 {
                        unsafe { tie(objects[i - 1], 16, obj) };
                    }
                }

                objects.push(obj);
            }

            for round in 0..4 {
                let t0 = Instant::now();
                let mut e = Epoch::open();
                checkpoint();
                let t1 = Instant::now();
                e.snapshot();
                let t2 = Instant::now();
                e.walk();
                let t3 = Instant::now();
                e.judge();
                let t4 = Instant::now();
                assert_eq!(e.stats.candidates, 0, "probe set must stay live");
                let _ = e.close();
                checkpoint();
                let t5 = Instant::now();
                println!(
                    "epoch_cost n={n} shape={} round={round}: handshake={:?} \
                     snapshot={:?} walk={:?} judge={:?} close+flush={:?} total={:?}",
                    if chained { "chain" } else { "singletons" },
                    t1 - t0,
                    t2 - t1,
                    t3 - t2,
                    t4 - t3,
                    t5 - t4,
                    t5 - t0,
                );
            }

            // Teardown without a 100k-deep drop cascade: null every
            // slot first (raw writes — the counts the slots held are
            // settled by hand below), then every node dies childless
            // with a uniform two releases: {constructed-or-slot ref,
            // the probe's retain} for chained, one for singletons.
            if chained {
                for &obj in &objects {
                    unsafe {
                        Object::prop_at(obj, 16).write(Value::null());
                    }
                }
            }

            for &obj in objects.iter().rev() {
                if chained {
                    unsafe { ll_release(obj as *mut RcHeader) };
                }

                if unsafe { ll_release(obj as *mut RcHeader) } {
                    unsafe { crate::object::ll_object_die(obj) };
                }
            }
        }
    }
}

/// Measurement probe, counts rather than clocks: what an epoch parks
/// as a function of deaths landed inside it. The design's bound is
/// churn times epoch duration, not live heap (`rfc/model/gc/rc-walk.md`,
/// "Deferred physical release, and when an epoch ends"), and the two
/// arms exhibit both halves of that correction: victims born before
/// the epoch and victims born inside it park alike. The stepped epoch
/// runs no collector thread, so the figures repeat exactly and the
/// clock's noise floor does not apply.
///
/// ```
/// cargo test --lib -- --ignored measure_parked_memory --nocapture
/// ```
#[test]
#[ignore = "measurement probe; run explicitly with --ignored"]
fn measure_parked_memory() {
    use crate::memory::deferred_free::parked_count;
    use crate::object::ll_object_die;

    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("ParkedMemory").build();
    let slot_bytes = unsafe { (*cls).object_size } as usize;
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    for &k in &[0usize, 100, 1_000, 10_000] {
        let pre: Vec<*mut Object> = (0..k)
            .map(|_| unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) })
            .collect();

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        for &obj in &pre {
            unsafe {
                assert!(ll_release(obj as *mut RcHeader));
                ll_object_die(obj);
            }
        }

        let pre_born = parked_count();
        e.walk();
        // The half the 2026-07-27 correction added: entities allocated
        // after the epoch opened park exactly like the snapshot's.
        for _ in 0..k {
            let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
            unsafe {
                assert!(ll_release(obj as *mut RcHeader));
                ll_object_die(obj);
            }
        }

        let with_mid_born = parked_count();
        e.judge();
        e.condemn();
        checkpoint();
        e.recheck_and_post();
        let _ = e.close();
        let at_close = parked_count();
        checkpoint();
        let after_flush = parked_count();
        assert_eq!(after_flush, 0, "the post-epoch flush returns everything");
        println!(
            "parked_memory k={k} slot_bytes={slot_bytes} pre_born={pre_born} \
             with_mid_born={with_mid_born} at_close={at_close} after_flush={after_flush}"
        );
    }
}
