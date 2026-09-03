//! A root the queue holds after the entity was torn down. The queue's entry
//! keeps the slot out of the allocator's hands, so the address is still this
//! entity's; what the entry does not keep is the entity's contents. Teardown
//! released every counted child and left the cells naming them
//! (`object::ll_default_dispose`, phase 2), so those cells are addresses of
//! slots the allocator may have handed to somebody else.
//!
//! The trace therefore reads the count before it reads the cells. It may:
//! the mutator does not free an entity the root queue names
//! (`memory::stdapi::ll_free`, the candidate arm), so a count this call reads
//! above zero cannot fall to a torn-down entity under it.

use super::*;

/// The whole of the rule, on the smallest graph that shows it: a torn-down
/// entity whose cell still names a live one. Expanding it subtracts an edge
/// that no longer exists, and the entity it points at reads as held by one
/// reference fewer than it has — which is a component proposed for teardown
/// on a count the heap never held.
#[test]
fn a_root_at_count_zero_expands_nothing() {
    let _g = test_guard();
    let holder = ClassBuilder::new("DeadRootHolder")
        .prop("child", true)
        .build();
    let plain = ClassBuilder::new("DeadRootChild").build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let torn_down = unsafe { new_constructed(&mut context, holder, MemoryCategory::GcHeap) };
    let child = unsafe { new_constructed(&mut context, plain, MemoryCategory::GcHeap) };

    unsafe { store_prop(&mut arena, torn_down, prop_offset(0), child) };
    let child_count_before = unsafe { crate::refcount::header_refcount(child as *mut RcHeader) };
    assert_eq!(
        child_count_before, 2,
        "the fixture's reference and the cell's"
    );

    // Taken to zero by hand rather than through `ll_release`: the death path
    // would run the teardown, and what this case needs is the state the queue
    // holds — an entity at zero whose cells still name a live child, with its
    // slot still its own.
    unsafe { crate::refcount::set_header_refcount(torn_down as *mut RcHeader, 0) };

    let mut shadow_arena = crate::cycle::testing::open_arena();
    assert_eq!(
        unsafe { mark(&mut shadow_arena, torn_down as *mut RcHeader) },
        MarkResult::Complete,
        "a root at zero is not a refusal: there is nothing to trace and nothing failed"
    );

    assert_eq!(
        shadow_arena.touched_blocks(),
        0,
        "the trace reserved rows, so it read the cells of an entity whose \
         teardown had already released them"
    );
    assert_eq!(
        unsafe { crate::refcount::header_refcount(child as *mut RcHeader) },
        child_count_before,
        "and the heap moved either way"
    );

    shadow_arena.reset();
    unsafe { crate::refcount::set_header_refcount(torn_down as *mut RcHeader, 1) };
    unsafe { ll_release(torn_down as *mut RcHeader) };
}
