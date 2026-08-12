//! One copy per distinct source entity, held once for every entry that
//! names it.
//!
//! What this rules out is a copy entered per *entry*: a source whose
//! children name each other twice per level then costs 2^depth copies,
//! and nesting depth on a store path is the attacker's to choose. The
//! count of distinct arrays in the finished copy is what says which of
//! the two happened — one per level, or one per path to a level.

use super::*;

/// A chain of `depth + 1` arena arrays where each level names the one
/// below it **twice**. Copied per entry that is 2^depth arrays; copied
/// per entity it is `depth + 1`.
///
/// Each level keeps the builder's own reference on top of the two the
/// entries take: an arena entity's cell is the reset's, so nothing here
/// gives them back one at a time.
unsafe fn doubling_chain(depth: usize) -> *mut LLArray {
    let mut level = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    for _ in 0..depth {
        let next = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
        for k in 0..2 {
            unsafe {
                crate::refcount::ll_retain(level as *mut RcHeader);
                crate::array::testing::insert(
                    next,
                    Key::Int(k),
                    Value::entity(crate::value::Tag::Array, level as *mut RcHeader),
                );
            }
        }

        level = next;
    }

    level
}

/// How many distinct arrays the graph under `root` holds, `root`
/// included.
unsafe fn distinct_arrays(root: *mut LLArray) -> usize {
    let mut seen: Vec<*mut LLArray> = Vec::new();
    let mut stack = vec![root];
    while let Some(a) = stack.pop() {
        if seen.contains(&a) {
            continue;
        }

        seen.push(a);
        // Through the tracing walk rather than the table, so the helper
        // reads whichever representation the array holds.
        unsafe {
            for_each_counted_child(a, |child| {
                let flags = crate::refcount::header_flags(child);
                let kind = (flags & crate::refcount::ENTITY_KIND_MASK)
                    >> crate::refcount::ENTITY_KIND_SHIFT;
                if kind == crate::refcount::EntityKind::Array as u32 {
                    stack.push(child as *mut LLArray);
                }
            })
        };
    }

    seen.len()
}

/// The diamond: two entries naming one child yield one copy of it, and
/// that copy is held twice — once for the entry that created it and once
/// for the entry that met it again.
#[test]
fn a_child_two_entries_name_is_copied_once_and_held_twice() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let src = unsafe { doubling_chain(1) };
    let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
    assert!(!copy.is_null(), "the copy was refused");

    let (table, head) = unsafe { as_table(copy) };
    let first = table.get(head, Key::Int(0)).expect("entry 0").entity_ptr();
    let second = table.get(head, Key::Int(1)).expect("entry 1").entity_ptr();
    assert_eq!(first, second, "the shared child was copied twice");
    assert_eq!(
        unsafe { crate::refcount::header_refcount(first) },
        2,
        "the one copy is not held once per entry naming it"
    );
    assert_eq!(unsafe { distinct_arrays(copy) }, 2);

    unsafe {
        assert!(crate::refcount::ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// Twelve levels, each naming the level below it twice: the copy holds
/// thirteen arrays. Per entry it would hold 4095, which is the shape this
/// exists to refuse.
///
/// **Twelve rather than the thirty the stage's criterion names.** The
/// depth decides what a regression can report: copied per entry a
/// twelve-level chain holds 8191 arrays, which a machine finishes and an
/// assertion names, while thirty is 2^31 and exhausts the pool with the
/// tests running beside it.
///
/// Which assertion reports depends on the build. In debug the probe at
/// `separate`'s loop head fires first — the list is popped
/// last-in-first-out, so the walk descends one spine and the first
/// sibling re-enters a source already entered — and the count below is
/// what a release build has instead. The 8191 was seen, with the
/// association disabled and before that probe was restored.
#[test]
fn a_doubling_chain_costs_one_copy_a_level() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    const DEPTH: usize = 12;
    let src = unsafe { doubling_chain(DEPTH) };
    let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr, CopyReason::Escape) };
    assert!(!copy.is_null(), "the copy was refused");
    assert_eq!(
        unsafe { distinct_arrays(copy) },
        DEPTH + 1,
        "the copy entered a level once per path to it rather than once"
    );

    unsafe {
        assert!(crate::refcount::ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
