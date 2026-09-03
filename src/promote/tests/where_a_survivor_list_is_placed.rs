//! Where a retained block's survivor list is written: into memory the
//! arena already holds, by three tiers — the block's own tail, the
//! reset's current block, a fresh pool block — and what each tier asks
//! of the pool (`rfc/model/gc/rc-cycle.md`, "The survivor list of a
//! retained block"). A block that holds another block's list outlives
//! it, and returns with its last hold.

use super::*;

/// Two survivors in two arena blocks, the first held by a heap object
/// of its own and the second as `SecondSurvivor` says, so either block
/// can be emptied alone. `leave_in_first` is how many bytes of the
/// first block's tail are left unused when the bump moves on to the
/// second.
///
/// One raw pointer per arena and per context, reused: a fresh `&mut`
/// per call would retag the pointer the objects were built through
/// (`dev/WORKFLOW.md`, Miri).
struct TwoBlocks {
    arena: Box<Arena>,
    first_holder: *mut Object,
    /// The heap object holding the second survivor; `None` when the
    /// drain kills that survivor and nothing is left to release.
    second_holder: Option<*mut Object>,
    first_block: usize,
    second_block: usize,
}

/// What holds the second block's survivor: a heap object the test
/// releases after the reset, or a heap box in an arena slot, whose
/// logged release kills the survivor inside the reset — the shape a
/// heap `&` box stored into an arena slot produces.
enum SecondSurvivor {
    HeldOnTheHeap,
    KilledByTheDrain,
}

unsafe fn two_blocks(
    name: &str,
    leave_in_first: usize,
    second_survivor: SecondSurvivor,
) -> TwoBlocks {
    let survivor_cls = ClassBuilder::new(&format!("{name}Survivor")).build();
    let holder_cls = ClassBuilder::new(&format!("{name}Holder"))
        .prop("member", true)
        .build();

    let mut arena = Box::new(Arena::new());
    let arena_ptr: *mut Arena = &mut *arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let first_holder =
        unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let first = unsafe {
        new_constructed(
            &mut *context_ptr,
            survivor_cls,
            MemoryCategory::RequestArena,
        )
    };
    let first_block = BlockHeader::of_ptr(first as *const u8) as usize;

    // The bump leaves the first block with `leave_in_first` bytes unused
    // and takes a second one for the next survivor.
    let room = unsafe { (*arena_ptr).room_left() };
    assert!(
        room > leave_in_first + 8,
        "the first block has no room to leave"
    );
    assert!(!unsafe { (*arena_ptr).alloc(room - leave_in_first) }.is_null());
    assert!(
        !unsafe { (*arena_ptr).alloc(leave_in_first + 8) }.is_null(),
        "the bump refused a fresh block"
    );
    let second = unsafe {
        new_constructed(
            &mut *context_ptr,
            survivor_cls,
            MemoryCategory::RequestArena,
        )
    };
    let second_block = BlockHeader::of_ptr(second as *const u8) as usize;
    assert_ne!(first_block, second_block, "one block took both survivors");

    unsafe { store_prop(arena_ptr, first_holder, 16, first) };
    let second_holder = match second_survivor {
        SecondSurvivor::HeldOnTheHeap => {
            let holder =
                unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
            unsafe { store_prop(arena_ptr, holder, 16, second) };
            Some(holder)
        }
        SecondSurvivor::KilledByTheDrain => {
            // The escape is into a heap box, and the box goes into an
            // arena slot: that is what logs the box's release against
            // the reset, and the release is what kills the promoted
            // survivor while the reset is still running.
            let corpse = unsafe {
                new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::RequestArena)
            };
            unsafe {
                let boxed = crate::reference::ll_reference_new();
                assert!(ref_store(
                    arena_ptr,
                    boxed as *mut RcHeader,
                    &raw mut (*boxed).value,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Object, second as *mut RcHeader),
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

            None
        }
    };

    TwoBlocks {
        arena,
        first_holder,
        second_holder,
        first_block,
        second_block,
    }
}

/// Release the one reference `holder` keeps on its survivor, which is
/// the survivor's death and the return of its block if nothing else
/// holds it.
unsafe fn let_go(holder: *mut Object) {
    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

fn kind_of(block: usize) -> u32 {
    unsafe { block_kind(block as *const u8) }
}

/// A list that fits below the block's last object is written there, and
/// the reset asks the pool for nothing.
#[test]
fn a_list_that_fits_goes_into_the_blocks_own_tail() {
    use crate::memory::block_pool::BLOCK_KIND_FREE;
    let _g = crate::memory::block_pool::test_guard();
    let mut shape = unsafe { two_blocks("OwnTail", 64, SecondSurvivor::HeldOnTheHeap) };
    let arena_ptr: *mut Arena = &mut *shape.arena;

    crate::test_support::allocation_probe::take_allocations();
    unsafe { arena_reset_full(arena_ptr) };
    let (_, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(pool, 0, "a list that fits its own tail drew a block");

    for (block, name) in [(shape.first_block, "first"), (shape.second_block, "second")] {
        assert_eq!(
            unsafe { crate::memory::retained::survivor_list_holder(block) },
            block,
            "the {name} block's list left the block it describes"
        );
        assert_eq!(
            unsafe { crate::memory::retained::pin_count(block) },
            0,
            "the {name} block is held for something beyond its survivor"
        );
    }

    unsafe { let_go(shape.first_holder) };
    assert_eq!(kind_of(shape.first_block), BLOCK_KIND_FREE);
    unsafe { let_go(shape.second_holder.expect("held on the heap")) };
    assert_eq!(kind_of(shape.second_block), BLOCK_KIND_FREE);
}

/// A block with no room in its tail puts its list into the reset's
/// current block, which is then held for it: the holder outlives the
/// block whose list it carries, and the pool is still asked for nothing.
#[test]
fn a_list_with_no_room_in_its_tail_goes_into_the_current_block() {
    use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_KIND_RETAINED};
    let _g = crate::memory::block_pool::test_guard();
    let mut shape = unsafe { two_blocks("CurrentBlock", 0, SecondSurvivor::HeldOnTheHeap) };
    let arena_ptr: *mut Arena = &mut *shape.arena;

    crate::test_support::allocation_probe::take_allocations();
    unsafe { arena_reset_full(arena_ptr) };
    let (_, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(pool, 0, "a list placed in the current block drew a block");

    assert_eq!(
        unsafe { crate::memory::retained::survivor_list_holder(shape.first_block) },
        shape.second_block,
        "the full block's list was not placed in the current block"
    );
    assert_eq!(
        unsafe { crate::memory::retained::survivor_list_holder(shape.second_block) },
        shape.second_block
    );
    assert_eq!(
        unsafe { crate::memory::retained::pin_count(shape.second_block) },
        1,
        "the holder is not held for the list standing in it"
    );

    unsafe { let_go(shape.first_holder) };
    assert_eq!(kind_of(shape.first_block), BLOCK_KIND_FREE);
    assert_eq!(
        kind_of(shape.second_block),
        BLOCK_KIND_RETAINED,
        "the holder went home under its own survivor"
    );
    assert_eq!(
        unsafe { crate::memory::retained::pin_count(shape.second_block) },
        0,
        "the returned block's list still holds its holder"
    );

    unsafe { let_go(shape.second_holder.expect("held on the heap")) };
    assert_eq!(kind_of(shape.second_block), BLOCK_KIND_FREE);
}

/// With no room in any block the reset draws one fresh block for every
/// list that missed, retains it as their holder, and returns it with the
/// last of them. The block is the arena's, so the ledger of the blocks
/// collection owns does not move.
#[test]
fn lists_with_no_room_anywhere_share_one_fresh_block_the_reset_retains() {
    use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_KIND_RETAINED};
    let _g = crate::memory::block_pool::test_guard();
    let mut shape = unsafe { two_blocks("FreshBlock", 0, SecondSurvivor::HeldOnTheHeap) };
    let arena_ptr: *mut Arena = &mut *shape.arena;
    let room = unsafe { (*arena_ptr).room_left() };
    assert!(!unsafe { (*arena_ptr).alloc(room) }.is_null());
    assert_eq!(
        unsafe { (*arena_ptr).room_left() },
        0,
        "the second block has room"
    );

    crate::memory::gc_metadata::lower_thread_peak_to_current();
    let before = crate::memory::gc_metadata::thread_stats();
    crate::test_support::allocation_probe::take_allocations();
    unsafe { arena_reset_full(arena_ptr) };
    let (_, pool) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        pool, 1,
        "two lists with no room anywhere drew {pool} blocks"
    );
    assert_eq!(
        crate::memory::gc_metadata::thread_stats(),
        before,
        "the fresh block was counted as collection's"
    );

    let fresh = unsafe { crate::memory::retained::survivor_list_holder(shape.first_block) };
    assert_ne!(fresh, shape.first_block);
    assert_ne!(fresh, shape.second_block);
    assert_eq!(
        unsafe { crate::memory::retained::survivor_list_holder(shape.second_block) },
        fresh,
        "the second list did not share the fresh block"
    );
    assert_eq!(kind_of(fresh), BLOCK_KIND_RETAINED);
    assert_eq!(
        unsafe { crate::memory::retained::pin_count(fresh) },
        2,
        "the fresh block is not held once per list"
    );
    assert!(
        unsafe { crate::memory::retained::occupant_count(fresh) }.is_none(),
        "a block that holds lists alone lists nothing itself"
    );

    unsafe { let_go(shape.first_holder) };
    assert_eq!(kind_of(shape.first_block), BLOCK_KIND_FREE);
    assert_eq!(kind_of(fresh), BLOCK_KIND_RETAINED);
    assert_eq!(unsafe { crate::memory::retained::pin_count(fresh) }, 1);

    unsafe { let_go(shape.second_holder.expect("held on the heap")) };
    assert_eq!(kind_of(shape.second_block), BLOCK_KIND_FREE);
    assert_eq!(
        kind_of(fresh),
        BLOCK_KIND_FREE,
        "the fresh block outlived the last list standing in it"
    );
}

/// Every list is placed before any block's count is read. The shape the
/// rule exists for: the current block's own survivor dies inside the
/// reset, and a full block's list lands in the current block's tail.
/// Read as soon as its own list was placed, the current block would
/// answer "empty" and the reset would return it under the list placed
/// there next; read after every placement, it is held by that list and
/// returns with the block the list describes.
///
/// The reset groups survivors by block in a map with no order, so one
/// run reaches the order that breaks a single pass half the time; the
/// shape is repeated so that a run reaching it is the rule.
#[test]
fn a_holder_emptied_inside_the_reset_is_read_after_the_list_placed_in_it() {
    use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_KIND_RETAINED};
    let _g = crate::memory::block_pool::test_guard();
    let rounds = if cfg!(miri) { 2 } else { 16 };
    for round in 0..rounds {
        let mut shape = unsafe {
            two_blocks(
                &format!("EmptiedHolder{round}"),
                0,
                SecondSurvivor::KilledByTheDrain,
            )
        };
        let arena_ptr: *mut Arena = &mut *shape.arena;
        unsafe { arena_reset_full(arena_ptr) };

        assert_eq!(
            unsafe { crate::memory::retained::survivor_list_holder(shape.first_block) },
            shape.second_block,
            "the full block's list was not placed in the current block"
        );
        assert!(
            !unsafe { crate::memory::retained::has_live_occupants(shape.second_block) },
            "the current block's survivor outlived the reset, so this test proves nothing"
        );
        assert_eq!(
            kind_of(shape.second_block),
            BLOCK_KIND_RETAINED,
            "the holder went home under the list standing in it"
        );
        assert_eq!(
            unsafe { crate::memory::retained::pin_count(shape.second_block) },
            1
        );

        unsafe { let_go(shape.first_holder) };
        assert_eq!(kind_of(shape.first_block), BLOCK_KIND_FREE);
        assert_eq!(
            kind_of(shape.second_block),
            BLOCK_KIND_FREE,
            "the holder outlived the last list standing in it"
        );
    }
}
