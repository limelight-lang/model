//! A body outside the entity's own slot leaves the arena by one of
//! two routes: an in-block payload or storage is copied, its block
//! going back to the pool, and one over a block payload is an
//! OS-direct run whose record the arena forgets, so the address does
//! not move and nothing can refuse it. A refused copy retains the
//! block instead, and the payload's own free is what hands it back.
//! An entity that had a block to itself keeps it, unstamped and out
//! of the log; one nothing carried out is freed with the reset.

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

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let owner =
        unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

    // The mixed vector, which is what the factory stamps and therefore
    // what an arena array carries when the reset finds it. The ordered
    // hash's carry is the two tests below.
    let storage_before = unsafe {
        assert!(crate::array::testing::push(array, Value::int(11)));
        assert!(crate::array::testing::push(array, Value::int(22)));
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
            crate::array::testing::at(array, 0).unwrap().as_int(),
            11,
            "the carried storage lost its elements"
        );
        assert_eq!(crate::array::testing::at(array, 1).unwrap().as_int(), 22);
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

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let owner =
        unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
    let array = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };

    let storage_before = unsafe {
        for i in 0..1100i64 {
            crate::array::testing::insert(array, Key::Int(i), Value::int(i));
        }

        crate::array::entity::storage_address(array)
    };

    assert!(
        unsafe { crate::array::testing::storage_and_capacity(array).1 } > BLOCK_PAYLOAD,
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
            assert_eq!(
                crate::array::testing::get(array, Key::Int(i))
                    .unwrap()
                    .as_int(),
                i
            );
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

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let owner =
        unsafe { new_constructed(&mut *context_ptr, owner_cls, MemoryCategory::RequestArena) };
    let array = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };

    let storage_before = unsafe {
        crate::array::testing::insert(array, Key::Int(1), Value::int(11));
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
    let s = unsafe { crate::string::ll_string_new(ctx_ptr, MemoryCategory::RequestArena, &content) }
        as *mut RcHeader;
    assert!(!s.is_null());
    assert!(
        crate::string::bytes_are_out_of_line(unsafe { crate::refcount::mutator_flags(s) }),
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

/// A survivor in a block of its own that dies inside the reset is not
/// read again by it. Its memory is not the ordinary retained kind, which
/// stays mapped precisely so a dead occupant's refcount word can be read
/// as zero: a run comes from `std::alloc` at 128 KiB and up, which glibc
/// returns to the system, so every later reader of that address is
/// reading something the process no longer owns.
///
/// The reset has two such readers over its survivor list, and both run
/// after the release drain — `reconcile_cow_counts` on the entity and
/// `index_retained_blocks` on the entity's block header.
#[test]
fn a_large_survivor_that_dies_inside_the_reset_is_read_no_further() {
    use crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN;
    let _g = crate::memory::block_pool::test_guard();

    let wide = crate::test_support::wide_class("WideDyingInsideTheReset", RUN_FILLERS, None);
    let corpse_cls = ClassBuilder::new("WideCorpse").prop("box", true).build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let obj = unsafe { new_constructed(&mut *context_ptr, wide, MemoryCategory::RequestArena) };
    let corpse =
        unsafe { new_constructed(&mut *context_ptr, corpse_cls, MemoryCategory::RequestArena) };
    let block = BlockHeader::of_ptr(obj as *const u8) as usize;
    assert_eq!(
        unsafe { block_kind(obj as *const u8) },
        BLOCK_KIND_ENTITY_LARGE_RUN,
        "the entity fits in a shared block, so this test proves nothing"
    );

    unsafe {
        // The escape is into a heap box, and the box goes into an arena
        // slot: that is what logs the box's release against the reset,
        // and the release is what kills the promoted survivor while the
        // reset is still running.
        let boxed = crate::reference::ll_reference_new();
        assert!(ref_store(
            arena_ptr,
            boxed as *mut RcHeader,
            &raw mut (*boxed).value,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, obj as *mut RcHeader),
        ));
        let slot = Object::prop_at(corpse, 16);
        assert!(ref_store(
            arena_ptr,
            corpse as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Reference, boxed as *mut RcHeader),
        ));
        assert!(!crate::refcount::ll_release(boxed as *mut RcHeader));
    }

    unsafe { arena_reset_full(&mut *arena_ptr) };

    assert!(
        !crate::memory::large_entity::snapshot().contains(&block),
        "the run outlived the entity it was allocated for"
    );
    assert!(
        !crate::memory::retained::snapshot()
            .iter()
            .any(|(b, _)| *b == block),
        "a run was entered into the retained index, which ends at the \
         64 KiB block pool"
    );
}

/// A class whose counted cells lie outside the object body owns its
/// storage the way an array owns its chunk, and the reset reaches that
/// storage through the group rather than through the entity kind. An
/// escapee's promotion is the moment it matters: the object becomes a
/// heap entity while its block is still arena memory, and the pages go
/// back at the end of the reset.
///
/// The cell's own child is what makes the copy observable — the carry
/// moves the bytes, so a walk of the promoted object has to find the
/// same child at the new address.
#[test]
fn a_hooked_survivor_carries_its_block_out_of_the_arena() {
    use crate::memory::block_pool::{BLOCK_KIND_ARENA, BLOCK_KIND_BUFFER};
    use crate::test_support::outside_block;
    let _g = crate::memory::block_pool::test_guard();

    let holder_cls = ClassBuilder::new("WakerHolder").prop("waker", true).build();
    let waker_cls = outside_block::class("WakerPromoted");
    let child_cls = ClassBuilder::new("WakerPromotedChild").build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let waker =
        unsafe { new_constructed(&mut *context_ptr, waker_cls, MemoryCategory::RequestArena) };
    let child = unsafe { new_constructed(&mut *context_ptr, child_cls, MemoryCategory::GcHeap) };

    let block_before = unsafe { outside_block::install_block(context_ptr, waker) };
    assert_eq!(
        unsafe { block_kind(block_before) },
        BLOCK_KIND_ARENA,
        "the block was drawn under some other category than the instance's"
    );

    unsafe {
        assert!(outside_block::store_cell(
            arena_ptr,
            waker,
            0,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, child as *mut RcHeader),
        ));
        // The escape: a heap holder takes the arena object, which is the
        // only reason anything here survives.
        store_prop(arena_ptr, holder, 16, waker);
        assert!(!crate::refcount::ll_release(child as *mut RcHeader));
    }

    unsafe { arena_reset_full(&mut *arena_ptr) };

    unsafe {
        assert_eq!(
            (*(waker as *mut RcHeader)).memory_category(),
            MemoryCategory::GcHeap,
            "the waker stayed behind in the dying arena"
        );

        let block_after = outside_block::block_of(waker);
        assert_ne!(block_after, block_before, "the block never moved");
        assert_eq!(
            block_kind(block_after),
            BLOCK_KIND_BUFFER,
            "the storage is still arena memory the reset gave back"
        );

        let mut seen = Vec::new();
        crate::cells::trace_entity(waker as *mut RcHeader, |c| seen.push(c));
        assert_eq!(
            seen,
            vec![child as *mut RcHeader],
            "the cell lost its child in the move"
        );

        // The promoted waker is an ordinary counted object now: the
        // holder's death releases it, and its own teardown gives the
        // carried block back.
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// The other half: an instance nothing holds dies with the reset, and
/// its storage needs no free of its own — the category rule is what
/// makes that true, the block being arena memory like the entity's own.
/// A block drawn from anywhere else would still be out here, with no
/// pointer left to it.
#[test]
fn a_hooked_corpse_leaves_nothing_behind() {
    use crate::memory::block_pool::BLOCK_KIND_ARENA;
    use crate::test_support::outside_block;
    let _g = crate::memory::block_pool::test_guard();

    let waker_cls = outside_block::class("WakerAbandoned");
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let waker =
        unsafe { new_constructed(&mut *context_ptr, waker_cls, MemoryCategory::RequestArena) };
    let block = unsafe { outside_block::install_block(context_ptr, waker) };
    let block_address = BlockHeader::of_ptr(block) as usize;
    assert_eq!(
        unsafe { block_kind(block) },
        BLOCK_KIND_ARENA,
        "the corpse's block is the arena's, so the pages are its free"
    );

    unsafe { arena_reset_full(&mut *arena_ptr) };

    assert!(
        !crate::memory::retained::snapshot()
            .iter()
            .any(|(b, _)| *b == block_address),
        "the reset held the block for a payload nobody carried out"
    );
}

/// A refused carry answers the bytes it left behind, and the reset pins
/// the block holding them instead of handing it back. The address comes
/// out of the same call that refused, so nothing can pin the block of an
/// entity other than this one — and the promoted instance's own teardown
/// is what spends the pin.
#[test]
fn a_refused_hooked_carry_pins_the_block_its_bytes_lie_in() {
    use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_KIND_RETAINED};
    use crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED;
    use crate::test_support::outside_block;
    use std::sync::atomic::Ordering;
    let _g = crate::memory::block_pool::test_guard();

    let holder_cls = ClassBuilder::new("RefusedWakerHolder")
        .prop("waker", true)
        .build();
    let waker_cls = outside_block::class("WakerRefused");

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let waker =
        unsafe { new_constructed(&mut *context_ptr, waker_cls, MemoryCategory::RequestArena) };
    let block_before = unsafe { outside_block::install_block(context_ptr, waker) };
    let block_address = BlockHeader::of_ptr(block_before) as usize;

    unsafe { store_prop(arena_ptr, holder, 16, waker) };

    FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
    unsafe { arena_reset_full(&mut *arena_ptr) };
    FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);

    unsafe {
        assert_eq!(
            outside_block::block_of(waker),
            block_before,
            "the carry was not refused, so this test proves nothing"
        );
        assert_eq!(
            *(block_address as *const u32),
            BLOCK_KIND_RETAINED,
            "a refused carry did not retain the block its bytes lie in"
        );
        // The kind alone proves nothing here: this block holds the
        // survivor too, so it is retained for an occupant whatever the
        // carry answered. The pin is the refusal's own mark.
        assert_eq!(
            crate::memory::retained::pinned_payloads(block_address),
            1,
            "the refusal named no block, so nothing pinned this one"
        );

        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
        assert_eq!(
            *(block_address as *const u32),
            BLOCK_KIND_FREE,
            "the block outlived the storage it was pinned for"
        );
    }
}

/// The pinned block stays out of circulation when the payload's own free
/// arrives **inside** the reset that pinned it. The pin is spent then,
/// and the occupant count that would still hold the block does not exist
/// yet: the index is built after the fixpoint, so a block emptied of
/// payloads reads as empty of everything.
///
/// The shape that frees a payload that early is the heap box behind `&`
/// (`memory::retained::register`): the box's logged release kills the
/// promoted survivor during the drain, and that survivor's teardown frees
/// the very bytes the refusal pinned the block for. Two other survivors
/// of the same block are what the block must still be held for.
#[test]
fn a_pin_spent_inside_the_reset_leaves_the_block_to_its_survivors() {
    use crate::memory::block_pool::BLOCK_KIND_RETAINED;
    use crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED;
    use crate::test_support::outside_block;
    use std::sync::atomic::Ordering;
    let _g = crate::memory::block_pool::test_guard();

    let holder_cls = ClassBuilder::new("PinnedNeighbourHolder")
        .prop("first", true)
        .prop("second", true)
        .build();
    let neighbour_cls = ClassBuilder::new("PinnedNeighbour").build();
    let corpse_cls = ClassBuilder::new("PinnedCorpse").prop("box", true).build();
    let waker_cls = outside_block::class("WakerFreedInsideTheReset");

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let first = unsafe {
        new_constructed(
            &mut *context_ptr,
            neighbour_cls,
            MemoryCategory::RequestArena,
        )
    };
    let second = unsafe {
        new_constructed(
            &mut *context_ptr,
            neighbour_cls,
            MemoryCategory::RequestArena,
        )
    };
    let waker =
        unsafe { new_constructed(&mut *context_ptr, waker_cls, MemoryCategory::RequestArena) };
    let corpse =
        unsafe { new_constructed(&mut *context_ptr, corpse_cls, MemoryCategory::RequestArena) };

    let block = unsafe { outside_block::install_block(context_ptr, waker) };
    let block_address = BlockHeader::of_ptr(block) as usize;
    for (entity, name) in [
        (first as *const u8, "first"),
        (second as *const u8, "second"),
        (waker as *const u8, "the waker"),
    ] {
        assert_eq!(
            BlockHeader::of_ptr(entity) as usize,
            block_address,
            "{name} was bumped into another block, so the pin it must \
             survive holds nothing of it"
        );
    }

    unsafe {
        // The two neighbours escape into a heap holder and are the
        // survivors the block is still owed to after the pin is spent.
        store_prop(arena_ptr, holder, 16, first);
        store_prop(arena_ptr, holder, 32, second);

        // The waker escapes into a heap reference box instead, and the
        // box goes into an arena slot — which is what logs the box's
        // release against the reset.
        let boxed = crate::reference::ll_reference_new();
        assert!(ref_store(
            arena_ptr,
            boxed as *mut RcHeader,
            &raw mut (*boxed).value,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, waker as *mut RcHeader),
        ));
        let slot = Object::prop_at(corpse, 16);
        assert!(ref_store(
            arena_ptr,
            corpse as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Reference, boxed as *mut RcHeader),
        ));

        // The creation reference goes now, so the logged release is the
        // box's last and the drain is what kills it.
        assert!(!crate::refcount::ll_release(boxed as *mut RcHeader));
    }

    let refusals_before = crate::memory::buffer_arena::refusals();
    FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
    unsafe { arena_reset_full(&mut *arena_ptr) };
    FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);

    // The subject is what the refusal leaves behind, and the entity that
    // was refused is dead by now, so the count is the only thing that
    // tells this run from one where the carry succeeded and no pin was
    // ever taken (`dev/POSTMORTEM.md`, "a forced-refusal test that never
    // proved the refusal").
    assert_eq!(
        crate::memory::buffer_arena::refusals() - refusals_before,
        1,
        "the carry was not refused, so nothing pinned the block"
    );
    assert_eq!(
        unsafe { block_kind(block_address as *const u8) },
        BLOCK_KIND_RETAINED,
        "the block went home while two survivors were still living in it"
    );
    let index = crate::memory::retained::snapshot();
    let (_, occupants) = index
        .iter()
        .find(|(b, _)| *b == block_address)
        .expect("the block kept no index, so no death of its can return it");
    for (entity, name) in [(first, "first"), (second, "second")] {
        assert!(
            occupants.contains(&(entity as usize)),
            "{name} survived outside its own block's index"
        );
    }

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// The end of the case
/// [`a_pin_spent_inside_the_reset_leaves_the_block_to_its_survivors`]
/// builds: the pinned block has no survivor left either, so nobody
/// outside the reset will ever report it empty and the reset hands it
/// over itself. The count it held until the index was built is what
/// makes that a reset-time question rather than a leak.
#[test]
fn a_block_whose_pin_and_occupants_both_go_inside_the_reset_goes_home() {
    use crate::memory::block_pool::BLOCK_KIND_FREE;
    use crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED;
    use crate::test_support::outside_block;
    use std::sync::atomic::Ordering;
    let _g = crate::memory::block_pool::test_guard();

    let corpse_cls = ClassBuilder::new("LonePinCorpse").prop("box", true).build();
    let waker_cls = outside_block::class("WakerAloneInItsBlock");

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let waker =
        unsafe { new_constructed(&mut *context_ptr, waker_cls, MemoryCategory::RequestArena) };
    let corpse =
        unsafe { new_constructed(&mut *context_ptr, corpse_cls, MemoryCategory::RequestArena) };
    let block = unsafe { outside_block::install_block(context_ptr, waker) };
    let block_address = BlockHeader::of_ptr(block) as usize;

    unsafe {
        let boxed = crate::reference::ll_reference_new();
        assert!(ref_store(
            arena_ptr,
            boxed as *mut RcHeader,
            &raw mut (*boxed).value,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, waker as *mut RcHeader),
        ));
        let slot = Object::prop_at(corpse, 16);
        assert!(ref_store(
            arena_ptr,
            corpse as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Reference, boxed as *mut RcHeader),
        ));
        assert!(!crate::refcount::ll_release(boxed as *mut RcHeader));
    }

    let refusals_before = crate::memory::buffer_arena::refusals();
    FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
    unsafe { arena_reset_full(&mut *arena_ptr) };
    FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);

    assert_eq!(
        crate::memory::buffer_arena::refusals() - refusals_before,
        1,
        "the carry was not refused, so nothing pinned the block"
    );
    assert_eq!(
        unsafe { block_kind(block_address as *const u8) },
        BLOCK_KIND_FREE,
        "the block stayed out of the pool with nothing holding it"
    );
    assert!(
        !crate::memory::retained::snapshot()
            .iter()
            .any(|(b, _)| *b == block_address),
        "the block went home with its index still naming it"
    );
}
