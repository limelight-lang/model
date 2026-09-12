//! What an unwind out of the close gives back.
//!
//! The row sweep runs before anything that can raise, so a panic past it —
//! inside a return, inside the reset, or injected between the two — finds the
//! returns made or makes them on the drop's own pass; a window dropped with
//! its rows still standing abandons what it withheld instead, which is the
//! reuse the window exists to prevent.

use super::*;

/// A window dropped before its rows are gone abandons what it withheld: no
/// memory goes back, a slot handed back under a row that names it being the
/// reuse the window exists to prevent. The abandoned slot keeps the bit
/// `ll_free` took, which is true of it — this window hands nothing back
/// ([`crate::refcount::DEAD_IN_PLACE`]).
///
/// Read at the flag rather than behind a panic. The close sweeps before
/// anything that can raise, so no panic site in the crate reaches this arm
/// and `WindowControl::swept` is what says which disposition the
/// drop takes — the fact read rather than inferred from where an unwind came
/// from (`dev/DECISIONS.md`, "the row sweep runs ahead of the candidate
/// restore"). The case therefore opens the window's own structure over an
/// arena's region and drops it without telling it the rows are gone.
#[test]
fn a_window_dropped_before_its_rows_are_gone_abandons_what_it_withheld() {
    const CLASS: usize = ENTITY_SIZE * 9;

    let _guard = test_guard();
    let keeper = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    let victim = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!keeper.is_null() && !victim.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { live_entity(victim, 1) };
    let block = block_of(victim);

    let mut arena = TraceScratchArena::open().expect("this thread's workspace is in hand");
    // Safety: the region is the arena's own, and the window below dies before
    // the arena does, which is the order `ActiveTrace` gives by field order.
    let returns = unsafe { WithheldReturns::open(arena.withheld_returns_region()) };
    DEFERRED_RETURNS.with(|control| control.set(returns.control));

    unsafe { ensure_row(&mut arena, victim, 1) };
    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(deferred_slot_count(), 1, "the death was withheld");

    // No `rows_are_gone`, which is the whole of the case: the rows still stand
    // over the block when the window falls.
    drop(returns);

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot is neither live nor free, which a returned slot reads too; \
         what says this window returned nothing is the block's count below"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before,
        "and no return was made"
    );
    assert_eq!(
        deferred_slot_count(),
        0,
        "the window is closed, which is what a count of zero reads after a drop"
    );

    drop(arena);
    let served = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert_ne!(
        served, victim as *mut u8,
        "the abandoned slot reached no free list"
    );

    // The abandoned slot is out of circulation by design: nothing handed it
    // back, so `ll_free` refuses it and the case has to hand it back itself
    // before it can leave the class as it found it
    // (`crate::refcount::DEAD_IN_PLACE`).
    unsafe { crate::memory::stdapi::hand_back_and_free(victim as *mut u8) };
    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// An unwind out of the close past the row sweep returns what the window
/// withheld: the rows that would make a return a reuse are gone by then, so
/// the drop's own pass gives every stacked slot back.
///
/// Staged rather than provoked: the disposition of the batch is a merge into
/// whatever the lane holds and has no refusal of its own
/// (`queue::merge_candidates`), so the only unwind between the sweep and the
/// returns is an injected one ([`InjectedCloseUnwind`]).
#[test]
fn an_unwind_out_of_the_close_returns_what_was_withheld() {
    const CLASS: usize = ENTITY_SIZE * 8;

    let _guard = test_guard();

    let keeper = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    let victim = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert!(!keeper.is_null() && !victim.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { live_entity(victim, 1) };
    let block = block_of(victim);

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    // Detached and empty, which is all the case needs of it: the disposition
    // refuses on neither arm, so a registered root would decide nothing here
    // and would leave a withheld slot behind the case.
    window.detach_candidates();
    unsafe { ensure_row(window.arena(), victim, 1) };

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
    );

    let armed = InjectedCloseUnwind::arm();
    let raised = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    drop(armed);
    assert!(raised.is_err(), "the close was expected to unwind");

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the slot reads as one `ll_free` holds, on both sides of the return"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "and the return was made, the sweep having run before the unwind"
    );

    let served = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert_eq!(served, victim as *mut u8, "the slot is the class's again");

    unsafe { dead_entity(served) };
    unsafe { crate::memory::stdapi::ll_free(served) };
    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A panic raised inside the arena's hand-back of its own blocks finds every
/// withheld slot already returned.
///
/// This is the panic the close is built to survive, and the order is what
/// survives it: the sweep runs first, then the returns, and only then does the
/// arena give its blocks back. A close that gave them back first would reach
/// the window's drop with the rows still standing, and the withheld slots would be
/// abandoned rather than returned — which is the disposition
/// `WindowControl::swept` decides.
///
/// The panic is injected, the reset's own sites being an underflowed ledger
/// and a poisoned pool mutex: a test can raise neither without taking the
/// tests after it down with it (`crate::cycle::arena::InjectedResetFailure`).
///
/// A live entity keeps the victim's block off the pool, so the block's own
/// words can be read after the unwind.
#[test]
fn a_panic_in_the_reset_returns_every_withheld_slot() {
    let _guard = test_guard();
    let held_before = gc_blocks();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let victim = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!victim.is_null());
    let victim = unsafe { live_entity(victim, 1) };
    let block = block_of(victim);
    assert_eq!(
        block_of(keeper),
        block,
        "the keeper stands in the victim's own block, which is what keeps that \
         block off the pool once the victim goes back"
    );

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), victim, 1) };

    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    unsafe { crate::refcount::set_header_refcount(victim, 0) };
    unsafe { crate::memory::stdapi::ll_free(victim as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
    );

    let armed = crate::cycle::arena::InjectedResetFailure::arm();
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the reset was expected to raise");
    drop(armed);

    assert_eq!(
        unsafe { crate::refcount::slot_state(victim) },
        crate::refcount::SlotState::DeadInPlace,
        "the return was made before the blocks went back, which the block's count \
         below shows"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 1,
        "through the owner's `used`, as an ordered close does"
    );
    assert_eq!(
        gc_blocks(),
        held_before,
        "and the arena's own blocks went back at its drop, the reset the panic \
         interrupted being idempotent"
    );

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A panic inside one return leaves the slots below it standing on the stack,
/// and the drop's own pass gives them back.
///
/// The pop takes the head off the stack before it hands the slot over, so the
/// slot whose return raises is named by nothing and the slots behind it are
/// still named by the head. A pop that moved the head after the return would
/// leave the head naming a slot the free list has taken back.
///
/// The panic is staged off `ll_free`'s own refusal, on the close's first
/// return, which is the newest withheld slot: a slot whose refcount is raised
/// while the window is open is one that entry point refuses at its head, with
/// the slot handed back already and the return unmade.
///
/// **Which return was lost is read off the free list**, the headers saying
/// nothing about it: a slot whose return was made and one whose return was
/// interrupted both read `DeadInPlace`, `ll_free` having taken the first again
/// at its return and never handed the second back. The block's free list is
/// LIFO and holds exactly what this close gave back, so the allocation at the
/// end names that slot.
///
/// The newest slot is the panic's own leak and the case returns it by hand,
/// with no window open, so the class is left as it was found.
#[test]
fn a_panic_inside_one_return_gives_back_the_slots_below_it() {
    let _guard = test_guard();

    let keeper = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
    assert!(!keeper.is_null());
    let keeper = unsafe { live_entity(keeper, 1) };
    let mut dying = [std::ptr::null_mut(); 3];
    for slot in &mut dying {
        let entity = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) };
        assert!(!entity.is_null());
        *slot = unsafe { live_entity(entity, 1) };
    }

    let [first, second, third] = dying;
    let block = block_of(first);
    for entity in [second, third] {
        assert_eq!(
            block_of(entity),
            block,
            "the three deaths stand in one block, whose occupancy counts the returns made"
        );
    }

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), first, 1) };

    // `third` dies last, so it is the head of the stack and the close's first
    // return is its own. Three rather than two, so that a pass which gave one
    // slot back and stopped is not the same reading as one that gave back
    // every slot behind the raising return.
    let occupied_before = unsafe { crate::memory::heap::block_occupancy(block) };
    for entity in dying {
        unsafe { crate::refcount::set_header_refcount(entity, 0) };
        unsafe { crate::memory::stdapi::ll_free(entity as *mut u8) };
        assert_eq!(
            unsafe { crate::refcount::slot_state(entity) },
            crate::refcount::SlotState::DeadInPlace,
            "all three deaths were withheld"
        );
    }

    // The pop will reach `third` first and find it reading live, which is the
    // free `ll_free` refuses at its head — the slot handed back already, the
    // return unmade.
    unsafe { crate::refcount::set_header_refcount(third, 1) };
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the close was expected to raise");

    assert_eq!(
        unsafe { crate::refcount::slot_state(third) },
        crate::refcount::SlotState::Live,
        "the unwind handed back the slot whose return it raised inside, \
         and the raised count is what that return refused on"
    );
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupied_before - 2,
        "two returns were made, and the raising slot's is the one they could not make"
    );

    // The free list is LIFO and holds what this close gave back: `second` went
    // on it first and `first` after it, so the block hands them out in that
    // order reversed.
    let served = [
        unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) },
        unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 3) },
    ];
    assert_eq!(
        served,
        [first as *mut u8, second as *mut u8],
        "the drop's own pass gave back every slot below the raising return"
    );

    // The raising slot reached no free list, so this is its first return
    // rather than a second.
    unsafe { crate::refcount::set_header_refcount(third, 0) };
    unsafe { crate::memory::stdapi::ll_free(third as *mut u8) };
    for slot in served {
        unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe { crate::refcount::set_header_refcount(keeper, 0) };
    unsafe { crate::memory::stdapi::ll_free(keeper as *mut u8) };
}

/// A panic inside the return of one retained survivor gives back the survivor
/// below it, and gives its block back with it.
///
/// The two survivors stand in retained blocks of their own, which is what says
/// *which* return the unwind made: a retained survivor's return is an atomic
/// decrement of its block's occupant count, so the block whose only occupant
/// went back reaches the pool and the other one stands.
///
/// The survivor whose return raised is still an occupant of its block, and the
/// case returns it by hand afterwards — with no window open, so the block goes
/// home the way an ordered close would have sent it.
#[test]
fn a_panic_inside_a_retained_survivors_return_gives_back_the_one_below_it() {
    let _guard = test_guard();
    let (_first_arena, [first_holder], [first_survivor], first_block) =
        unsafe { retained_survivors::<1>() };
    let (_second_arena, [second_holder], [second_survivor], second_block) =
        unsafe { retained_survivors::<1>() };
    assert_ne!(
        first_block, second_block,
        "each survivor stands in a retained block of its own, which is what \
         tells the two returns apart"
    );
    let held_before = gc_blocks();

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    for survivor in [first_survivor, second_survivor] {
        unsafe { ensure_row(window.arena(), survivor, 1) };
    }

    // The holders take no row, so their own slots stand in a block this
    // collection never met and their deaths are returned at once. The second
    // survivor dies last, which puts it at the head of the stack and makes its
    // return the one the close raises inside.
    for holder in [first_holder, second_holder] {
        unsafe {
            assert!(crate::refcount::ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }
    }

    for survivor in [first_survivor, second_survivor] {
        assert_eq!(
            unsafe { crate::refcount::slot_state(survivor) },
            crate::refcount::SlotState::DeadInPlace,
            "both deaths were withheld"
        );
    }

    // As the slotted case stages it: a raised count is the free `ll_free`
    // refuses at its head, with the slot handed back already and the return unmade.
    unsafe { crate::refcount::set_header_refcount(second_survivor, 1) };
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the close was expected to raise");

    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*first_block).kind) },
        BLOCK_KIND_FREE,
        "the drop's own pass returned the survivor below the raising one, and \
         that emptied its block"
    );
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*second_block).kind) },
        BLOCK_KIND_RETAINED,
        "and the raising survivor is still its own block's occupant"
    );
    assert_eq!(
        unsafe { crate::memory::retained::held_occupant_count(second_block as usize) },
        1
    );

    // The raising survivor is the panic's own leak, and its return is what
    // empties the block it stands in.
    unsafe { crate::refcount::set_header_refcount(second_survivor, 0) };
    unsafe { crate::memory::stdapi::ll_free(second_survivor as *mut u8) };
    assert_eq!(
        unsafe { crate::memory::block_pool::load_block_kind(&raw const (*second_block).kind) },
        BLOCK_KIND_FREE,
        "the return the unwind lost is the one this makes"
    );
    assert_eq!(gc_blocks(), held_before, "and drew nothing to do any of it");
}

/// The unwind's half for a slot of another thread's block: the drop hands the
/// slot back and makes the return through the block's own stack of cross-thread
/// frees.
///
/// The panic is staged off `ll_free`'s own refusal rather than off the
/// injection: a withheld slot whose refcount is raised while the window is
/// open is one that entry point refuses in a test build. The sweep has run by
/// then, so the disposition is the returning one.
///
/// The block is `abandoned_block_of`'s, in a class of this case's own, so the
/// adoption at the end has nowhere to draw from but the frees the drop
/// posted — which is what says the return was made rather than dropped, and
/// which leaves the class as the case found it.
#[test]
fn a_panic_in_the_close_leaves_no_withheld_slot_standing() {
    const CLASS: usize = ENTITY_SIZE * 7;

    let _guard = test_guard();

    let held = abandoned_block_of(CLASS);

    let foreign = held[held.len() / 2] as *mut RcHeader;

    let raiser = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE * 2) };
    assert!(!raiser.is_null());
    let raiser = unsafe { dead_entity(raiser) };

    let mut window = ActiveTrace::open().expect("the pool funds the trace window");
    unsafe { ensure_row(window.arena(), foreign, 1) };
    unsafe { ensure_row(window.arena(), raiser, 0) };

    // The foreign slot dies first and the raiser second, so the raiser is the
    // head: the close raises on its first pop, and the foreign slot's return is
    // the drop's own pass — which is the half this case is about.
    unsafe { crate::refcount::set_header_refcount(foreign, 0) };
    unsafe { crate::memory::stdapi::ll_free(foreign as *mut u8) };
    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::DeadInPlace,
        "the death was withheld"
    );
    unsafe { crate::memory::stdapi::ll_free(raiser as *mut u8) };

    // The pop will reach this slot and find it reading live, which is the free
    // `ll_free` refuses.
    unsafe { crate::refcount::set_header_refcount(raiser, 1) };
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop(window);
    }));
    assert!(refused.is_err(), "the pop was expected to refuse");

    assert_eq!(
        unsafe { crate::refcount::slot_state(foreign) },
        crate::refcount::SlotState::DeadInPlace,
        "the unwind made the return, which the adoption below is what shows"
    );

    // The return itself: the block is full, so the adoption below has nothing
    // to serve from but the cross-thread frees the drop posted onto it
    // (`crate::memory::heap::Heap::alloc_block_full`). An unwind that popped
    // the slot and returned nothing leaves this drawing a fresh block.
    let served = unsafe { crate::memory::heap::entity_alloc(CLASS) };
    assert_eq!(
        served, foreign as *mut u8,
        "the unwind popped the slot without making the return it deferred"
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

    // The slot whose refusal raised is the one return the unwind lost: it
    // reached no free list, so this is its first return rather than a second.
    unsafe { crate::refcount::set_header_refcount(raiser, 0) };
    unsafe { crate::memory::stdapi::ll_free(raiser as *mut u8) };
}
