//! Mutator-side GC-protocol tax on the object lifecycle.
//!
//! What the numbers answer (session 2026-07-27, `dev/BENCHMARKS.md`):
//! the cost of create → release-to-zero → die through the public ABI,
//! and what the death-branch checkpoint costs there — plain
//! `ll_release` (one checkpoint test per death) against the batched
//! form (`ll_gc_checkpoint` once per run + `ll_release_batch` per
//! death).
//!
//! Run in BOTH configurations, A→B→A per `dev/BENCHMARKS.md`:
//!
//! ```
//! cargo bench --bench lifecycle                          # rc-walk (default)
//! cargo bench --bench lifecycle --no-default-features    # rc-trace
//! ```
//!
//! The class has no properties and no destructor, so the loop isolates
//! factory + header + release machinery; the object is 16 bytes — the
//! hottest size class.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ll_model::object::{ll_object_constructed, ll_object_die, ll_object_new_abi};
use ll_model::refcount::{MemoryCategory, RcHeader, ll_release, ll_release_batch};
use ll_model::gc::ll_gc_checkpoint;
use ll_model::{Class, ClassBuilder, Object};

const BATCH: usize = 64;

fn class() -> *const Class {
    ClassBuilder::new("LifecycleBench").build()
}

/// create → constructed → release (dies) → die, one object at a time.
/// The death branch runs the checkpoint test on every iteration in an
/// rc-walk build; an rc-trace build has no test here.
fn create_release_die(c: &mut Criterion, cls: *const Class) {
    c.bench_function("lifecycle/create_release_die", |b| {
        b.iter(|| unsafe {
            let obj = ll_object_new_abi(
                std::ptr::null_mut(),
                cls,
                MemoryCategory::GcHeap as u32,
            );
            ll_object_constructed(std::ptr::null_mut(), obj);
            if ll_release(black_box(obj) as *mut RcHeader) {
                ll_object_die(obj);
            }
        })
    });
}

/// The same work in runs of `BATCH`, released with the per-death
/// checkpoint test (plain `ll_release`). Baseline for the batched arm.
fn batch_plain(c: &mut Criterion, cls: *const Class) {
    let mut objects: Vec<*mut Object> = Vec::with_capacity(BATCH);
    c.bench_function("lifecycle/batch_64_plain_release", |b| {
        b.iter(|| unsafe {
            for _ in 0..BATCH {
                let obj = ll_object_new_abi(
                    std::ptr::null_mut(),
                    cls,
                    MemoryCategory::GcHeap as u32,
                );
                ll_object_constructed(std::ptr::null_mut(), obj);
                objects.push(obj);
            }
            for &obj in &objects {
                if ll_release(black_box(obj) as *mut RcHeader) {
                    ll_object_die(obj);
                }
            }
            objects.clear();
        })
    });
}

/// The compiler-emitted shape: one `ll_gc_checkpoint` fronting the
/// run, `ll_release_batch` per death. The delta against
/// `batch_64_plain_release` is the checkpoint test times 63.
fn batch_batched(c: &mut Criterion, cls: *const Class) {
    let mut objects: Vec<*mut Object> = Vec::with_capacity(BATCH);
    c.bench_function("lifecycle/batch_64_batched_release", |b| {
        b.iter(|| unsafe {
            for _ in 0..BATCH {
                let obj = ll_object_new_abi(
                    std::ptr::null_mut(),
                    cls,
                    MemoryCategory::GcHeap as u32,
                );
                ll_object_constructed(std::ptr::null_mut(), obj);
                objects.push(obj);
            }
            ll_gc_checkpoint();
            for &obj in &objects {
                if ll_release_batch(black_box(obj) as *mut RcHeader) {
                    ll_object_die(obj);
                }
            }
            objects.clear();
        })
    });
}

/// Non-final release churn: retain + release on one live object, the
/// count never reaching zero. This is the path where rc-trace pays its
/// candidate machinery (first decrement buffers, later ones test the
/// buffered bit) and rc-walk pays only its whole-word header protocol.
fn retain_release(c: &mut Criterion, cls: *const Class) {
    let obj = unsafe {
        let obj = ll_object_new_abi(
            std::ptr::null_mut(),
            cls,
            MemoryCategory::GcHeap as u32,
        );
        ll_object_constructed(std::ptr::null_mut(), obj);
        obj
    };
    c.bench_function("lifecycle/retain_release_nonfinal", |b| {
        b.iter(|| unsafe {
            ll_model::refcount::ll_retain(black_box(obj) as *mut RcHeader);
            ll_release(black_box(obj) as *mut RcHeader);
        })
    });
}

fn benches(c: &mut Criterion) {
    let cls = class();
    create_release_die(c, cls);
    batch_plain(c, cls);
    batch_batched(c, cls);
    retain_release(c, cls);
}

criterion_group!(lifecycle, benches);
criterion_main!(lifecycle);
