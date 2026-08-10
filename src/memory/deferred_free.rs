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
//! What rides the queue: every block kind that reaches `ll_free` and can
//! put memory back in circulation — heap raw buffers, entity slots,
//! pooled large, OS-direct runs and retained blocks (the last for the
//! reason given below) — and **buffer-arena chunks**, which do not reach
//! `ll_free` at all. A chunk is freed
//! by `buffer_arena::buffer_free_longlived_payload` calling
//! `BufferArena::free` directly, so it never passes `ll_free`'s test;
//! that branch does its own, and parks the whole call rather than the
//! link write, because `free` also decrements the block's live count
//! and can hand an emptied block back to the pool to be re-stamped as
//! another kind. Parked chunks are the reason a parked record carries a
//! size at all: `BufferArena::free` is size-carrying, the chunk itself
//! holds no metadata, and the block header would be gone by flush time
//! anyway. The third rider is a **payload in a retained block**, which
//! arrives from the same function and hands back no memory of its own —
//! what it may hand back is the block those bytes pinned
//! (`retained::payload_freed`). A record therefore names the free it
//! replays rather than deriving it from a size. A string payload and an
//! array's table storage both live in
//! buffer chunks, and since `walk::trace_cells` gained its Array arm the
//! walker strides an array's entries inside its chunk — so a chunk freed
//! mid-epoch is memory a walker may be reading, which is what parking it
//! answers.
//!
//! What does not ride it: the arena kind, which recycles nothing, so
//! identity holds without parking. A **retained** block rides it, and
//! not for the usual reason: nothing is recycled *inside* such a
//! block — former arena memory has neither stride nor free list — but
//! the death of its last live occupant hands the whole block to the
//! pool (`retained::occupant_freed`), and a block reissued mid-epoch is
//! exactly the identity loss the queue exists to prevent. The free of a
//! payload such a block was pinned for reaches the pool the same way and
//! parks for the same reason.
//!
//! **A cross-thread free rides it like any other.** The epoch test in
//! `stdapi::ll_free` fires on the block kind alone and stands *before*
//! the owner dispatch, so during an epoch a free of another thread's heap
//! or entity slot parks on the freeing thread and reaches `free_foreign`
//! only when [`release`] replays it at the flush. The crate is
//! single-mutator today, so nothing depends on that ordering yet; actors
//! will reopen the question, and the answer to read then is this
//! paragraph rather than the block below, which is about where the flush
//! runs.
//!
//! Known limit: a thread that parks and exits before flushing leaks its
//! parked list until process end — bounded by what that thread freed
//! inside one epoch window. **Measured in blocks, not bytes**, once
//! buffer chunks ride it: a dropped 16-byte chunk record leaves `live`
//! above zero on its block forever, and a block never empties, so it
//! bounces between the abandoned list and its adopters instead of going
//! home. One record can therefore pin 64 KiB — and a large-entity run
//! raises that ceiling to the run's own size, which the class decides:
//! 192 KiB for the ten-thousand-property instance
//! `rfc/model/memory/large-entities.md` measures, 3.2 MB for the
//! 200 000-property one, unbounded in general. A dropped run record also
//! keeps its registry entry, so the collector walks it once per epoch
//! for the life of the process.
//!
//! Two obligations on the epoch protocol (build step 3, commit 4):
//! the collector must **publish the flag through a handshake before
//! snapshotting** — a mutator that has not yet observed the flag can
//! still recycle a slot the snapshot is about to include — and the
//! flush runs **on the owning thread** after the epoch closes, because
//! the parked list is thread-local and the underlying frees are
//! owner-bound.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

/// The GC activity bit: an epoch is in flight, park instead of freeing.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// A parked release: the pointer, and the free it owes.
#[derive(Clone, Copy)]
struct Parked {
    ptr: *mut u8,
    free: DeferredFree,
}

/// Which free a parked record replays, named rather than inferred from a
/// size: the three differ in what they hand back, and only one of them
/// needs a size at all.
#[derive(Clone, Copy)]
enum DeferredFree {
    /// Anything that reaches `ll_free`, which reads what it needs from
    /// the block header: an entity slot, a heap buffer, a pooled large
    /// block, an OS-direct run.
    Allocation,
    /// A buffer-arena chunk, with the granted capacity its free takes as
    /// an argument — a chunk carries no metadata of its own
    /// (`buffer_arena.rs`, the zero-metadata contract).
    Chunk { capacity: usize },
    /// A payload lying in a retained block. Nothing about the bytes
    /// waits, former arena memory having no free list; what waits is the
    /// block they pin ([`park_retained_payload`]).
    RetainedPayload,
}

thread_local! {
    /// This thread's parked allocations, in park order. Out-of-band:
    /// the parked memory itself is never touched (module doc).
    ///
    /// A raw pointer in a `Cell`, not a `RefCell<Vec<_>>`, and the
    /// reason is soundness rather than speed: a `Vec` has drop glue, so
    /// its key is registered for TLS destruction, and this list is
    /// reached **from** a TLS destructor — thread exit runs the
    /// static-block teardown (`static_block.rs`), whose deaths reach
    /// `flush_due` through the epoch checkpoint. TLS destructor order
    /// is unspecified and on glibc is reverse registration order, which
    /// puts the exit guard last precisely because it registers first,
    /// so this list is reliably already gone. `with` would then panic
    /// with `AccessError` inside a destructor, and a panic there cannot
    /// unwind: the process aborts. A `Cell<*mut _>` has no drop glue,
    /// is never registered, and stays readable for the whole life of
    /// the thread; [`dispose`] frees it explicitly.
    static PARKED: Cell<*mut Vec<Parked>> = const { Cell::new(std::ptr::null_mut()) };
}

/// This thread's list, allocated on first use.
fn parked_list() -> *mut Vec<Parked> {
    PARKED.with(|cell| {
        let mut list = cell.get();
        if list.is_null() {
            list = Box::into_raw(Box::new(Vec::new()));
            cell.set(list);
        }
        list
    })
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
    unsafe {
        (*parked_list()).push(Parked {
            ptr,
            free: DeferredFree::Allocation,
        })
    };
}

/// Park the free of a payload lying in a retained block. Nothing about
/// the bytes waits — former arena memory has no free list — but the block
/// they pin may become empty, and handing a block to the pool while a
/// walker still holds addresses inside it is what this queue exists to
/// prevent.
///
/// # Safety
/// `ptr` must be a live payload inside a retained block, freed by this
/// call and reachable by nothing until [`flush`] replays it.
pub(crate) unsafe fn park_retained_payload(ptr: *mut u8) {
    unsafe {
        (*parked_list()).push(Parked {
            ptr,
            free: DeferredFree::RetainedPayload,
        })
    };
}

/// Park a buffer-arena chunk, which does not pass `ll_free` and whose
/// free needs the granted capacity back. The whole call is parked, not
/// just the free-list link it would write: `BufferArena::free` also
/// decrements the block's live count, and an emptied block returns to
/// the global pool and can be re-stamped as another kind while the
/// walker still holds addresses inside it.
///
/// # Safety
/// `(ptr, capacity)` must be exactly one live chunk of this thread's
/// buffer arena, freed by this call and reachable by nothing until
/// [`flush`] releases it.
pub(crate) unsafe fn park_buffer_chunk(ptr: *mut u8, capacity: usize) {
    debug_assert!(capacity > 0, "a chunk's free is size-carrying");
    unsafe {
        (*parked_list()).push(Parked {
            ptr,
            free: DeferredFree::Chunk { capacity },
        })
    };
}

/// Release one parked record through the free path it names.
///
/// # Safety
/// Runs on the parking thread, with no epoch in flight.
unsafe fn release(p: Parked) {
    match p.free {
        DeferredFree::Allocation => unsafe { crate::memory::stdapi::ll_free(p.ptr) },
        DeferredFree::Chunk { capacity } => unsafe {
            crate::memory::buffer_arena::free_parked_chunk(p.ptr, capacity)
        },
        DeferredFree::RetainedPayload => {
            let block = crate::memory::block_pool::BlockHeader::of_ptr(p.ptr) as usize;
            if crate::memory::retained::payload_freed(block) {
                unsafe { crate::memory::retained::give_block_back(block) };
            }
        }
    }
}

/// Release this thread's parked backlog through the real free path.
/// Returns how many allocations were flushed, and **zero without
/// touching the backlog if an epoch is in flight**: flushing mid-epoch
/// would recycle the very slots the queue exists to pin, and the caller
/// cannot rule that out on its own (below). Frees in reverse park order,
/// so the free lists come
/// out as the intrusive-list draft left them (LIFO — tests and cache
/// behaviour rely on last-freed-first-reused).
///
/// # Safety
/// Must run on the thread that parked the allocations (the list and
/// the underlying frees are both thread-bound).
pub(crate) unsafe fn flush() -> usize {
    // Not an assertion, because the caller's [`flush_due`] cannot hold:
    // the bit is global, the collector raises it from its own thread, and
    // it flips between that read and this one often enough to be measured
    // (the regression test below). An epoch that opened in that window has
    // not been acked by this thread yet — `Epoch::open` raises the bit
    // before requesting the handshake, and the snapshot waits for the ack
    // — so nothing has read the slots yet and skipping is free: the
    // backlog goes at the next checkpoint. Recycling anyway would be the
    // one thing this queue exists to prevent.
    if active() {
        return 0;
    }
    let list = PARKED.with(|cell| cell.get());
    if list.is_null() {
        return 0;
    }
    let backlog = unsafe { std::mem::take(&mut *list) };
    for &parked in backlog.iter().rev() {
        unsafe { release(parked) };
    }
    backlog.len()
}

/// A closed epoch left parked memory on this thread: the checkpoint's
/// flush trigger. Two cheap reads (one global, one thread-local).
#[inline]
pub(crate) fn flush_due() -> bool {
    if active() {
        return false;
    }
    let list = PARKED.with(|cell| cell.get());
    !list.is_null() && unsafe { !(*list).is_empty() }
}

/// Give this thread's list back at thread exit, after the last flush.
///
/// Called from `ll_thread_exit` rather than from a TLS destructor,
/// which is the whole point (see the `PARKED` doc). Flushes what is
/// still parked when no epoch is in flight; when one is, the backlog
/// leaks, which is the known limit the module doc already declares —
/// freeing mid-epoch would recycle the very slots the queue pins.
///
/// Null-tolerant and idempotent: a thread that never parked, and a
/// second call, both find nothing.
pub(crate) fn dispose() {
    let list = PARKED.with(|cell| cell.replace(std::ptr::null_mut()));
    if list.is_null() {
        return;
    }
    let backlog = unsafe { Box::from_raw(list) };
    if !active() {
        for &parked in backlog.iter().rev() {
            unsafe { release(parked) };
        }
    }
}

/// This thread's parked backlog size (stats, tests).
#[cfg(test)]
pub(crate) fn parked_count() -> usize {
    let list = PARKED.with(|cell| cell.get());
    if list.is_null() {
        0
    } else {
        unsafe { (*list).len() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The activity bit is global and the collector runs on another
    /// thread, so it can be raised between a caller's [`flush_due`] and
    /// the flush itself. Measured under contention: the flag flipped
    /// inside that window on roughly one check in nine. The flush must
    /// leave the backlog alone when that happens rather than assert.
    #[test]
    fn an_epoch_opening_under_the_flush_leaves_the_backlog_parked() {
        let _g = crate::memory::block_pool::test_guard();
        let a = unsafe { crate::memory::stdapi::ll_malloc(64) };
        begin_epoch();
        unsafe { crate::memory::stdapi::ll_free(a) };
        assert_eq!(parked_count(), 1, "parked while the epoch is in flight");
        end_epoch();

        // The caller's `flush_due()` said yes here; the collector opens
        // the next epoch before the flush runs.
        assert!(flush_due());
        begin_epoch();
        assert_eq!(unsafe { flush() }, 0, "nothing recycled mid-epoch");
        assert_eq!(parked_count(), 1, "and the backlog is still there");

        end_epoch();
        assert_eq!(unsafe { flush() }, 1, "released at the next checkpoint");
    }
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
            assert_eq!(
                (*(addr as *const RcHeader)).refcount,
                0,
                "occupancy: reads free"
            );
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
        assert_eq!(
            class_word, cls as usize,
            "the class word sits at bytes 8-15"
        );

        begin_epoch();
        unsafe {
            assert!(ll_release(obj as *mut RcHeader));
            crate::object::ll_object_die(obj);
        }
        assert_eq!(parked_count(), 1);
        unsafe {
            assert_eq!(
                (*(addr as *const RcHeader)).refcount,
                0,
                "header: reads free"
            );
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

    /// A buffer-arena chunk never passes `ll_free`, so the epoch test
    /// that parks everything else used to miss it entirely:
    /// `BufferArena::free` wrote its `{ next, size }` link straight into
    /// the freed chunk — which is where a string payload's bytes and an
    /// array's table storage are, and what a walker chasing either would
    /// read — and could hand an emptied block back to the pool mid-epoch.
    #[test]
    fn a_buffer_chunk_parks_instead_of_being_written_into() {
        let _g = crate::memory::block_pool::test_guard();
        use crate::memory::buffer::Buffer;
        use crate::memory::buffer_arena::{buffer_ensure_longlived, buffer_free_longlived_payload};

        let mut buf = Buffer::new();
        let data = buffer_ensure_longlived(&mut buf, 64, 0);
        assert!(!data.is_null());
        let capacity = buf.capacity;
        // Payload bytes, which is what a walker chasing this chunk reads.
        unsafe { std::ptr::write_bytes(data, 0xA5, capacity) };

        begin_epoch();
        unsafe { buffer_free_longlived_payload(data, capacity) };
        assert_eq!(parked_count(), 1, "the chunk parked with its capacity");
        let head = unsafe { std::slice::from_raw_parts(data, 16) };
        assert!(
            head.iter().all(|&b| b == 0xA5),
            "the free-list link was written into a chunk the walker may be reading"
        );

        end_epoch();
        assert_eq!(unsafe { flush() }, 1, "released for real at the flush");
        assert_eq!(parked_count(), 0);
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

    /// The two large-entity kinds park for the same reason and one
    /// stronger: a run is **unmapped** at its free, and an epoch's
    /// snapshot holds its address, so an unparked free would leave the
    /// collector reading memory the system allocator has taken back
    /// rather than an intact corpse.
    #[test]
    fn a_large_entity_parks_whichever_half_it_is() {
        let _g = crate::memory::block_pool::test_guard();
        unsafe {
            let pooled = crate::memory::large_entity::alloc(20_000);
            let run = crate::memory::large_entity::alloc(200_000);
            assert!(!pooled.is_null() && !run.is_null());
            let run_block = (run as usize) & !crate::memory::block_pool::BLOCK_MASK;

            begin_epoch();
            ll_free(pooled);
            ll_free(run);
            assert_eq!(parked_count(), 2, "neither half recycles mid-epoch");
            assert!(
                crate::memory::large_entity::snapshot().contains(&run_block),
                "a parked run is still addressable, so it is still registered"
            );
            end_epoch();
            assert_eq!(flush(), 2);
            assert!(
                !crate::memory::large_entity::snapshot().contains(&run_block),
                "and its registry entry went with the flush"
            );
        }
    }
}
