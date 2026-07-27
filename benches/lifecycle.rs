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

/// The bulk-operations 2x2 (`rfc/model/memory/bulk-operations.md`):
/// creation by factory vs by reserved cells, teardown by per-object
/// release vs one vector call. 64 objects per iteration, destructorless
/// class (the compiler emits no `constructed` call for those), all four
/// arms in one binary — no configuration juggling.
fn bulk(c: &mut Criterion, cls: *const Class) {
    use ll_model::memory::heap::ll_entity_reserve;
    use ll_model::object::{ll_object_new_in, ll_release_vector};

    const N: usize = 64;

    unsafe fn create_factory(cls: *const Class, out: &mut Vec<*mut Object>) {
        for _ in 0..N {
            out.push(unsafe {
                ll_object_new_abi(std::ptr::null_mut(), cls, MemoryCategory::GcHeap as u32)
            });
        }
    }

    unsafe fn create_reserved(cls: *const Class, out: &mut Vec<*mut Object>) {
        let mut cells = [std::ptr::null_mut::<u8>(); N];
        let mut contiguous = 0usize;
        let size = unsafe { (*cls).object_size } as usize;
        let n = unsafe { ll_entity_reserve(size, N, cells.as_mut_ptr(), &mut contiguous) };
        for &cell in &cells[..n] {
            out.push(unsafe { ll_object_new_in(cell, cls) });
        }
        for _ in n..N {
            // The manager served short: the ordinary factory is the
            // contract's fallback.
            out.push(unsafe {
                ll_object_new_abi(std::ptr::null_mut(), cls, MemoryCategory::GcHeap as u32)
            });
        }
    }

    unsafe fn release_loop(objects: &mut Vec<*mut Object>) {
        for &obj in objects.iter() {
            if unsafe { ll_release(obj as *mut RcHeader) } {
                unsafe { ll_object_die(obj) };
            }
        }
        objects.clear();
    }

    unsafe fn release_vector(objects: &mut Vec<*mut Object>) {
        unsafe {
            ll_release_vector(objects.as_ptr() as *const *mut RcHeader, objects.len())
        };
        objects.clear();
    }

    let mut objects: Vec<*mut Object> = Vec::with_capacity(N);
    let _ = &mut objects; // reused across arms; criterion runs them serially

    c.bench_function("bulk/factory_create_loop_release", |b| {
        b.iter(|| unsafe {
            create_factory(black_box(cls), &mut objects);
            release_loop(&mut objects);
        })
    });
    c.bench_function("bulk/reserved_create_loop_release", |b| {
        b.iter(|| unsafe {
            create_reserved(black_box(cls), &mut objects);
            release_loop(&mut objects);
        })
    });
    c.bench_function("bulk/factory_create_vector_release", |b| {
        b.iter(|| unsafe {
            create_factory(black_box(cls), &mut objects);
            release_vector(&mut objects);
        })
    });
    c.bench_function("bulk/reserved_create_vector_release", |b| {
        b.iter(|| unsafe {
            create_reserved(black_box(cls), &mut objects);
            release_vector(&mut objects);
        })
    });
}

fn benches(c: &mut Criterion) {
    let cls = class();
    create_release_die(c, cls);
    batch_plain(c, cls);
    batch_batched(c, cls);
    retain_release(c, cls);
    bulk(c, cls);
}

criterion_group!(lifecycle, benches);
criterion_main!(lifecycle);
