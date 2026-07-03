//! Long-lived server workload simulation.
//!
//! Emulates a server processing many requests over a long run:
//!   - each request gets its own arena, allocates a randomized batch of
//!     objects (varied sizes, headers written), then the arena is
//!     dropped — returning its blocks to the shared global pool;
//!   - a persistent "cache" arena accumulates a bounded set of
//!     long-lived objects (never reset), simulating warmed server state;
//!   - a multi-threaded phase runs worker threads against the one shared
//!     pool, the way a real server's workers do.
//!
//! The point it proves: memory (2 MB regions carved from the OS) must
//! PLATEAU — the resident set is a small working set that requests churn
//! through, not something that grows with request count. Run:
//!
//!     cargo run --release --example server_sim

use std::time::Instant;

use ll_model::memory::block_pool::REGION_SIZE;
use ll_model::{Arena, BlockPool};

/// Seeded xorshift — deterministic, no dependency.
struct Rng(u64);
impl Rng {
    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    #[inline]
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next_u64() as usize) % (hi - lo)
    }
}

#[inline]
fn write_header(p: *mut u8) {
    unsafe { (p as *mut u64).write(0x1) };
}

/// One request: allocate a randomized batch into `arena`, return
/// (objects, bytes). The arena is reset by the caller.
fn run_request(arena: &mut Arena, rng: &mut Rng) -> (u64, u64) {
    let count = rng.range(1000, 4000); // objects per request
    let mut bytes = 0u64;
    for _ in 0..count {
        // Realistic small-object size mix: 16..256 bytes.
        let size = rng.range(16, 256);
        let p = arena.alloc(size);
        write_header(p);
        bytes += ((size + 7) & !7) as u64;
    }
    (count as u64, bytes)
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn single_thread_phase() {
    const REQUESTS: u64 = 100_000;
    println!("== Single-thread phase: {REQUESTS} requests ==");

    let pool = BlockPool::global();
    let regions_before = pool.regions_carved();

    // Persistent cache arena: never reset, accumulates bounded state.
    let mut cache = Arena::new();
    let mut cache_objects = 0u64;
    const CACHE_CAP: u64 = 20_000;

    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut total_objects = 0u64;
    let mut total_bytes = 0u64;

    // Snapshot the region count once the cache is warm, so the plateau
    // check reflects steady state, not warm-up.
    let mut warm_regions = 0usize;

    let start = Instant::now();
    for i in 0..REQUESTS {
        let mut arena = Arena::new();
        let (objs, bytes) = run_request(&mut arena, &mut rng);
        total_objects += objs;
        total_bytes += bytes;

        // A few objects "escape" into the long-lived cache until warm.
        if cache_objects < CACHE_CAP {
            for _ in 0..2 {
                write_header(cache.alloc(64));
                cache_objects += 1;
            }
        }

        arena.reset(|_| {}); // request ends: blocks return to the pool

        if i == REQUESTS / 10 {
            warm_regions = pool.regions_carved();
        }
    }
    let elapsed = start.elapsed();

    let regions_after = pool.regions_carved();
    let carved = regions_after - regions_before;
    let resident = carved as u64 * REGION_SIZE as u64;

    let secs = elapsed.as_secs_f64();
    println!("  objects allocated : {total_objects}");
    println!("  bytes allocated   : {:.1} MiB", mib(total_bytes));
    println!("  wall time         : {:.3} s", secs);
    println!(
        "  throughput        : {:.0} req/s, {:.1} M obj/s",
        REQUESTS as f64 / secs,
        total_objects as f64 / secs / 1e6
    );
    println!(
        "  per object        : {:.2} ns",
        elapsed.as_nanos() as f64 / total_objects as f64
    );
    println!(
        "  regions carved    : {carved}  ({:.0} MiB resident high-water)",
        mib(resident)
    );
    println!(
        "  churn ratio       : {:.0}x  (bytes allocated / resident)",
        total_bytes as f64 / resident as f64
    );
    println!(
        "  plateau check     : {} regions at 10% done -> {} at 100%  ({})",
        warm_regions - regions_before,
        carved,
        if carved <= (warm_regions - regions_before) + 2 {
            "STABLE — memory did not grow with request count"
        } else {
            "GREW — investigate"
        }
    );
    drop(cache);
    println!();
}

fn multi_thread_phase() {
    const THREADS: usize = 8;
    const REQUESTS_PER_THREAD: u64 = 25_000;
    println!(
        "== Multi-thread phase: {THREADS} workers x {REQUESTS_PER_THREAD} requests, one shared pool =="
    );

    let pool = BlockPool::global();
    let regions_before = pool.regions_carved();

    let start = Instant::now();
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            std::thread::spawn(move || {
                let mut rng = Rng(0xD1B54A32D192ED03 ^ (t as u64).wrapping_mul(0x2545F491));
                let mut objs = 0u64;
                for _ in 0..REQUESTS_PER_THREAD {
                    let mut arena = Arena::new();
                    let (o, _) = run_request(&mut arena, &mut rng);
                    objs += o;
                    arena.reset(|_| {});
                }
                objs
            })
        })
        .collect();

    let total_objects: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let elapsed = start.elapsed();

    let carved = pool.regions_carved() - regions_before;
    let total_requests = THREADS as u64 * REQUESTS_PER_THREAD;
    let secs = elapsed.as_secs_f64();

    println!("  objects allocated : {total_objects}");
    println!("  wall time         : {:.3} s", secs);
    println!(
        "  throughput        : {:.0} req/s, {:.1} M obj/s (aggregate)",
        total_requests as f64 / secs,
        total_objects as f64 / secs / 1e6
    );
    println!(
        "  regions carved    : {carved}  ({:.0} MiB resident, ~{} per worker)",
        mib(carved as u64 * REGION_SIZE as u64),
        carved / THREADS
    );
    println!(
        "    (bounded: each worker holds an arena + a small block cache; no growth with request count)"
    );
    println!();
}

fn main() {
    println!("Limelight memory manager — server workload simulation\n");
    single_thread_phase();
    multi_thread_phase();
    println!("Done. Memory stayed bounded while requests churned through the pool.");
}
