//! How a death is classified: stacked where this collection met the block,
//! returned at once where it did not.
//!
//! Each of the four populations is put through both arms, and the stack is
//! read off byte 8 of the dead entities; the block's own words — occupancy,
//! kind, the retained count, the run registry — are what say a return was
//! withheld rather than made. Beside the four stand the reset's whole-block
//! sentinel, which has no entity header, and a stamped block another thread
//! owns.

use super::*;

/// A death in a block the trace has stamped is held in the slot itself: no
/// block is drawn and nothing is recorded.
///
/// This is the whole of the withholding path, with no process end anywhere on
/// it and no memory asked of anyone (`dev/DECISIONS.md`, "one stack through
/// the dead entity holds every withheld return").
///
/// **The header says nothing about the withholding.** `ll_free` takes the slot
/// of a withheld death and of a returned one alike, so what pins this path is the walk
/// and the allocation below: the census passes over the slot and the class
/// cannot hand it out. What returns it is the close, which is
/// `the_close_returns_a_withheld_slot`'s subject.
#[test]
fn a_stamped_slot_is_stacked() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!victim.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let block = block_of(victim);

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), victim, 1) };
    assert!(!row.is_null());
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "the row stamped the victim's own block"
    );

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };

    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds the withheld return"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and nothing was drawn to hold it, which is what takes the abort off this path"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot is neither live nor free"
    );
    assert_eq!(
        unsafe { crate::refcount::header_refcount(victim) },
        0,
        "and its count still reads zero, which is what a queue reader depends on"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before,
        "the block's occupancy falls at the return and not at the withholding"
    );

    // The walker every census goes through passes over it, and the class it
    // belongs to hands it to nobody: the slot is on no free list and below its
    // block's bump cursor.
    let mut live_in_block = 0;
    unsafe {
        crate::memory::heap::for_each_entity_slot(|slot| {
            if block_of(slot) == block {
                live_in_block += 1;
            }
        });
    }

    assert_eq!(
        live_in_block, 0,
        "the walk passes over a zero-count slot, withheld or not"
    );
    let served = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!served.is_null());
    assert_ne!(
        served, victim as *mut u8,
        "the slot is on no free list and below its block's bump cursor, so the \
         allocator cannot reach it"
    );
    assert!(
        crate::memory::heap::describe_slot(victim as usize).contains("state DeadInPlace"),
        "and the slot describes itself as neither live nor free"
    );

    drop(window);

    // The allocator is the witness, not the state word: the return re-enters
    // `ll_free`, which takes the slot again on its way to the free list, so the
    // word reads alike on both sides of the close
    // (`crate::refcount::DEAD_IN_PLACE`).
    let after_close = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_eq!(
        after_close, victim as *mut u8,
        "the close returned the slot the stack held, and the class hands it out \
         again — the return itself is `the_close_returns_a_withheld_slot`'s subject"
    );

    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    unsafe { dead_entity(after_close) };
    unsafe { crate::memory::stdapi::ll_free(after_close) };
}

/// The stack is threaded through byte 8 of each dead entity: the newest
/// withheld slot holds the address of the one below it there, and the oldest
/// holds null.
///
/// The case reads that word off the offset `memory::heap` names rather than
/// through the module's own helper, because the claim is about the two links
/// sharing one word: the return of a slotted death writes its free-list link
/// exactly there, which is why the pop reads the link before it hands the slot
/// over.
#[test]
fn a_withheld_return_names_the_one_below_it_through_byte_eight() {
    let _guard = test_guard();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 4) };
    let first = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 4) };
    let second = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 4) };
    assert!(!keeper.is_null() && !first.is_null() && !second.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let first = unsafe { live_entity(first, 1) };
    let second = unsafe { live_entity(second, 1) };
    let block = block_of(first);
    assert_eq!(
        block_of(second),
        block,
        "one row stamps the block both deaths stand in"
    );

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), first, 1) };

    for entity in [first, second] {
        unsafe { crate::refcount::set_header_refcount(entity, 0) };
        unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
    }

    assert_eq!(deferred_slot_count(), 2, "both deaths were withheld");

    assert_eq!(
        unsafe { link_of(second) },
        first as *mut u8,
        "the newest withheld slot names the one below it"
    );
    assert!(
        unsafe { link_of(first) }.is_null(),
        "and the oldest names null, which is what ends the pop"
    );

    drop(window);

    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 2,
        "the close returned both, which the block's own count is what shows: each \
         return re-enters `ll_free` and is taken again, so the state word reads \
         alike on both sides of the close"
    );

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// The retained population takes the same word: a survivor withheld after
/// another names it through byte 8 of its own header.
///
/// A retained survivor has no size class behind it — its bytes are the
/// object's own, rounded to eight by the arena it was promoted out of — so
/// what says the link fits is the class's own size, asserted in
/// `retained_survivors` before the promotion. This case reads the link
/// back; the room is the fixture's assertion, an overrun into the next
/// survivor's header reading the same value here.
#[test]
fn a_withheld_retained_survivor_names_the_one_below_it_through_byte_eight() {
    let _guard = test_guard();
    let (_arena, holders, survivors, block) = unsafe { retained_survivors::<2>() };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    for survivor in survivors {
        unsafe { ensure_row(window.arena(), survivor, 1) };
    }

    // The holders stand in a block no row addresses, so their own deaths are
    // returned at once and the stack holds the two survivors alone.
    for holder in holders {
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    assert_eq!(deferred_slot_count(), 2, "both survivors were withheld");

    assert_eq!(
        unsafe { link_of(survivors[1]) },
        survivors[0] as *mut u8,
        "the survivor withheld second names the one withheld first"
    );
    assert!(
        unsafe { link_of(survivors[0]) }.is_null(),
        "and the first names null, which is what ends the pop"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned both survivors, which emptied their block"
    );
}

/// A death in a block no row of this collection addresses is returned at
/// once.
///
/// The block's shadow pointer is the whole test, and this is the case that
/// justifies the load: a block this collection never touched carries no row
/// for any of its slots, so a new occupant of this one inherits nothing and
/// the window has nothing to withhold.
#[test]
fn an_unstamped_block_is_returned_at_once() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    // A stamped block has to stand for the shadow-pointer test to be doing any
    // work: with none in the process the case passes for an implementation
    // that reads the wrong block's shadow, or that asks whether a window is
    // open at all.
    let stamped = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!stamped.is_null());
    let stamped = unsafe { live_entity(stamped, 1) };

    // Another size class, so the death below comes from a block of its own
    // and the row addresses none of it; and a keeper in that block, so the
    // return does not empty it and hand it to the pool under the reads that
    // follow.
    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let over_capacity = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!over_capacity.is_null());

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), stamped, 1) };
    assert!(!row.is_null());

    let block = block_of(over_capacity);
    let stamped_block = block_of(stamped);
    assert_ne!(
        block, stamped_block,
        "the two deaths are in different blocks"
    );
    assert!(
        !unsafe { crate::memory::heap::block_shadow(stamped_block) }.is_null(),
        "the row stamped its own block"
    );
    assert!(
        unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "and no row addresses this one"
    );

    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };

    // With both allocation paths refusing, so that the answer is read as the
    // one that asks for nothing rather than as the one that got lucky.
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    unsafe { dead_entity(over_capacity) };
    unsafe { crate::memory::stdapi::ll_free(over_capacity) };
    drop(oom);

    assert_eq!(
        deferred_slot_count(),
        0,
        "a death in memory this collection never met is returned at once"
    );
    assert_eq!(gc_blocks(), held_before, "and a block was drawn to hold it");
    assert_eq!(
        unsafe { crate::refcount::slot_state(over_capacity as *const RcHeader) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot reads as one `ll_free` holds, the return itself being what the \
         block's count below shows"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before - 1,
        "the return reached the block's own count"
    );

    let served = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert_eq!(
        served, over_capacity,
        "the slot reached its free list inside the window, which is what \
         separates a return from a withholding of any kind"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close drew a block");

    unsafe { crate::refcount::set_header_refcount(stamped, 0) };
    unsafe { crate::memory::stdapi::ll_free(stamped as *mut u8) };
    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    crate::memory::critical::drain_for_test();
}

/// A death in a stamped block **this thread does not own** is stacked like
/// every other: the close returns it through the block's own stack of
/// cross-thread frees, and walks no slot of the block, such a walk being
/// bounded by a cursor its owner moves.
///
/// The block is `abandoned_block_of`'s, in a class of this case's own, so
/// that the adoption at the end has nowhere to draw from but the block's own
/// stack of cross-thread frees, which is what says the return was made rather
/// than dropped.
#[test]
fn a_slot_of_a_stamped_block_this_thread_does_not_own_is_stacked() {
    const CLASS: usize = ENTITY_SIZE * 6;

    let _guard = test_guard();

    let held = abandoned_block_of(CLASS);

    let foreign = held[held.len() / 2] as *mut RcHeader;
    let block = block_of(foreign);
    assert!(
        !unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) },
        "the block belongs to a heap that no longer exists"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), foreign, 1) };
    assert!(!row.is_null());
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block) }.is_null(),
        "the row stamped the foreign block, so only ownership separates this case \
         from `a_stamped_slot_is_stacked`"
    );

    assert!(
        !unsafe { crate::memory::heap::block_is_owned_by_this_thread(block) },
        "the fill left the foreign block unadopted"
    );

    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(foreign, 0) };
    unsafe { crate::memory::stdapi::ll_free(foreign as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        1,
        "the return was withheld rather than made"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot reads as one `ll_free` holds"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before,
        "the owner has not heard of the death, so its count still holds the slot \
         and the block cannot reach the pool while the return is withheld"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot is neither live nor free, which is the whole of what the word \
         says on either side of a return; the return itself is the adoption \
         below"
    );

    // The return itself: the block is full, so the adoption below has nothing
    // to serve from but the cross-thread frees the close posted onto it
    // (`crate::memory::heap::Heap::alloc_block_full`). A close that popped
    // the slot and returned nothing leaves this drawing a fresh block.
    let served = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert_eq!(
        served, foreign as *mut u8,
        "the close popped the slot without making the return it deferred"
    );

    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    for slot in held {
        let slot = slot as *mut RcHeader;
        if slot == foreign {
            continue;
        }

        unsafe { crate::refcount::set_header_refcount(slot, 0) };
        unsafe { crate::memory::stdapi::ll_free(slot as *mut u8) };
    }
}

/// A retained survivor dying in a block the trace has stamped is stacked like
/// a slotted death, and its block still counts it: the count is what the
/// withheld return owes, so the block cannot go home while the return is
/// withheld.
///
/// The holder is given a row of its own, so that its own death is withheld
/// beside the survivor's rather than returned at once.
#[test]
fn a_stamped_retained_survivor_is_stacked() {
    let _guard = test_guard();
    let (_arena, [holder], [survivor], block) = unsafe { retained_survivors::<1>() };
    let held_before = gc_blocks();

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), survivor, 1) };
    assert_eq!(unsafe { shadow::count(*row) }, 1);
    unsafe { ensure_row(window.arena(), holder as *mut RcHeader, 1) };
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block as *mut u8) }.is_null(),
        "the row stamped the retained block"
    );

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        deferred_slot_count(),
        2,
        "the survivor and its holder are both on the stack"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and nothing was drawn to hold them, which is what takes the abort off this path"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(survivor) },
        crate::refcount::SlotState::DeadInPlace,
        "the survivor is neither live nor free"
    );
    assert_eq!(
        unsafe { crate::refcount::header_refcount(survivor) },
        0,
        "and its count still reads zero"
    );
    assert_eq!(
        unsafe { crate::refcount::slot_state(holder as *const RcHeader) },
        crate::refcount::SlotState::DeadInPlace,
        "and the holder's own slot was withheld beside it rather than returned"
    );
    assert_eq!(
        unsafe { crate::memory::retained::held_occupant_count(block as usize) },
        1,
        "a withheld survivor is a counted survivor: its decrement is what the \
         withheld return owes"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "so the block is not the pool's while the return is withheld"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned both, and the survivor's spent the last hold \
         on the block (`the_close_returns_a_withheld_retained_survivor`)"
    );
}

/// A pooled large entity dying under a row of its own is stacked, and its
/// block stays out of the pool until the withheld return is made.
#[test]
fn a_stamped_pooled_large_entity_is_stacked() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE
    );

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds the withheld return"
    );
    assert_eq!(gc_blocks(), held_before, "and nothing was drawn to hold it");
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the one slot the block holds is neither live nor free"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE,
        "so the block is not the pool's while the return is withheld"
    );

    drop(window);
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the close returned the withheld entity, and the block with it \
         (`the_close_returns_a_withheld_pooled_large_entity`)"
    );
}

/// An OS-direct run dying under a row of its own is stacked, and the mapping
/// stands until the withheld return is made.
#[test]
fn a_stamped_run_is_stacked() {
    let _guard = test_guard();
    let entity = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!entity.is_null());
    let entity = unsafe { dead_entity(entity) };
    let block = BlockHeader::of_ptr(entity as *const u8) as usize;
    assert!(crate::memory::large_entity::snapshot().contains(&block));

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), entity, 0) };

    unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };

    assert_eq!(
        deferred_slot_count(),
        1,
        "the stack holds the withheld return"
    );
    assert_eq!(gc_blocks(), held_before, "and nothing was drawn to hold it");
    assert_eq!(
        unsafe { crate::refcount::slot_state(entity) },
        crate::refcount::SlotState::DeadInPlace,
        "the one slot the run holds is neither live nor free"
    );
    assert!(
        crate::memory::large_entity::snapshot().contains(&block),
        "so the mapping stands while the return is withheld"
    );

    drop(window);
    assert!(
        !crate::memory::large_entity::snapshot().contains(&block),
        "the close returned the withheld entity, which unmapped the run \
         (`the_close_returns_a_withheld_run`)"
    );
}

/// A large entity dying in a block no trace has met is unmapped at once.
///
/// Its own block header word is the stamp, and this is the case that gives
/// that word its work: an untouched row is one no new occupant could inherit,
/// there being no new occupant of a run at all, so the window has nothing to
/// hold the run for.
#[test]
fn an_unmet_large_entity_is_returned_at_once() {
    let _guard = test_guard();

    // A met large entity has to stand, or the case passes for an
    // implementation that reads another block's row or asks only whether a
    // window is open at all.
    let met = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!met.is_null());
    let met = unsafe { live_entity(met, 1) };
    let met_block = block_of(met);

    let unmet = crate::memory::large_entity::alloc(crate::memory::block_pool::BLOCK_PAYLOAD + 1);
    assert!(!unmet.is_null());
    let unmet = unsafe { dead_entity(unmet) };
    let unmet_block = BlockHeader::of_ptr(unmet as *const u8) as usize;

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), met, 1) };
    assert_ne!(
        shadow::color(unsafe { *crate::memory::large_entity::shadow_row(met_block) }),
        shadow::Color::Untouched,
        "the row met its own block"
    );
    assert_eq!(
        shadow::color(unsafe { *crate::memory::large_entity::shadow_row(unmet_block as *mut u8) }),
        shadow::Color::Untouched,
        "and no row met this one"
    );

    // With both allocation paths refusing, so that the answer is read as the
    // one that asks for nothing rather than as the one that got lucky.
    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    unsafe { crate::memory::stdapi::ll_free(unmet as *mut u8) };
    drop(oom);

    assert_eq!(
        deferred_slot_count(),
        0,
        "a death in memory this collection never met is returned at once"
    );
    assert_eq!(gc_blocks(), held_before, "and a block was drawn to hold it");
    assert!(
        !crate::memory::large_entity::snapshot().contains(&unmet_block),
        "the run outlived the free that no row of this collection could have \
         made unsafe"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close drew a block");

    unsafe { crate::refcount::set_header_refcount(met, 0) };
    unsafe { crate::memory::stdapi::ll_free(met as *mut u8) };
    crate::memory::critical::drain_for_test();
}

/// A retained survivor dying in a block no trace has stamped is returned at
/// once — and the return empties its block, which goes home inside the window.
#[test]
fn an_unstamped_retained_survivor_is_returned_at_once() {
    let _guard = test_guard();
    let (_stamped_arena, [stamped_holder], [stamped_survivor], stamped_block) =
        unsafe { retained_survivors::<1>() };
    let (_arena, [holder], [_survivor], block) = unsafe { retained_survivors::<1>() };
    assert_ne!(
        stamped_block, block,
        "the two survivors are in different retained blocks"
    );

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), stamped_survivor, 1) };
    assert!(
        !unsafe { crate::memory::heap::block_shadow(stamped_block as *mut u8) }.is_null(),
        "the row stamped its own block"
    );
    assert!(
        unsafe { crate::memory::heap::block_shadow(block as *mut u8) }.is_null(),
        "and no row addresses this one"
    );

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        deferred_slot_count(),
        0,
        "neither block carries a row, so both deaths are returned at once"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and a block was drawn to hold the pair"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the survivor's return was withheld, so its block never emptied"
    );

    drop(window);
    assert_eq!(gc_blocks(), held_before, "the close drew a block");

    unsafe {
        assert!(crate::refcount::ll_release(stamped_holder as *mut RcHeader));
        ll_object_die(stamped_holder);
    }

    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*stamped_block).kind) },
        BLOCK_KIND_FREE
    );
}

/// The reset's whole-block sentinel — a return whose address is the block
/// header rather than an entity — is returned at once.
///
/// It has no entity header of its own, and it needs none: `retain_block`
/// clears the collector line before it publishes the kind, so no row of this
/// collection can address the block.
///
/// The sentinel is staged through the primitives the reset returns it with
/// rather than through a reset: a pin held across the survivor's death, and
/// the release that finds the block held by nothing
/// (`promote::arena_reset_full`, the `emptied` loop). A stamped block stands
/// beside it, so the case cannot pass for a window that stamped nothing.
#[test]
fn the_whole_block_sentinel_is_returned_at_once() {
    let _guard = test_guard();
    let (_arena, [holder], [_survivor], block) = unsafe { retained_survivors::<1>() };

    let stamped = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!stamped.is_null());
    let stamped = unsafe { live_entity(stamped, 1) };

    // The reset's own pin, which is what keeps the block standing while its
    // last occupant dies.
    unsafe { crate::memory::retained::pin(block as usize) };
    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_RETAINED,
        "the pin held the block through its survivor's death"
    );

    let held_before = gc_blocks();
    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    let row = unsafe { ensure_row(window.arena(), stamped, 1) };
    assert!(!row.is_null());
    assert!(
        unsafe { crate::memory::retained::hold_released(block as usize) },
        "the pin was the last thing holding the block"
    );

    crate::memory::critical::drain_for_test();
    let oom = force_oom();
    unsafe { crate::memory::stdapi::ll_free(block as *mut u8) };
    drop(oom);

    assert_eq!(
        deferred_slot_count(),
        0,
        "the sentinel addresses a block no row of this collection names"
    );
    assert_eq!(gc_blocks(), held_before, "and a block was drawn to hold it");
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the block was withheld although no row of this collection named it"
    );

    drop(window);
    unsafe { crate::refcount::set_header_refcount(stamped, 0) };
    unsafe { crate::memory::stdapi::ll_free(stamped as *mut u8) };
    crate::memory::critical::drain_for_test();
}

/// The sentinel arm and the unstamped arm answer alike, and this is what
/// separates them: a whole-block sentinel arriving on a block a collection has
/// stamped fails a test build.
///
/// The state is built by hand and production cannot reach it — the sentinel is
/// issued by `promote::arena_reset_full` between `retain_block`'s clearing of
/// the collector line and the block's return, with no trace step in between,
/// which is why the arm carries an assertion rather than an answer. What the
/// case pins is that the arm is read at all: without it a sentinel would fall
/// to the arm below, whose `block_shadow` test would send the stack's link into
/// the block's own header.
///
/// Under `debug_assertions` alone: a release build carries no assertion, takes
/// the arm below and returns at once.
#[cfg(debug_assertions)]
#[test]
fn a_sentinel_on_a_stamped_block_fails_a_test_build() {
    let _guard = test_guard();
    let (_arena, [holder], [survivor], block) = unsafe { retained_survivors::<1>() };
    let held_before = gc_blocks();

    // The reset's own hold, so the block outlives its last occupant and the
    // sentinel free below is the one that returns it.
    unsafe { crate::memory::retained::pin(block as usize) };
    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), survivor, 0) };
    assert!(
        !unsafe { crate::memory::heap::block_shadow(block as *mut u8) }.is_null(),
        "the row stamped the block the sentinel is about to name"
    );
    assert!(
        unsafe { crate::memory::retained::hold_released(block as usize) },
        "the pin was the last thing holding the block"
    );

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        crate::memory::stdapi::ll_free(block as *mut u8);
    }));
    let raised = refused.expect_err("the sentinel arm was expected to refuse");
    // Both payload shapes, because an assertion with no argument raises a
    // `&'static str` where one with arguments raises a `String`.
    let message = raised
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| raised.downcast_ref::<&'static str>().copied())
        .unwrap_or_default();
    assert!(
        message.contains("the reset's whole-block sentinel reached a stamped block"),
        "and it says which rule it refused on: {message}"
    );

    // The refusal read the block and changed nothing of it, so the close and
    // the return below are the ones an unstamped sentinel would have made.
    drop(window);
    unsafe { crate::memory::stdapi::ll_free(block as *mut u8) };
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_FREE,
        "the block went home once the stamp was gone"
    );
    assert_eq!(gc_blocks(), held_before, "and nothing was drawn to do it");
}
