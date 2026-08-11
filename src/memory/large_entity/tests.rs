use super::*;
use crate::memory::block_pool::{BLOCK_MASK, test_guard};
use crate::refcount::MemoryCategory;
use std::sync::atomic::Ordering;

/// The zero pass is what makes a commissioned block safe, not the
/// publication order: the entity's first bytes read dead until a
/// factory publishes, so a walker may meet the block first.
mod the_commissioning_rule {
    use super::*;

    /// Up to one block payload the allocation is a pooled block: its
    /// kind says so, the entity sits at `+LINE_SIZE`, its header word
    /// reads zero, and no registry entry is made — the region scan
    /// already reaches it.
    #[test]
    fn a_pooled_large_entity_is_block_aligned_and_reads_dead_until_published() {
        let _g = test_guard();
        let before = snapshot().len();

        let entity = alloc(BLOCK_PAYLOAD);
        assert!(!entity.is_null());
        let block = (entity as usize & !BLOCK_MASK) as *mut u8;
        assert_eq!(
            entity as usize - block as usize,
            LINE_SIZE,
            "the entity starts after the header line"
        );
        assert_eq!(
            unsafe {
                (*(block as *const LargeEntityHeader))
                    .kind
                    .load(Ordering::Relaxed)
            },
            BLOCK_KIND_ENTITY_LARGE
        );
        assert_eq!(
            unsafe { *(entity as *const u64) },
            0,
            "the occupancy word is zeroed before the kind is published"
        );
        assert_eq!(unsafe { occupant(block) }, (entity, BLOCK_PAYLOAD));
        assert_eq!(snapshot().len(), before, "a pooled block needs no registry");

        unsafe { free(block, BLOCK_KIND_ENTITY_LARGE) };
    }
}

/// The whole point of the module: an entity past what its category
/// packs keeps its inline layout as the sole occupant, and lives and
/// dies through the ordinary doors.
mod an_entity_that_fills_its_own_block {
    use super::*;

    /// The case that opened the stage, through the real factory: a class
    /// of 10 000 declared properties, which PHP 8.6.0-dev runs at 163 840
    /// bytes an instance. On this layout that is 160 016 bytes, so the
    /// object takes a run of three blocks, and every one of its slots is
    /// where the class put it — the point of keeping the inline layout
    /// whole rather than indirecting the cells.
    #[test]
    fn an_object_of_ten_thousand_properties_is_built_read_and_torn_down() {
        let _g = test_guard();
        let mut builder = crate::class::ClassBuilder::new("WideClass");
        let names: Vec<String> = (0..10_000).map(|i| format!("p{i}")).collect();
        for name in &names {
            // Boxed, so a slot is the 16 bytes the measurement counted; a
            // scalar class is half the size and lands in a two-block run,
            // where the last slot's offset is no longer where this test
            // writes.
            builder = builder.prop(name, true);
        }

        let class = builder.build();
        assert_eq!(
            unsafe { (*class).object_size } as usize,
            16 + 10_000 * 16,
            "the header and ten thousand Box slots"
        );

        let mut arena = crate::memory::arena::Arena::new();
        let mut ctx = crate::memory::context::LLContext { arena: &mut arena };
        unsafe {
            let obj = crate::object::new_constructed(&mut ctx, class, MemoryCategory::GcHeap);
            assert!(!obj.is_null(), "a wide class is instantiable");
            let block = (obj as usize & !BLOCK_MASK) as *mut u8;
            assert_eq!(
                (*(block as *const LargeEntityHeader))
                    .kind
                    .load(Ordering::Relaxed),
                BLOCK_KIND_ENTITY_LARGE_RUN
            );

            // The first and the last slot the class laid out, which is
            // what a lost header line or a short round-up would take out.
            let first = crate::object::Object::prop_at(obj, 16);
            let last = crate::object::Object::prop_at(obj, 16 + 9_999 * 16);
            assert_eq!(
                last as usize + 16 - obj as usize,
                (*class).object_size as usize,
                "the last slot ends where the class says the object does"
            );
            first.write(crate::value::Value::int(-1));
            last.write(crate::value::Value::int(9_999));
            assert_eq!(first.read().as_int(), -1);
            assert_eq!(last.read().as_int(), 9_999);

            assert!(crate::refcount::ll_release(
                obj as *mut crate::refcount::RcHeader
            ));
            crate::object::ll_entity_die(obj as *mut crate::refcount::RcHeader);
            assert!(
                !snapshot().contains(&(block as usize)),
                "and its run left the registry with it"
            );
        }

        arena.reset(|_| {});
    }

    /// A second free of the same pooled block does nothing. It is the one
    /// mistake here that corrupts rather than leaks: `BlockPool::put`
    /// twice threads the block into the free chain twice, and the second
    /// taker gets memory the first is already using. What stops it is
    /// that `put` re-stamps the kind, so the second `ll_free` reads
    /// `BLOCK_KIND_FREE` and falls through the arm that tolerates it.
    #[test]
    fn a_second_free_of_a_pooled_large_entity_does_nothing() {
        let _g = test_guard();
        let entity = alloc(20_000);
        assert!(!entity.is_null());
        let block = (entity as usize & !BLOCK_MASK) as *mut u8;

        unsafe {
            crate::memory::stdapi::ll_free(entity);
            assert_eq!(
                (*(block as *const LargeEntityHeader))
                    .kind
                    .load(Ordering::Relaxed),
                crate::memory::block_pool::BLOCK_KIND_FREE,
                "the pool re-stamps on the way in, which is what makes the \
                 second free legible"
            );
            crate::memory::stdapi::ll_free(entity);
        }

        // The proof that the chain is intact: a block comes back out and
        // is the one that went in.
        let again = alloc(20_000);
        assert_eq!(
            (again as usize & !BLOCK_MASK) as *mut u8,
            block,
            "the freed block is handed out once, not twice"
        );
        unsafe { free(block, BLOCK_KIND_ENTITY_LARGE) };
    }
}

/// A pooled block and an OS-direct run share a shape and nothing
/// else: exhaustion of one leaves the other alone, and only the run
/// half is found through the registry.
mod the_two_halves_are_separate_populations {
    use super::*;

    /// Exhaustion is reported, not absorbed — and the two halves draw on
    /// different allocators, which is what makes the refusal legible:
    /// with the pool refusing, the pooled half is null while the run half
    /// is served, so a null here can only have come from the pool.
    #[test]
    fn the_pooled_half_reports_pool_exhaustion_and_the_run_half_is_unaffected() {
        let _g = test_guard();
        crate::memory::block_pool::FORCE_OOM.store(true, std::sync::atomic::Ordering::Relaxed);

        let pooled = alloc(BLOCK_PAYLOAD);
        let run = alloc(BLOCK_PAYLOAD + 1);

        crate::memory::block_pool::FORCE_OOM.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(pooled.is_null(), "the pool refused and the refusal carried");
        assert!(
            !run.is_null(),
            "the run comes from the system allocator, which was not refusing"
        );

        let block = (run as usize & !BLOCK_MASK) as *mut u8;
        unsafe { free(block, BLOCK_KIND_ENTITY_LARGE_RUN) };
    }

    /// One byte more and the allocation is a run: outside every region,
    /// so the registry is what makes it findable, and the entry goes
    /// when the memory does.
    #[test]
    fn a_run_is_registered_while_it_lives_and_gone_after_it_is_freed() {
        let _g = test_guard();
        let size = BLOCK_PAYLOAD + 1;

        let entity = alloc(size);
        assert!(!entity.is_null());
        let block = (entity as usize & !BLOCK_MASK) as *mut u8;
        assert_eq!(
            unsafe {
                (*(block as *const LargeEntityHeader))
                    .kind
                    .load(Ordering::Relaxed)
            },
            BLOCK_KIND_ENTITY_LARGE_RUN
        );
        assert_eq!(
            entity as usize - block as usize,
            LINE_SIZE,
            "the entity starts after the header line here too — the block
             round-up leaves slack at the tail, so no write in this test
             would fault on a lost line"
        );
        assert!(
            snapshot().contains(&(block as usize)),
            "a run is enumerated from the registry or not at all"
        );
        assert_eq!(unsafe { occupant(block) }.1, size);

        // The last byte the entity claims is inside the allocation: a
        // round-up that lost the header line would fault here.
        unsafe { entity.add(size - 1).write(0xab) };

        unsafe { free(block, BLOCK_KIND_ENTITY_LARGE_RUN) };
        assert!(
            !snapshot().contains(&(block as usize)),
            "and the entry goes before the memory"
        );
    }
}
