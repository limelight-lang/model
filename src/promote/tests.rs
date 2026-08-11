use super::*;
use crate::class::ClassBuilder;
use crate::memory::barrier::ref_store;
use crate::memory::block_pool::BLOCK_KIND_ARENA;
use crate::memory::context::{LLContext, set_current_context};
use crate::object::{ll_object_die, new_constructed};
use crate::refcount::{DESTRUCTOR_PENDING, DESTRUCTOR_RAN};
use crate::test_support::RUN_FILLERS;
use crate::value::{Tag, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Entity pointer behind a Box slot, or null for scalar/null Boxes.
fn entity_checked(v: &Value) -> *mut RcHeader {
    if v.is_refcounted() {
        v.entity_ptr()
    } else {
        std::ptr::null_mut()
    }
}

/// Store `value` into `holder`'s slot at `offset` through the real
/// barrier, as generated code would.
unsafe fn store_prop(arena: *mut Arena, holder: *mut Object, offset: u32, value: *mut Object) {
    unsafe {
        let slot = Object::prop_at(holder, offset);
        let old = entity_checked(&*slot);
        let new = if value.is_null() {
            Value::null()
        } else {
            Value::entity(Tag::Object, value as *mut RcHeader)
        };

        assert!(ref_store(arena, holder as *mut RcHeader, slot, old, new));
    }
}

/// Survival comes from the escape count rather than from a
/// remembered set of holder slots: a reset with no escapes returns
/// every block, an escapee survives with the count its live holders
/// justify, and the children behind it come out with it. The counter
/// is what makes a stale entry impossible — an overwritten slot
/// leaves no survivor behind, and a holder that died before the
/// reset already dropped its count, so nothing dereferences a freed
/// slot.
mod who_survives_a_reset {
    use super::*;

    #[test]
    fn no_escapes_returns_every_block() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Temp").prop("x", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        let block = BlockHeader::of_ptr(obj as *const u8);

        unsafe { arena_reset_full(&mut arena) };

        // The block went home: a fresh arena must get it back.
        let mut second = Arena::new();
        let p = second.alloc(8);
        assert_eq!(BlockHeader::of_ptr(p), block);
        second.reset(|_| {});
    }

    #[test]
    fn escaped_object_survives_with_exact_count_and_retained_block() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Session").prop("x", true).build();
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe { store_prop(&mut arena, holder, 16, obj) };
        let block = BlockHeader::of_ptr(obj as *const u8);
        assert_eq!(
            unsafe { (*block).kind.load(Ordering::Relaxed) },
            BLOCK_KIND_ARENA
        );

        unsafe { arena_reset_full(&mut arena) };

        let o = unsafe { &*obj };
        assert_eq!(
            o.rc.memory_category(),
            MemoryCategory::GcHeap,
            "recategorized in place"
        );
        assert_eq!(o.rc.refcount, 1, "exactly the one external reference");
        assert_eq!(o.rc.flags & ARENA_RESET_MARK, 0, "transient mark cleared");
        assert_eq!(
            unsafe { (*block).kind.load(Ordering::Relaxed) },
            BLOCK_KIND_RETAINED
        );

        // The survivor is an ordinary counted object now: its one
        // reference is the holder's slot, so the holder's death
        // releases it and cascades into the survivor's own teardown.
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    /// An arena referent behind a surviving reference box outlives the
    /// reset, and comes out of it with exactly one holder.
    ///
    /// **What carries it is the escape count, since the box moved to the
    /// heap** (S3.1): storing an arena object into a heap box is a
    /// crossing, so the object is an escapee in its own right and the
    /// reset promotes it from the escapee log. The test was written for a
    /// different mechanism — promotion gated recursion on `is_object`, so
    /// every other kind was a leaf and the arena object behind an *arena*
    /// `&` was never marked, dying with the reset while a promoted box
    /// still pointed at it. That configuration cannot be built any more,
    /// because no box is an arena entity; the assertions below are worth
    /// keeping for the survival and the count, not as a guard on the
    /// recursion.
    #[test]
    fn a_surviving_reference_box_carries_its_referent() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Node").prop("x", true).build();
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let target = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        let r = crate::reference::ll_reference_new();

        unsafe {
            assert!(ref_store(
                &mut arena,
                r as *mut RcHeader,
                &raw mut (*r).value,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, target as *mut RcHeader),
            ));
            // The heap holder takes the box, which is what keeps the box
            // — and through it the referent — reachable past the reset.
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                &mut arena,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Reference, r as *mut RcHeader),
            ));
        }

        unsafe { arena_reset_full(&mut arena) };

        assert_eq!(
            unsafe { (*(target as *mut RcHeader)).memory_category() },
            MemoryCategory::GcHeap,
            "the referent stayed behind in the dying arena"
        );
        assert_eq!(
            unsafe { (*(target as *mut RcHeader)).refcount },
            1,
            "the box's slot is its one holder"
        );
    }

    #[test]
    fn internal_edges_survive_and_are_counted() {
        let _g = crate::memory::block_pool::test_guard();
        let node = ClassBuilder::new("Node").prop("next", true).build();
        let holder_cls = ClassBuilder::new("Root").prop("head", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
        let b = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };

        unsafe {
            store_prop(&mut arena, a, 16, b); // arena→arena: no logs
            store_prop(&mut arena, holder, 16, a); // escape of a (and b transitively)
            arena_reset_full(&mut arena);
        }

        unsafe {
            assert_eq!((*a).rc.memory_category(), MemoryCategory::GcHeap);
            assert_eq!((*b).rc.memory_category(), MemoryCategory::GcHeap);
            assert_eq!((*a).rc.refcount, 1, "one external reference");
            assert_eq!((*b).rc.refcount, 1, "one internal edge from a");
        }
    }

    #[test]
    fn overwritten_slot_is_stale_and_only_the_final_target_survives() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Val").build();
        let holder_cls = ClassBuilder::new("One").prop("v", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe {
            store_prop(&mut arena, holder, 16, a); // logged
            store_prop(&mut arena, holder, 16, b); // same slot, logged again
            arena_reset_full(&mut arena);
        }

        unsafe {
            assert_eq!((*b).rc.memory_category(), MemoryCategory::GcHeap);
            assert_eq!((*b).rc.refcount, 1, "deduplicated: one slot, one count");
            // `a` was conservatively marked but is unreferenced: floating
            // garbage of this reset, never a dangling pointer.
        }
    }

    /// Regression for the remembered-set dangle (C2): a heap holder can die
    /// before the arena resets. The old design logged holder *slots* and
    /// read them back at reset, so a freed holder's slot was dereferenced
    /// (and its stale contents re-counted). The escape counter never reads
    /// a slot: the holder's teardown already dropped the count (`lose`), so
    /// reset sees the true, live external count.
    #[test]
    fn holder_death_before_reset_neither_dangles_nor_miscounts() {
        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("Box").prop("v", true).build();
        let val_cls = ClassBuilder::new("Val").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let h1 = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let h2 = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, val_cls, MemoryCategory::RequestArena) };

        unsafe {
            // A escapes into two heap holders: hold-count 2.
            store_prop(&mut arena, h1, 16, a);
            store_prop(&mut arena, h2, 16, a);
            assert_eq!((*a).rc.refcount, 2, "two heap holders");

            // H1 dies before reset. Its teardown drops the count (lose) and
            // frees its memory — including the slot that held A. The old
            // slot-based reset would read that freed slot and re-count A to
            // 2; the counter leaves the count at exactly 1.
            assert!(crate::refcount::ll_release(h1 as *mut RcHeader));
            ll_object_die(h1);
            assert_eq!((*a).rc.refcount, 1, "H1's death dropped the count");

            arena_reset_full(&mut arena);

            // A survived (H2 holds it), promoted with exactly one
            // reference, and no freed slot was ever dereferenced.
            assert_eq!(
                (*a).rc.memory_category(),
                MemoryCategory::GcHeap,
                "promoted"
            );
            assert_eq!((*a).rc.refcount, 1, "exactly H2's reference, not two");

            // H2 dies for real: A cascades to teardown.
            assert!(crate::refcount::ll_release(h2 as *mut RcHeader));
            ll_object_die(h2);
        }
    }
}

/// A body outside the entity's own slot leaves the arena by one of
/// two routes: an in-block payload or storage is copied, its block
/// going back to the pool, and one over a block payload is an
/// OS-direct run whose record the arena forgets, so the address does
/// not move and nothing can refuse it. A refused copy retains the
/// block instead, and the payload's own free is what hands it back.
/// An entity that had a block to itself keeps it, unstamped and out
/// of the log; one nothing carried out is freed with the reset.
mod the_memory_a_survivor_takes_with_it {
    use super::*;

    /// An arena array reached from an escaping object takes its storage
    /// with it. The route matters: an array is a COW entity, so it never
    /// escapes on its own — the barrier copies a COW value out of the
    /// arena instead — and it becomes a survivor only as a **child** of
    /// something that did escape. That child edge is what the array's
    /// tracing arm added, and this is the first thing to walk it.
    ///
    /// Without the carry the storage goes back to the block pool at the
    /// reset while the promoted array still points at it, so the array
    /// reads whatever the next owner of those bytes writes.
    #[test]
    fn an_array_reached_from_an_escapee_carries_its_storage_out() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK};
        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();
        let owner_cls = ClassBuilder::new("Owner").prop("items", true).build();

        // One raw pointer per arena and per context, reused: `ll_array_new`
        // resolves the arena from the mounted context rather than taking
        // one, and a fresh `&mut` per call would retag the pointer parked
        // in TLS (`dev/WORKFLOW.md`, Miri).
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut context = LLContext { arena: arena_ptr };
        let context_ptr: *mut LLContext = &mut context;
        crate::memory::context::set_current_context(context_ptr);

        let holder =
            unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
        let owner =
            unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
        let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

        let storage_before = unsafe {
            (*array).table.insert(
                crate::array::entity::category_of(array),
                Key::Int(1),
                Value::int(11),
            );
            (*array).table.insert(
                crate::array::entity::category_of(array),
                Key::Int(2),
                Value::int(22),
            );
            crate::array::entity::storage_address(array)
        };

        assert!(!storage_before.is_null());

        unsafe {
            // The array into the arena owner: same category on both sides,
            // so no escape copy is asked for.
            let slot = Object::prop_at(owner, 16);
            assert!(ref_store(
                arena_ptr,
                owner as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
            // The heap holder takes the owner: this is the escape, and the
            // only reason anything here survives.
            store_prop(arena_ptr, holder, 16, owner);
        }

        unsafe { arena_reset_full(&mut *arena_ptr) };
        crate::memory::context::set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                (*(array as *mut RcHeader)).memory_category(),
                MemoryCategory::GcHeap,
                "the array stayed behind in the dying arena"
            );
            let storage_after = crate::array::entity::storage_address(array);
            let kind = crate::memory::block_pool::load_block_kind(
                ((storage_after as usize) & !BLOCK_MASK) as *const std::sync::atomic::AtomicU32,
            );
            assert_eq!(
                kind, BLOCK_KIND_BUFFER,
                "the storage is still arena memory the reset gave back"
            );
            assert_eq!(
                (*array).table.get(Key::Int(1)).unwrap().as_int(),
                11,
                "the carried storage lost its entries"
            );
            assert_eq!((*array).table.get(Key::Int(2)).unwrap().as_int(), 22);
        }
    }

    /// The other route out: a storage larger than a block payload is an
    /// OS-direct run the arena logged, and carrying it is making the arena
    /// forget the record rather than copying anything. Getting that wrong
    /// is not a leak but a use-after-free — the reset frees every logged
    /// run, and the promoted array would go on reading the freed memory.
    /// The address is therefore unchanged, which is the observable.
    #[test]
    fn an_over_block_storage_transfers_instead_of_being_copied() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::memory::block_pool::BLOCK_PAYLOAD;
        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();
        let owner_cls = ClassBuilder::new("Owner").prop("items", true).build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut context = LLContext { arena: arena_ptr };
        let context_ptr: *mut LLContext = &mut context;
        crate::memory::context::set_current_context(context_ptr);

        let holder =
            unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
        let owner =
            unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
        let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

        let storage_before = unsafe {
            for i in 0..1100i64 {
                (*array).table.insert(
                    crate::array::entity::category_of(array),
                    Key::Int(i),
                    Value::int(i),
                );
            }

            crate::array::entity::storage_address(array)
        };

        assert!(
            unsafe { (*array).table.storage_and_capacity().1 } > BLOCK_PAYLOAD,
            "the table never grew past one block, so this proves nothing"
        );

        unsafe {
            let slot = Object::prop_at(owner, 16);
            assert!(ref_store(
                arena_ptr,
                owner as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
            store_prop(arena_ptr, holder, 16, owner);
        }

        unsafe { arena_reset_full(&mut *arena_ptr) };
        crate::memory::context::set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                crate::array::entity::storage_address(array),
                storage_before,
                "an OS-direct storage was copied instead of transferred"
            );
            for i in 0..1100i64 {
                assert_eq!((*array).table.get(Key::Int(i)).unwrap().as_int(), i);
            }
        }
    }

    /// A carry the buffer arena refuses leaves the bytes where they are,
    /// and the reset keeps their block out of circulation instead. The
    /// block is then held by a payload rather than by occupants, and what
    /// hands it back is the payload's own free — the promoted array's
    /// death. Before 2026-08-08 the pin was permanent and the block was
    /// gone for the life of the process; the test was seen failing on the
    /// kind still reading retained after the array died.
    ///
    /// The refusal is aimed at one allocation rather than at the pool:
    /// `FORCE_OOM` leaves the buffer arena free to serve the carry from a
    /// block it already owns or adopts, which made this test pass 35
    /// times in 40 and prove nothing the other five. The assertion that
    /// the storage did not move is what says the refusal landed where the
    /// test needs it.
    #[test]
    fn a_refused_carry_pins_the_block_and_the_payload_frees_it() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_MASK};
        use crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED;
        use std::sync::atomic::Ordering;
        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("RefusedCache").prop("last", true).build();
        let owner_cls = ClassBuilder::new("RefusedOwner")
            .prop("items", true)
            .build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut context = LLContext { arena: arena_ptr };
        let context_ptr: *mut LLContext = &mut context;
        crate::memory::context::set_current_context(context_ptr);

        let holder =
            unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
        let owner =
            unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
        let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

        let storage_before = unsafe {
            (*array).table.insert(
                crate::array::entity::category_of(array),
                Key::Int(1),
                Value::int(11),
            );
            crate::array::entity::storage_address(array)
        };

        assert!(!storage_before.is_null());
        let payload_block = (storage_before as usize) & !BLOCK_MASK;

        unsafe {
            let slot = Object::prop_at(owner, 16);
            assert!(ref_store(
                arena_ptr,
                owner as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
            store_prop(arena_ptr, holder, 16, owner);
        }

        FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
        unsafe { arena_reset_full(&mut *arena_ptr) };
        FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);
        crate::memory::context::set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                crate::array::entity::storage_address(array),
                storage_before,
                "the carry was not refused, so this test proves nothing"
            );
            assert_eq!(
                *(payload_block as *const u32),
                crate::memory::block_pool::BLOCK_KIND_RETAINED,
                "a refused carry did not retain the block its bytes lie in"
            );

            // The promoted array dies with its holder, and its storage is
            // freed into a block that has been waiting for exactly that.
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
            assert_eq!(
                *(payload_block as *const u32),
                BLOCK_KIND_FREE,
                "the block outlived the payload it was pinned for"
            );
        }
    }

    /// An oversize arena string survives with its holder instead of being
    /// copied out. The store that put it there is arena→arena, so no
    /// barrier saw it, and the escape that follows promotes the whole
    /// subgraph — so a copy-on-write string does reach the payload carry,
    /// which until the layout split only the proved-single-owner form
    /// could (`rfc/model/memory/large-entities.md`).
    #[test]
    fn an_oversize_cow_arena_string_carries_its_payload_through_promotion() {
        let _g = crate::memory::block_pool::test_guard();

        let holder_cls = ClassBuilder::new("Cache").prop("keep", true).build();
        let keeper_cls = ClassBuilder::new("Keeper").prop("s", true).build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        let keeper = unsafe { new_constructed(ctx_ptr, keeper_cls, MemoryCategory::RequestArena) };
        let content = vec![b'p'; crate::memory::block_pool::BLOCK_PAYLOAD];
        let s = unsafe {
            crate::string::ll_string_new(ctx_ptr, MemoryCategory::RequestArena, &content)
        } as *mut RcHeader;
        assert!(!s.is_null());
        assert_ne!(
            unsafe { crate::refcount::header_flags(s) } & crate::refcount::STRING_OUT_OF_LINE,
            0,
            "out of line, or the payload carry is not on the path"
        );

        unsafe {
            let slot = Object::prop_at(keeper, 16);
            assert!(ref_store(
                arena_ptr,
                keeper as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::String, s),
            ));
            assert!(!crate::refcount::ll_release(s));
            store_prop(arena_ptr, holder, 16, keeper);
            arena_reset_full(arena_ptr);
        }

        set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                (*s).memory_category(),
                MemoryCategory::GcHeap,
                "promoted with its holder rather than copied at the barrier"
            );
            assert_eq!(
                crate::string::string_bytes(s as *const crate::string::LLString),
                &content[..],
                "and the payload came with it, wherever it now lives"
            );

            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            crate::object::ll_entity_die(holder as *mut RcHeader);
        }
    }

    /// A survivor that had a block to itself keeps it, and the three
    /// rules the reset applies to one are what make that safe. The stamp
    /// is the silent one: `BLOCK_KIND_RETAINED` on a run sends a 128 KiB
    /// OS allocation to the 64 KiB block pool when the entity finally
    /// dies, and nothing between the reset and that death looks wrong.
    #[test]
    fn a_promoted_large_entity_keeps_its_block_and_leaves_the_arenas_log() {
        let _g = crate::memory::block_pool::test_guard();
        let wide = crate::test_support::wide_class("WideSession", RUN_FILLERS, None);
        let holder_cls = ClassBuilder::new("Cache").prop("last", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let obj = unsafe { new_constructed(&mut ctx, wide, MemoryCategory::RequestArena) };

        let block = BlockHeader::of_ptr(obj as *const u8) as usize;
        assert_eq!(
            unsafe { *(block as *const u32) },
            crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN,
            "the arena's entity door gave it a run of its own"
        );

        unsafe { store_prop(&mut arena, holder, 16, obj) };
        unsafe { arena_reset_full(&mut arena) };

        unsafe {
            assert_eq!(
                (*obj).rc.memory_category(),
                MemoryCategory::GcHeap,
                "recategorized in place, like any other survivor"
            );
            assert_eq!(
                *(block as *const u32),
                crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN,
                "stamped retained, and the death below would push a run \
                 onto the block pool"
            );
        }

        assert!(
            !crate::memory::retained::snapshot()
                .iter()
                .any(|(b, _)| *b == block),
            "a block with one computed occupant needs no inventory, and an \
             entry here is the same mistake by the other route"
        );
        assert!(
            crate::memory::large_entity::snapshot().contains(&block),
            "and the registry it was entered into at allocation is what \
             the walk finds it by now that it is a heap entity"
        );

        // The survivor is an ordinary counted object: the holder's death
        // releases it, and its own teardown is what returns the run —
        // which is also the proof that the arena stopped owning it, since
        // a record left in the log would have freed it at the reset.
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }

        assert!(
            !crate::memory::large_entity::snapshot().contains(&block),
            "the run went back with the entity"
        );
    }

    /// The other half of the door's contract: a large arena entity that
    /// nothing carries out is freed by the reset, like every other run
    /// the arena logged.
    #[test]
    fn an_unpromoted_large_arena_entity_is_freed_by_the_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let wide = crate::test_support::wide_class("WideTemp", RUN_FILLERS, None);

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, wide, MemoryCategory::RequestArena) };
        let block = BlockHeader::of_ptr(obj as *const u8) as usize;
        assert!(crate::memory::large_entity::snapshot().contains(&block));

        unsafe { arena_reset_full(&mut arena) };

        assert!(
            !crate::memory::large_entity::snapshot().contains(&block),
            "the corpse's run went with the reset"
        );
    }
}

/// Destructors run inside the settling loop, so the graph moves
/// under it: a store into an already-traced survivor is arena to
/// arena and escapes nothing, which is why the reset watches the
/// bump cursor and re-reads the survivors' children; an escape
/// created there survives although its holder is already
/// destructed; and a release log grown during its own drain is
/// drained again. A COW survivor's count stays readable throughout
/// and is settled once at the end, from the edges that remain plus
/// the holders acquired after promotion.
mod what_a_destructor_does_during_the_fixpoint {
    use super::*;

    #[test]
    fn destructor_created_escape_survives_already_destructed() {
        let _g = crate::memory::block_pool::test_guard();
        static HOLDER_SLOT: AtomicUsize = AtomicUsize::new(0);
        static DTORS: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn escaping_dtor(obj: *mut Object) {
            DTORS.fetch_add(1, Ordering::Relaxed);
            // `$GLOBALS['x'] = $this;` — through the real barrier, with
            // the TLS context (as generated destructor code would).
            let holder = HOLDER_SLOT.load(Ordering::Relaxed) as *mut Object;
            unsafe {
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                let slot = Object::prop_at(holder, 16);
                assert!(ref_store(
                    arena,
                    holder as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Object, obj as *mut RcHeader),
                ));
            }
        }

        let holder_cls = ClassBuilder::new("Globals").prop("x", true).build();
        let cls = ClassBuilder::new("LastWill")
            .destructor(escaping_dtor as *const ())
            .build();

        // One raw pointer per entity, reused — the shape generated code
        // actually has (an `LLContext*` in a register). Taking a fresh
        // `&mut arena`/`&mut ctx` per call would retag, invalidating the
        // pointer `set_current_context` parked in TLS.
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        HOLDER_SLOT.store(holder as usize, Ordering::Relaxed);
        let obj = unsafe { new_constructed(ctx_ptr, cls, MemoryCategory::RequestArena) };

        unsafe { arena_reset_full(arena_ptr) };
        set_current_context(std::ptr::null_mut());

        assert_eq!(DTORS.load(Ordering::Relaxed), 1);
        unsafe {
            assert_eq!(
                (*obj).rc.memory_category(),
                MemoryCategory::GcHeap,
                "the destructor-created escape was caught by the fixpoint"
            );
            assert_eq!((*obj).rc.refcount, 1);
            assert_ne!(
                (*obj).rc.flags & DESTRUCTOR_RAN,
                0,
                "survives already-destructed"
            );
            assert_ne!((*obj).rc.flags & DESTRUCTOR_PENDING, 0);
        }
    }

    /// Regression for H2: a "dirty" destructor stores a *fresh* arena object
    /// into an already-traced survivor. That store is arena→arena, so the
    /// barrier does not escape it; without re-tracing the survivor after a
    /// dirty destructor, the new child is never marked and dangles once the
    /// survivor is promoted. The reset watches the arena bump cursor to know
    /// a destructor allocated, then re-reads the survivors' children.
    #[test]
    fn dirty_destructor_storing_into_a_survivor_traces_the_new_child() {
        let _g = crate::memory::block_pool::test_guard();

        static SURVIVOR: AtomicUsize = AtomicUsize::new(0);
        static NODE_CLS: AtomicUsize = AtomicUsize::new(0);
        static NEW_CHILD: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn mutate_survivor_dtor(_o: *mut Object) {
            let node_cls = NODE_CLS.load(Ordering::Relaxed) as *const crate::class::Class;
            let s = SURVIVOR.load(Ordering::Relaxed) as *mut Object;
            // `$s->next = new Node();` — a fresh arena object stored into an
            // already-traced survivor (arena→arena: not an escape).
            let node = unsafe {
                new_constructed(std::ptr::null_mut(), node_cls, MemoryCategory::RequestArena)
            };

            NEW_CHILD.store(node as usize, Ordering::Relaxed);
            unsafe {
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                store_prop(arena, s, 16, node);
            }
        }

        let node_cls = ClassBuilder::new("Node").prop("next", true).build();
        let holder_cls = ClassBuilder::new("Cache").prop("keep", true).build();
        let trigger_cls = ClassBuilder::new("Trigger")
            .destructor(mutate_survivor_dtor as *const ())
            .build();

        // One raw pointer each, reused (see the note in
        // `destructor_created_escape_survives_already_destructed`): the
        // destructor reenters and resolves this same arena, so the reset
        // must be handed the very pointer the context holds.
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        let s = unsafe { new_constructed(ctx_ptr, node_cls, MemoryCategory::RequestArena) };
        let _trigger =
            unsafe { new_constructed(ctx_ptr, trigger_cls, MemoryCategory::RequestArena) };

        NODE_CLS.store(node_cls as usize, Ordering::Relaxed);
        SURVIVOR.store(s as usize, Ordering::Relaxed);
        NEW_CHILD.store(0, Ordering::Relaxed);

        unsafe {
            // S escapes into the heap holder → it is a survivor.
            store_prop(arena_ptr, holder, 16, s);
            // Trigger is unheld with a destructor (tracked); at reset its
            // destructor stores a fresh Node into survivor S.
            arena_reset_full(arena_ptr);
        }

        set_current_context(std::ptr::null_mut());

        let node = NEW_CHILD.load(Ordering::Relaxed) as *mut Object;
        assert!(!node.is_null(), "the destructor created the child");
        unsafe {
            assert_eq!(
                (*s).rc.memory_category(),
                MemoryCategory::GcHeap,
                "survivor promoted"
            );
            assert_eq!(
                (*node).rc.memory_category(),
                MemoryCategory::GcHeap,
                "the destructor-added child was traced and promoted, not left to die with the arena"
            );
            assert_eq!((*node).rc.refcount, 1, "held once, by the survivor's slot");

            // Teardown cascades holder → s → node with no dangling.
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    /// A COW entity's count is a value, and it stays readable through the
    /// whole fixpoint. Marking a survivor used to zero it, so a destructor
    /// releasing the same string — an ordinary `unset` — decremented from
    /// zero and underflowed inside the reset. The count is settled once
    /// instead, after the last destructor, from the edges that remain.
    #[test]
    fn a_destructor_may_release_a_cow_survivor_during_the_fixpoint() {
        let _g = crate::memory::block_pool::test_guard();

        static DROPPER: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn unset_the_string_dtor(o: *mut Object) {
            // `unset($this->s)` — the store barrier releases the string
            // this object holds, while the reset is still settling.
            unsafe {
                let slot = Object::prop_at(o, 16);
                let old = entity_checked(&*slot);
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                assert!(ref_store(
                    arena,
                    o as *mut RcHeader,
                    slot,
                    old,
                    Value::null()
                ));
            }
        }

        let keeper_cls = ClassBuilder::new("Keeper").prop("s", true).build();
        let holder_cls = ClassBuilder::new("Cache").prop("keep", true).build();
        let dropper_cls = ClassBuilder::new("Dropper")
            .prop("s", true)
            .destructor(unset_the_string_dtor as *const ())
            .build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        let keeper = unsafe { new_constructed(ctx_ptr, keeper_cls, MemoryCategory::RequestArena) };
        let dropper =
            unsafe { new_constructed(ctx_ptr, dropper_cls, MemoryCategory::RequestArena) };
        DROPPER.store(dropper as usize, Ordering::Relaxed);

        let s = unsafe {
            crate::string::ll_string_new(ctx_ptr, MemoryCategory::RequestArena, b"shared")
        } as *mut RcHeader;

        unsafe {
            for owner in [keeper, dropper] {
                let slot = Object::prop_at(owner, 16);
                assert!(ref_store(
                    arena_ptr,
                    owner as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    Value::entity(Tag::String, s),
                ));
            }

            // The creation reference goes, as it would at the end of the
            // statement that built the string.
            assert!(!crate::refcount::ll_release(s));
            assert_eq!((*s).refcount, 2, "both holders, counted as COW demands");

            // Keeper escapes: it survives, and the string with it. Dropper
            // is unheld, so its destructor runs during the fixpoint.
            store_prop(arena_ptr, holder, 16, keeper);
            arena_reset_full(arena_ptr);
        }

        set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                (*s).memory_category(),
                MemoryCategory::GcHeap,
                "the string survived with its keeper"
            );
            assert_eq!(
                (*s).refcount,
                1,
                "one surviving holder: the dead one never released twice"
            );
        }
    }

    /// A holder acquired **after** the survivor was promoted must survive
    /// the reconciliation. Promotion happens inside the settling loop and
    /// the release-log drain runs user destructors after it, so a
    /// destructor can store an already-promoted string into a heap object
    /// that outlives the request — a legitimate `+1` that no edge between
    /// survivors accounts for. Assigning the count from those edges alone
    /// erased it, which left the string with one count and two holders.
    #[test]
    fn a_holder_acquired_after_promotion_keeps_its_count() {
        let _g = crate::memory::block_pool::test_guard();
        static CACHE: AtomicUsize = AtomicUsize::new(0);
        static STRING: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn cache_the_string_dtor(_o: *mut Object) {
            // A dying heap entity, torn down by the release drain, puts the
            // string into a heap object: `Cache::$last = $s`.
            let cache = CACHE.load(Ordering::Relaxed) as *mut Object;
            let s = STRING.load(Ordering::Relaxed) as *mut RcHeader;
            unsafe {
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                let slot = Object::prop_at(cache, 16);
                assert!(ref_store(
                    arena,
                    cache as *mut RcHeader,
                    slot,
                    std::ptr::null_mut(),
                    Value::entity(Tag::String, s),
                ));
            }
        }

        let keeper_cls = ClassBuilder::new("Keeper").prop("s", true).build();
        let holder_cls = ClassBuilder::new("Holder").prop("keep", true).build();
        let cache_cls = ClassBuilder::new("Cache").prop("last", true).build();
        let dying_cls = ClassBuilder::new("Dying")
            .destructor(cache_the_string_dtor as *const ())
            .build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
        let cache = unsafe { new_constructed(ctx_ptr, cache_cls, MemoryCategory::GcHeap) };
        let keeper = unsafe { new_constructed(ctx_ptr, keeper_cls, MemoryCategory::RequestArena) };
        let container =
            unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::RequestArena) };
        let dying = unsafe { new_constructed(ctx_ptr, dying_cls, MemoryCategory::GcHeap) };
        CACHE.store(cache as usize, Ordering::Relaxed);

        let s = unsafe {
            crate::string::ll_string_new(ctx_ptr, MemoryCategory::RequestArena, b"cached")
        } as *mut RcHeader;
        STRING.store(s as usize, Ordering::Relaxed);

        unsafe {
            // The keeper holds the string and escapes, so both survive.
            let slot = Object::prop_at(keeper, 16);
            assert!(ref_store(
                arena_ptr,
                keeper as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::String, s),
            ));
            assert!(!crate::refcount::ll_release(s), "the creation reference");
            store_prop(arena_ptr, holder, 16, keeper);

            // The dying heap entity sits in an arena container, so the
            // release log tears it down — after the promotion pass.
            store_prop(arena_ptr, container, 16, dying);
            assert!(!crate::refcount::ll_release(dying as *mut RcHeader));

            arena_reset_full(arena_ptr);
        }

        set_current_context(std::ptr::null_mut());

        unsafe {
            assert_eq!(
                (*s).refcount,
                2,
                "the keeper's slot and the one the destructor added"
            );
            assert_eq!((*s).memory_category(), MemoryCategory::GcHeap);
        }
    }

    /// Regression for H7: a release-log entity's `__destruct` runs during
    /// the release drain and appends a *new* release-log entry (it stores a
    /// heap reference into a still-alive arena container). The single-pass
    /// reset drained the log once and dropped that late entry, tripping
    /// finish_reset's "logs drained" assert; the settling loop re-drains it.
    #[test]
    fn release_log_grown_during_the_drain_is_still_drained() {
        let _g = crate::memory::block_pool::test_guard();
        static C2_PTR: AtomicUsize = AtomicUsize::new(0);
        static B_PTR: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn a_dtor(_o: *mut Object) {
            // A, dying, stores heap B into the arena container C2 → appends
            // a release-log entry *while the log is being drained*.
            let c2 = C2_PTR.load(Ordering::Relaxed) as *mut Object;
            let b = B_PTR.load(Ordering::Relaxed) as *mut Object;
            unsafe {
                let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
                store_prop(arena, c2, 16, b);
            }
        }

        let cont_cls = ClassBuilder::new("Container").prop("x", true).build();
        let a_cls = ClassBuilder::new("A")
            .destructor(a_dtor as *const ())
            .build();
        let b_cls = ClassBuilder::new("B").build();

        // One raw pointer each, reused: `a_dtor` reenters and resolves
        // this same arena during the release drain.
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        let c1 = unsafe { new_constructed(ctx_ptr, cont_cls, MemoryCategory::RequestArena) };
        let c2 = unsafe { new_constructed(ctx_ptr, cont_cls, MemoryCategory::RequestArena) };
        let a = unsafe { new_constructed(ctx_ptr, a_cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(ctx_ptr, b_cls, MemoryCategory::GcHeap) };

        C2_PTR.store(c2 as usize, Ordering::Relaxed);
        B_PTR.store(b as usize, Ordering::Relaxed);

        unsafe {
            // Heap A into arena container C1 → release-log entry, A retained.
            store_prop(arena_ptr, c1, 16, a);
            // A's only remaining reference is the log's (creator ref dropped).
            assert!(!crate::refcount::ll_release(a as *mut RcHeader));

            // Reset: releasing A runs a_dtor, which appends B's release-log
            // entry mid-drain; the loop must still drain it.
            arena_reset_full(arena_ptr);

            // B was retained by the store and released once by the re-drained
            // log: back to the creator's single reference (not leaked at 2).
            assert_eq!(
                (*b).rc.refcount,
                1,
                "B's late release-log entry was drained"
            );

            assert!(ll_release(b as *mut RcHeader));
            ll_object_die(b);
        }

        set_current_context(std::ptr::null_mut());
    }
}

/// A heap entity stored into an arena container is released by the
/// reset, so a survivor holding one has to compensate that log
/// entry, while a holder that dies takes its child down with it at
/// teardown. Overwriting the last reference to a heap object tears
/// it down at the store rather than leaking it.
mod the_release_log {
    use super::*;

    #[test]
    fn survivor_holding_heap_entity_compensates_the_release_log() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Keeper").prop("cfg", true).build();
        let holder_cls = ClassBuilder::new("Slot").prop("v", true).build();
        let cfg_cls = ClassBuilder::new("Config").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let cfg = unsafe { new_constructed(&mut ctx, cfg_cls, MemoryCategory::GcHeap) };
        let keeper = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };

        unsafe {
            // Heap entity into an arena container: retain + release log.
            store_prop(&mut arena, keeper, 16, cfg);
            assert_eq!((*cfg).rc.refcount, 2);
            // The keeper escapes.
            store_prop(&mut arena, holder, 16, keeper);
            arena_reset_full(&mut arena);
        }

        // Log's -1 and the survivor compensation +1 cancel out: the
        // keeper legitimately holds cfg.
        assert_eq!(unsafe { (*cfg).rc.refcount }, 2);

        // Keeper dies for real, and it dies **through its holder**: the
        // `Slot` object's property is the reference keeping it alive, so
        // releasing behind the holder's back leaves a live object naming
        // freed memory. Only block reuse makes that visible — a freed
        // slot nobody reissues still reads refcount 0, which is what
        // makes the dangling property look harmless.
        unsafe {
            let slot = Object::prop_at(holder, 16);
            assert!(crate::memory::barrier::ref_store(
                &mut arena,
                holder as *mut RcHeader,
                slot,
                keeper as *mut RcHeader,
                Value::null(),
            ));
        }

        assert_eq!(
            unsafe { (*cfg).rc.refcount },
            1,
            "exactly one release at real death"
        );
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    #[test]
    fn heap_entity_of_a_dying_holder_dies_with_teardown() {
        let _g = crate::memory::block_pool::test_guard();
        static CFG_DTORS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn cfg_dtor(_o: *mut Object) {
            CFG_DTORS.fetch_add(1, Ordering::Relaxed);
        }

        let cfg_cls = ClassBuilder::new("DoomedCfg")
            .destructor(cfg_dtor as *const ())
            .build();
        let tmp_cls = ClassBuilder::new("Tmp").prop("cfg", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let cfg = unsafe { new_constructed(&mut ctx, cfg_cls, MemoryCategory::GcHeap) };
        let tmp = unsafe { new_constructed(&mut ctx, tmp_cls, MemoryCategory::RequestArena) };

        unsafe {
            store_prop(&mut arena, tmp, 16, cfg);
            // The test's own reference goes away: the arena holds the last one.
            assert!(!crate::refcount::ll_release(cfg as *mut RcHeader));
            arena_reset_full(&mut arena);
        }

        assert_eq!(
            CFG_DTORS.load(Ordering::Relaxed),
            1,
            "release log's last release must run real teardown"
        );
    }

    /// Overwriting a slot that held the last reference to a heap object
    /// tears that object down (destructor + children + free), rather than
    /// leaking it — the store barrier's displaced-value path.
    #[test]
    fn overwriting_the_last_reference_tears_down_the_displaced_object() {
        let _g = crate::memory::block_pool::test_guard();
        static DTORS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn dtor(_o: *mut Object) {
            DTORS.fetch_add(1, Ordering::Relaxed);
        }

        let val_cls = ClassBuilder::new("Val")
            .destructor(dtor as *const ())
            .build();
        let holder_cls = ClassBuilder::new("Holder").prop("x", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let owner = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, val_cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, val_cls, MemoryCategory::GcHeap) };

        unsafe {
            store_prop(&mut arena, owner, 16, a); // owner->x = a (a.rc 2)
            assert!(!crate::refcount::ll_release(a as *mut RcHeader)); // a.rc 1 (the slot)

            // Overwrite: A's last reference (the slot) goes away → A dies and
            // its destructor runs. The old code released A but never tore it
            // down.
            store_prop(&mut arena, owner, 16, b);
            assert_eq!(
                DTORS.load(Ordering::Relaxed),
                1,
                "displaced A was torn down"
            );

            // cleanup: owner death releases b's slot reference (b.rc 2 → 1),
            // then drop b's creator reference.
            assert!(crate::refcount::ll_release(owner as *mut RcHeader));
            ll_object_die(owner);
            assert!(crate::refcount::ll_release(b as *mut RcHeader));
            ll_object_die(b);
        }
    }
}
