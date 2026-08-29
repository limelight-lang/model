//! Multi-threaded allocator comparison — the fair "in your face" test.
//!
//! Now that the heap is thread-safe, we can compare it against mimalloc
//! and the system allocator on multi-threaded workloads, where their
//! thread-safety machinery is actually exercised (in the single-thread
//! bench, mimalloc still paid for a per-free thread check that our
//! single-thread path skipped — that caveat is gone here).
//!
//! Two patterns, both from the Larson server benchmark family:
//!
//! - **independent**: T threads each run larson entirely on their own
//!   (fixed live set, free-one + alloc-one). Measures scaling and the
//!   per-thread fast path with no cross-thread traffic.
//! - **bleeding**: T threads in a ring — every object is allocated by
//!   one thread and freed by the *next* (Larson's "bleeding"). This is
//!   the cross-thread path: our `remote_free` stack vs mimalloc's
//!   per-page `xthread_free`.
//!
//! Every allocation is written to; sizes 16..8192 (our heap's envelope)
//! for all. Run: `cargo run --release --example mt_bench`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use ll_model::memory::heap::{ll_thread_init, with_thread_heap};
use mimalloc::MiMalloc;

const THREADS: usize = 8;
const MIN: usize = 16;
const MAX: usize = 8192;

struct Rng(u64);
impl Rng {
    #[inline]
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    #[inline]
    fn size(&mut self) -> usize {
        MIN + (self.next() as usize) % (MAX - MIN)
    }
}

#[inline]
fn touch(p: *mut u8) {
    if !p.is_null() {
        unsafe { (p as *mut u64).write(0x1) };
    }
}

/// Allocator dispatch. Each variant's `alloc`/`free` closures use the
/// calling thread's resources, so the same closure gives per-thread
/// behavior across spawned threads.
#[derive(Clone, Copy)]
enum Which {
    Heap,
    Mi,
    Sys,
}

impl Which {
    #[inline]
    fn alloc(self, size: usize) -> *mut u8 {
        match self {
            Which::Heap => unsafe { with_thread_heap(|h| h.alloc(size)) },
            Which::Mi => unsafe { MiMalloc.alloc(Layout::from_size_align_unchecked(size, 8)) },
            Which::Sys => unsafe { System.alloc(Layout::from_size_align_unchecked(size, 8)) },
        }
    }
    #[inline]
    unsafe fn free(self, ptr: *mut u8, size: usize) {
        match self {
            Which::Heap => unsafe { with_thread_heap(|h| h.free(ptr)) },
            Which::Mi => unsafe {
                MiMalloc.dealloc(ptr, Layout::from_size_align_unchecked(size, 8))
            },
            Which::Sys => unsafe {
                System.dealloc(ptr, Layout::from_size_align_unchecked(size, 8))
            },
        }
    }
    fn name(self) -> &'static str {
        match self {
            Which::Heap => "our heap",
            Which::Mi => "mimalloc",
            Which::Sys => "system",
        }
    }
}

/// T threads, each independent larson: `slots` live, `rounds` of
/// free-one + alloc-one. Returns aggregate throughput (M ops/s), where
/// one op = one free+alloc round.
fn independent(which: Which, slots: usize, rounds: usize) -> f64 {
    let start = Instant::now();
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            thread::spawn(move || {
                assert!(ll_thread_init(), "the runtime started this thread");
                let mut rng = Rng(0x1234_5678 ^ (t as u64 + 1).wrapping_mul(0x9E37));
                let mut live: Vec<(*mut u8, usize)> = Vec::with_capacity(slots);
                for _ in 0..slots {
                    let s = rng.size();
                    let p = which.alloc(s);
                    touch(p);
                    live.push((p, s));
                }
                for _ in 0..rounds {
                    let i = (rng.next() as usize) % slots;
                    let (p, s) = live[i];
                    unsafe { which.free(p, s) };
                    let ns = rng.size();
                    let np = which.alloc(ns);
                    touch(np);
                    live[i] = (np, ns);
                }
                for (p, s) in live {
                    unsafe { which.free(p, s) };
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let secs = start.elapsed().as_secs_f64();
    (THREADS * rounds) as f64 / secs / 1e6
}

/// T threads in a ring: each allocates `rounds` objects and sends them
/// to the next thread, which frees them (cross-thread). Returns
/// aggregate throughput (M cross-thread free+alloc / s).
fn bleeding(which: Which, rounds: usize) -> f64 {
    // rx[i] receives objects for thread i to free; each thread sends to
    // the next thread's sender.
    let mut txs = Vec::with_capacity(THREADS);
    let mut rxs = Vec::with_capacity(THREADS);
    for _ in 0..THREADS {
        let (tx, rx) = mpsc::channel::<(usize, usize)>();
        txs.push(tx);
        rxs.push(Some(rx));
    }

    let done = Arc::new(AtomicU64::new(0));
    let start = Instant::now();

    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let tx_next = txs[(t + 1) % THREADS].clone();
            let rx = rxs[t].take().unwrap();
            let done = done.clone();
            thread::spawn(move || {
                assert!(ll_thread_init(), "the runtime started this thread");
                let mut rng = Rng(0x0BADF00D ^ (t as u64 + 1).wrapping_mul(0x2545F491));
                let mut freed = 0usize;
                for _ in 0..rounds {
                    // Allocate and hand to the next thread.
                    let s = rng.size();
                    let p = which.alloc(s);
                    touch(p);
                    tx_next.send((p as usize, s)).unwrap();
                    // Free whatever has arrived from the previous thread.
                    while let Ok((q, qs)) = rx.try_recv() {
                        unsafe { which.free(q as *mut u8, qs) };
                        freed += 1;
                    }
                }
                // Drain the rest: this thread must free exactly `rounds`
                // objects (all sent by the previous thread).
                while freed < rounds {
                    let (q, qs) = rx.recv().unwrap();
                    unsafe { which.free(q as *mut u8, qs) };
                    freed += 1;
                }
                done.fetch_add(freed as u64, Ordering::Relaxed);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let secs = start.elapsed().as_secs_f64();
    assert_eq!(done.load(Ordering::Relaxed) as usize, THREADS * rounds);
    (THREADS * rounds) as f64 / secs / 1e6
}

fn main() {
    // Warm each allocator's thread caches.
    let _ = independent(Which::Heap, 100, 100);
    let _ = independent(Which::Mi, 100, 100);
    let _ = independent(Which::Sys, 100, 100);

    println!("Multi-threaded allocator comparison ({THREADS} threads)\n");

    println!("== independent larson (3000 slots, 20000 rounds/thread) ==");
    println!("   aggregate throughput, higher is better:");
    for w in [Which::Heap, Which::Mi, Which::Sys] {
        let mops = independent(w, 3000, 20_000);
        println!("   {:<10} {:>7.1} M ops/s", w.name(), mops);
    }

    println!("\n== bleeding larson (30000 cross-thread objects/thread) ==");
    println!("   every object freed by a different thread than allocated:");
    for w in [Which::Heap, Which::Mi, Which::Sys] {
        let mops = bleeding(w, 30_000);
        println!("   {:<10} {:>7.1} M ops/s", w.name(), mops);
    }

    println!("\n(Heap note: our per-op cost includes a thread-local RefCell");
    println!(" borrow that mimalloc/system don't pay in this harness.)");
}
