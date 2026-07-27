//! Deferred physical release — the GC activity bit of
//! `rfc/model/gc/heap-design.md`, demanded by the rc-walk collector
//! (`rfc/model/gc/rc-walk.md`, "Deferred physical release, and when an
//! epoch ends"). While a collection epoch is in flight, memory released
//! by ordinary refcount death is **parked rather than recycled**: the
//! entity dies normally and on time, `__destruct` included — only reuse
//! waits, so the walker cannot read a slot that has become a different
//! object underneath it.
//!
//! The queue's real job is **identity**, which makes it soundness, not
//! comfort: an id must name one entity from walk to drain. Without it a
//! slot could be freed and recycled mid-epoch, and the Phase 4 exact
//! test could balance by coincidence on an object that was never
//! judged.
//!
//! Mechanics: one process-global flag, tested with a relaxed load and a
//! predicted branch in `ll_free` after the block-kind dispatch — the
//! single funnel every ordinary local free passes. Parking is
//! **out-of-band**: a thread-local vector of parked pointers, and the
//! parked memory itself is **never written until the flush** (review
//! finding, 2026-07-27, `rfc/model/gc/rc-walk.md`). The first draft
//! threaded an intrusive link through the allocation's bytes 8–15 —
//! which in an entity slot is exactly the class word the walker
//! dereferences one pass after reading the header: a wild read under
//! the walker's feet. Out-of-band, a corpse stays intact — header
//! reading refcount 0 (occupancy), class word live, fields nulled —
//! so a walker chasing a stale pointer lands on readable bytes. The
//! cost: the park path may allocate (a `Vec` push, cold, epoch-only),
//! which the in-slot draft avoided.
//!
//! What rides the queue: all four freeable block kinds (heap raw
//! buffers, entity slots, pooled large, OS-direct runs). The walker
//! today chases only entity slots, but array storage (Phase C) will
//! make it chase raw buffers of any size — parking them all now costs
//! nothing and closes that door in advance. What does not: cross-thread
//! frees (`free_foreign` — the crate is single-mutator; actors will
//! reopen this) and the no-op kinds (arena, retained: nothing is
//! recycled, so identity holds without parking).
//!
//! Known limit: a thread that parks and exits before flushing leaks its
//! parked list until process end — bounded by what that thread freed
//! inside one epoch window.
//!
//! Two obligations on the epoch protocol (build step 3, commit 4):
//! the collector must **publish the flag through a handshake before
//! snapshotting** — a mutator that has not yet observed the flag can
//! still recycle a slot the snapshot is about to include — and the
//! flush runs **on the owning thread** after the epoch closes, because
//! the parked list is thread-local and the underlying frees are
//! owner-bound.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

/// The GC activity bit: an epoch is in flight, park instead of freeing.
static ACTIVE: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// This thread's parked allocations, in park order. Out-of-band:
    /// the parked memory itself is never touched (module doc).
    static PARKED: RefCell<Vec<*mut u8>> = const { RefCell::new(Vec::new()) };
}

/// The free-path test: one relaxed load, a predicted branch.
#[inline]
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Open the deferral window. Caller (the epoch protocol) owes the
/// publication handshake before it snapshots — see the module doc.
pub(crate) fn begin_epoch() {
    let was = ACTIVE.swap(true, Ordering::Relaxed);
    debug_assert!(!was, "epochs do not nest (one verdict in flight, ever)");
}

/// Close the deferral window. Frees after this recycle immediately;
/// the parked backlog waits for [`flush`] on each owning thread.
pub(crate) fn end_epoch() {
    let was = ACTIVE.swap(false, Ordering::Relaxed);
    debug_assert!(was, "end_epoch without begin_epoch");
}

/// Park a freed allocation instead of recycling it. Writes nothing
/// into the allocation — a walker may still be reading it.
///
/// # Safety
/// `ptr` must be a just-freed allocation of a freeable block kind,
/// owned by this call (nothing may recycle it until [`flush`]
/// releases it for real).
pub(crate) unsafe fn park(ptr: *mut u8) {
    PARKED.with(|parked| parked.borrow_mut().push(ptr));
}

/// Release this thread's parked backlog through the real free path.
/// Returns how many allocations were flushed. Runs only between
/// epochs: flushing mid-epoch would recycle the very slots the queue
/// exists to pin. Frees in reverse park order, so the free lists come
/// out as the intrusive-list draft left them (LIFO — tests and cache
/// behaviour rely on last-freed-first-reused).
///
/// # Safety
/// Must run on the thread that parked the allocations (the list and
/// the underlying frees are both thread-bound).
pub(crate) unsafe fn flush() -> usize {
    debug_assert!(!active(), "flush runs only between epochs");
    let backlog = PARKED.with(|parked| parked.take());
    for &ptr in backlog.iter().rev() {
        unsafe { crate::memory::stdapi::ll_free(ptr) };
    }
    backlog.len()
}

/// A closed epoch left parked memory on this thread: the checkpoint's
/// flush trigger. Two cheap reads (one global, one thread-local).
#[inline]
pub(crate) fn flush_due() -> bool {
    !active() && PARKED.with(|parked| !parked.borrow().is_empty())
}

/// This thread's parked backlog size (stats, tests).
#[cfg(test)]
pub(crate) fn parked_count() -> usize {
    PARKED.with(|parked| parked.borrow().len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::memory::arena::Arena;
    use crate::memory::context::LLContext;
    use crate::memory::stdapi::{ll_free, ll_malloc};
    use crate::object::new_constructed;
    use crate::refcount::{MemoryCategory, RcHeader, ll_release};
    use std::sync::atomic::AtomicUsize;

    /// While the window is open a freed raw buffer is parked, not
    /// recycled; after end + flush the allocator hands the slot out
    /// again (the free list is LIFO: same size → same address).
    #[test]
    fn a_parked_buffer_is_not_recycled_until_the_flush() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            // Baseline: LIFO recycling is what "not recycled" is measured
            // against.
            let a = ll_malloc(48);
            ll_free(a);
            let b = ll_malloc(48);
            assert_eq!(a, b, "LIFO baseline: a freed slot is handed out next");

            begin_epoch();
            ll_free(b);
            assert_eq!(parked_count(), 1);
            let c = ll_malloc(48);
            assert_ne!(c, b, "parked: the slot must not be reused mid-epoch");

            end_epoch();
            assert_eq!(flush(), 1);
            assert_eq!(parked_count(), 0);
            let d = ll_malloc(48);
            assert_eq!(d, b, "flushed: the slot is in circulation again");
            ll_free(c);
            ll_free(d);
        }
    }

    /// The entity population rides the same bit: a dying object's slot
    /// parks, its header keeps reading refcount 0 (occupancy — the walk
    /// must not see it), and the flush returns it to the entity heap.
    #[test]
    fn a_dying_entity_parks_and_stays_out_of_the_walk() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("ParkedEntity").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let addr = obj as usize;

        begin_epoch();
        unsafe {
            assert!(ll_release(obj as *mut RcHeader));
            crate::object::ll_object_die(obj);
        }
        assert_eq!(parked_count(), 1);
        unsafe {
            assert_eq!((*(addr as *const RcHeader)).refcount, 0, "occupancy: reads free");
        }
        let mut seen = Vec::new();
        unsafe { crate::memory::heap::for_each_entity_slot(|e| seen.push(e as usize)) };
        assert!(!seen.contains(&addr), "a parked slot is dead to the walk");

        // Not recycled: a same-class allocation must not land on it.
        let other = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        assert_ne!(other as usize, addr);

        end_epoch();
        assert_eq!(unsafe { flush() }, 1);
        // Recycled now: the entity free list is LIFO too.
        let reused = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        assert_eq!(reused as usize, addr, "flushed slot back in circulation");

        unsafe {
            assert!(ll_release(other as *mut RcHeader));
            crate::object::ll_object_die(other);
            assert!(ll_release(reused as *mut RcHeader));
            crate::object::ll_object_die(reused);
        }
        arena.reset(|_| {});
    }

    /// Review finding (2026-07-27): parking must not write the parked
    /// memory. The walker reads a slot's header in one pass and
    /// dereferences the class word at bytes 8–15 in the next; the
    /// in-slot park link of the first draft landed exactly there. A
    /// corpse must stay intact until the flush: header reading 0,
    /// class word live.
    #[test]
    fn parking_leaves_the_corpse_bytes_intact() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("IntactCorpse").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let addr = obj as usize;
        let class_word = unsafe { *((addr + 8) as *const usize) };
        assert_eq!(class_word, cls as usize, "the class word sits at bytes 8-15");

        begin_epoch();
        unsafe {
            assert!(ll_release(obj as *mut RcHeader));
            crate::object::ll_object_die(obj);
        }
        assert_eq!(parked_count(), 1);
        unsafe {
            assert_eq!((*(addr as *const RcHeader)).refcount, 0, "header: reads free");
            assert_eq!(
                *((addr + 8) as *const usize),
                class_word,
                "the class word survives parking — nothing wrote the corpse"
            );
        }
        end_epoch();
        assert_eq!(unsafe { flush() }, 1);
        arena.reset(|_| {});
    }

    /// Only reuse waits: the destructor runs at death, inside the window.
    #[test]
    fn a_parked_death_still_runs_its_destructor_on_time() {
        let _g = crate::memory::block_pool::test_guard();
        static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn counting(_o: *mut crate::object::Object) {
            DESTRUCTS.fetch_add(1, Ordering::Relaxed);
        }
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("ParkedDestructor")
            .destructor(counting as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };

        begin_epoch();
        unsafe {
            assert!(ll_release(obj as *mut RcHeader));
            crate::object::ll_object_die(obj);
        }
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            1,
            "the entity dies on time; only the memory waits"
        );
        end_epoch();
        assert_eq!(unsafe { flush() }, 1);
        arena.reset(|_| {});
    }

    /// The pooled-large and OS-direct kinds park too — array storage of
    /// any size must never be recycled mid-epoch once the walker chases
    /// it (Phase C).
    #[test]
    fn large_and_huge_allocations_park_too() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let large = ll_malloc(20_000); // pooled LARGE
            let huge = ll_malloc(200_000); // OS-direct run
            assert!(!large.is_null() && !huge.is_null());

            begin_epoch();
            ll_free(large);
            ll_free(huge);
            assert_eq!(parked_count(), 2);
            end_epoch();
            assert_eq!(flush(), 2, "both released for real at the flush");
        }
    }
}
