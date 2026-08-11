use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::memory::stdapi::{ll_free, ll_malloc};
use crate::object::new_constructed;
use crate::refcount::{MemoryCategory, RcHeader, ll_release};
use std::sync::atomic::AtomicUsize;

/// Every population a walker addresses parks instead of being
/// recycled: a small heap buffer, an entity slot, a buffer-arena
/// chunk, a pooled-large or OS-direct allocation, and both halves of
/// a large entity — the run half most of all, its memory being
/// unmapped at the free while a snapshot still holds the address.
mod what_parks_while_an_epoch_is_open {
    use super::*;

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

/// Only reuse waits. The destructor runs at the death inside the
/// window, and the parked memory keeps its bytes: the walker reads a
/// slot's header in one pass and dereferences the class word in the
/// next, so a park link written into the slot would be followed as a
/// class pointer.
mod what_parking_may_not_disturb {
    use super::*;

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
}

/// The activity bit is process-wide and the collector raises it on
/// another thread, so it can rise between a caller's `flush_due` and
/// the flush itself. The backlog stays parked when that happens
/// rather than the flush asserting.
mod an_epoch_that_opens_under_the_flush {
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
}
