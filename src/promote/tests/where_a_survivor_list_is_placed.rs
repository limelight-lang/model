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

/// Which survivor escapes first, which decides the order the reset meets
/// the two blocks in: an escape is logged where it happens, the escapee log
/// becomes the survivor chain in that order, and the grouping walks that
/// chain. A test whose subject is the order says which one it needs.
enum EscapesFirst {
    TheFirstBlocksSurvivor,
    TheSecondBlocksSurvivor,
}

unsafe fn two_blocks(
    name: &str,
    leave_in_first: usize,
    second_survivor: SecondSurvivor,
    escapes_first: EscapesFirst,
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

    let escape_the_first = || unsafe { store_prop(arena_ptr, first_holder, 16, first) };
    if matches!(escapes_first, EscapesFirst::TheFirstBlocksSurvivor) {
        escape_the_first();
    }

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

    if matches!(escapes_first, EscapesFirst::TheSecondBlocksSurvivor) {
        escape_the_first();
    }

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
    let mut shape = unsafe {
        two_blocks(
            "OwnTail",
            64,
            SecondSurvivor::HeldOnTheHeap,
            EscapesFirst::TheFirstBlocksSurvivor,
        )
    };
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
    let mut shape = unsafe {
        two_blocks(
            "CurrentBlock",
            0,
            SecondSurvivor::HeldOnTheHeap,
            EscapesFirst::TheFirstBlocksSurvivor,
        )
    };
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
    let mut shape = unsafe {
        two_blocks(
            "FreshBlock",
            0,
            SecondSurvivor::HeldOnTheHeap,
            EscapesFirst::TheFirstBlocksSurvivor,
        )
    };
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
/// **The order is the fixture's, and it is the point.** The grouping walks
/// the survivor chain, so the block it reaches first is the block whose
/// survivor escaped first, and only one of the two orders can catch a
/// publication that ran too early: the current block has to be placed
/// before the full block's list lands in it. So this shape escapes the
/// current block's survivor first and asserts that the placement pass
/// reached that block first, which is what makes a collapsed pass fail
/// here rather than pass by luck.
#[test]
fn a_holder_emptied_inside_the_reset_is_read_after_the_list_placed_in_it() {
    use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_KIND_RETAINED};
    let _g = crate::memory::block_pool::test_guard();
    let mut shape = unsafe {
        two_blocks(
            "EmptiedHolder",
            0,
            SecondSurvivor::KilledByTheDrain,
            EscapesFirst::TheSecondBlocksSurvivor,
        )
    };
    let arena_ptr: *mut Arena = &mut *shape.arena;
    let _ = crate::promote::take_first_placed_block();
    unsafe { arena_reset_full(arena_ptr) };
    assert_eq!(
        crate::promote::take_first_placed_block(),
        shape.second_block,
        "the placement pass reached the full block first, so a publication \
         that ran too early would have nothing to catch it"
    );

    assert_eq!(
        unsafe { crate::memory::retained::survivor_list_holder(shape.first_block) },
        shape.second_block,
        "the full block's list was not placed in the current block"
    );
    assert!(
        !unsafe { crate::memory::retained::has_held_occupants(shape.second_block) },
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

/// A placement the arena refuses publishes the count without a list. The
/// block stays retained and is found only by its occupants' deaths, which
/// is what `retained::register` answers for a null list — and the survivor
/// it holds is promoted like any other.
///
/// **The refusal is injected**, because the only other way to it is a pool
/// with nothing left in it: three branches stand on this arm — the
/// sentinel the placing pass writes where an address would go, its reading
/// by the publishing pass, and `register`'s own null arm — and under a
/// real exhaustion all three would run for the first time together.
#[test]
fn a_refused_placement_publishes_the_count_without_a_list() {
    use crate::memory::arena::RefusedListPlacement;
    let _g = crate::memory::block_pool::test_guard();
    let holder_cls = ClassBuilder::new("RefusedListCache")
        .prop("last", true)
        .build();
    let cls = ClassBuilder::new("RefusedListSurvivor")
        .prop("x", true)
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let obj = unsafe { new_constructed(&mut *context_ptr, cls, MemoryCategory::RequestArena) };
    unsafe { store_prop(arena_ptr, holder, 16, obj) };
    let block = BlockHeader::of_ptr(obj as *const u8) as usize;

    {
        let _refused = RefusedListPlacement::arm();
        unsafe { arena_reset_full(&mut *arena_ptr) };
    }

    unsafe {
        assert_eq!(
            crate::refcount::entity_category(obj),
            MemoryCategory::GcHeap,
            "the survivor was not promoted"
        );
        assert_eq!(
            crate::memory::heap::block_survivor_list(block as *mut u8),
            (std::ptr::null(), 0),
            "a list was published although the arena refused the memory for it"
        );
        assert_eq!(
            crate::memory::retained::held_occupant_count(block),
            1,
            "the hold on the block went with the list it could not place"
        );
        assert_eq!(
            crate::memory::retained::occupant_count(block),
            None,
            "a listless block answers no index, which is what a trace reads"
        );

        // The block still returns by its occupant's death, which is the
        // whole of what a listless retained block can answer.
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
        assert_eq!(
            block_kind(block as *const u8),
            crate::memory::block_pool::BLOCK_KIND_FREE,
            "the block outlived the survivor it was retained for"
        );
    }
}

/// Several survivors in each of several blocks, all held by one heap
/// object: every block gets a list of its own, holding its own survivors
/// and no others, and the grouping that decides which block a survivor
/// belongs to asks the global allocator for nothing.
///
/// The allocation count is the point: the reset path draws nothing from the
/// process allocator for its grouping, which a table keyed by block address
/// and a vector per block cannot say (`dev/DECISIONS.md`, "the reset
/// window's memory comes from the manager").
#[test]
fn the_grouping_draws_nothing_from_the_global_allocator() {
    let _g = crate::memory::block_pool::test_guard();
    const BLOCKS: usize = 3;
    const PER_BLOCK: usize = 3;

    let mut holder_class = ClassBuilder::new("GroupingCache");
    for i in 0..BLOCKS * PER_BLOCK {
        holder_class = holder_class.prop(&format!("member{i}"), true);
    }

    let holder_cls = holder_class.build();
    let survivor_cls = ClassBuilder::new("GroupingSurvivor").build();

    let mut arena = Box::new(Arena::new());
    let arena_ptr: *mut Arena = &mut *arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let mut blocks: Vec<usize> = Vec::new();
    let mut survivors: Vec<Vec<usize>> = Vec::new();
    for block_index in 0..BLOCKS {
        let mut in_this_block = Vec::new();
        for member in 0..PER_BLOCK {
            let survivor = unsafe {
                new_constructed(
                    &mut *context_ptr,
                    survivor_cls,
                    MemoryCategory::RequestArena,
                )
            };
            let slot = (16 + 16 * (block_index * PER_BLOCK + member)) as u32;
            unsafe { store_prop(arena_ptr, holder, slot, survivor) };
            in_this_block.push(survivor as usize);
        }

        let block = BlockHeader::of_ptr(in_this_block[0] as *const u8) as usize;
        assert!(
            in_this_block
                .iter()
                .all(|s| BlockHeader::of_ptr(*s as *const u8) as usize == block),
            "the block took only some of its survivors"
        );
        assert!(!blocks.contains(&block), "a block was filled twice");
        blocks.push(block);
        survivors.push(in_this_block);

        // Fill the rest of the block, so the next survivor takes a fresh
        // one and the grouping has more than one block to tell apart.
        if block_index + 1 < BLOCKS {
            let room = unsafe { (*arena_ptr).room_left() };
            assert!(!unsafe { (*arena_ptr).alloc(room) }.is_null());
        }
    }

    crate::test_support::allocation_probe::take_allocations();
    unsafe { arena_reset_full(arena_ptr) };
    let (heap, _) = crate::test_support::allocation_probe::take_allocations();
    assert_eq!(
        heap, 0,
        "the reset drew {heap} allocations from the process"
    );

    for (block, mut expected) in blocks.iter().zip(survivors) {
        let (list, count) = unsafe { crate::memory::heap::block_survivor_list(*block as *mut u8) };
        assert_eq!(count, PER_BLOCK, "the block's list lost a survivor");
        expected.sort_unstable();
        assert_eq!(
            unsafe { std::slice::from_raw_parts(list, count) },
            &expected[..],
            "the block's list is not its own survivors, sorted"
        );
    }

    unsafe { let_go(holder) };
    for block in blocks {
        assert_eq!(
            kind_of(block),
            crate::memory::block_pool::BLOCK_KIND_FREE,
            "a block outlived every survivor it held"
        );
    }
}
