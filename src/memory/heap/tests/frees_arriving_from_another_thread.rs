//! A slot freed by a non-owner is posted to the owning block's
//! stack, and that push is a CAS loop, so several producers at once
//! may lose none of it: after the freers finish, the owner drains
//! and must report zero live slots.

use super::*;

#[test]
fn cross_thread_free_is_correct() {
    let _g = crate::memory::block_pool::test_guard();
    use std::sync::mpsc;
    use std::thread;

    const N: u64 = 5000;
    let (tx, rx) = mpsc::channel::<usize>();

    // Producer: allocate on its own heap, stamp each with its index,
    // hand the pointer to the consumer, and keep allocating (so its
    // slow path drains the incoming cross-thread frees concurrently).
    let producer = thread::spawn(move || {
        ll_thread_init();
        unsafe {
            with_thread_heap(|h| {
                for i in 0..N {
                    let p = h.alloc(24);
                    (p as *mut u64).write(i);
                    tx.send(p as usize).unwrap();
                    // extra churn to exercise the drain path
                    let t = h.alloc(24);
                    h.free(t);
                }
            });
        }
    });

    // Consumer (this thread): verify each value survived, then free
    // cross-thread (posts to the producer's remote stack).
    ll_thread_init();
    let mut count = 0u64;
    for _ in 0..N {
        let p = rx.recv().unwrap() as *mut u8;
        let v = unsafe { *(p as *mut u64) };
        assert!(v < N, "value corrupted across threads");
        unsafe { with_thread_heap(|h| h.free(p)) };
        count += 1;
    }

    assert_eq!(count, N);
    producer.join().unwrap();
}

/// Several threads freeing into the **same** owner's blocks at once.
///
/// The existing coverage missed this: `many_threads_alloc_free_no_corruption`
/// has every thread allocate and free on its own heap, so no slot ever
/// reaches `remote_free`, and `cross_thread_free_is_correct` has exactly
/// one producer. The multi-producer push had no test at all.
///
/// What would break if it were wrong: `free_remote` is a CAS loop, so a
/// lost race would drop a slot from the chain, and the owner would
/// account for fewer slots than were actually freed. That is measured
/// directly — after every freer has finished, the owner drains its
/// queues and must report **zero** live slots. Corruption of the slot
/// contents before the free is caught by the stamp check in each freer.
///
/// It deliberately does *not* assert on the process-global
/// `blocks_out`. That counter is shared with every other test, so a
/// block returning late from elsewhere moves it in either direction —
/// which made this test flaky at ~2 runs in 10 under
/// `--test-threads=16`, failing on someone else's straggler rather
/// than on anything it was testing.
#[test]
fn many_threads_freeing_into_one_owner_lose_no_slots() {
    let _g = crate::memory::block_pool::test_guard();
    use std::sync::mpsc;
    use std::thread;

    const FREERS: usize = 4;
    const PER: usize = 500;
    const STAMP: u8 = 0xAB;

    let mut txs = Vec::with_capacity(FREERS);
    let mut freers = Vec::with_capacity(FREERS);
    for _ in 0..FREERS {
        let (tx, rx) = mpsc::channel::<usize>();
        txs.push(tx);
        freers.push(thread::spawn(move || {
            ll_thread_init();
            let mut n = 0usize;
            for p in rx {
                let p = p as *mut u8;
                assert_eq!(
                    unsafe { *p },
                    STAMP,
                    "slot corrupted before its cross-thread free"
                );
                unsafe { with_thread_heap(|h| h.free(p)) };
                n += 1;
            }

            ll_thread_exit();
            n
        }));
    }

    // This thread owns the blocks. Hand slots out round-robin so all
    // four freers contend on the same block, and keep churning so the
    // drain path runs while their pushes are arriving.
    ll_thread_init();
    unsafe {
        with_thread_heap(|h| {
            for i in 0..(FREERS * PER) {
                let p = h.alloc(24);
                p.write(STAMP);
                txs[i % FREERS].send(p as usize).unwrap();
                let churn = h.alloc(24);
                h.free(churn);
            }
        });
    }

    drop(txs);

    let freed: usize = freers.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(freed, FREERS * PER, "every slot was freed exactly once");

    // Every freer is done, so every push has landed. Drain the queues
    // and account: a slot lost in the CAS loop shows up here as a live
    // slot nobody holds.
    let live = unsafe { with_thread_heap(|h| h.live_slots_after_collect()) };
    assert_eq!(
        live, 0,
        "the owner lost track of a slot freed from another thread"
    );
}

#[test]
fn many_threads_alloc_free_no_corruption() {
    let _g = crate::memory::block_pool::test_guard();
    use std::thread;

    let handles: Vec<_> = (0..8)
        .map(|t| {
            thread::spawn(move || {
                ll_thread_init();
                unsafe {
                    with_thread_heap(|h| {
                        let mut live = Vec::new();
                        for i in 0..2000usize {
                            let size = 16 + (i * 8 + t) % 512;
                            let p = h.alloc(size);
                            assert!(!p.is_null());
                            p.write((t as u8).wrapping_add(1));
                            live.push(p);
                            if live.len() > 100 {
                                let victim = live.swap_remove(i % live.len());
                                h.free(victim);
                            }
                        }

                        for p in live {
                            h.free(p);
                        }
                    });
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
}
