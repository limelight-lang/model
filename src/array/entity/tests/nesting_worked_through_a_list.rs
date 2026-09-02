//! Depth is the caller's input, so neither the copy nor the teardown
//! recurses: both drain a list held in a buffer-arena chunk, and each
//! runs against a 64 KiB stack, where a frame set per level ends on
//! the guard page with no unwinding and no record — the teardown at
//! 2 000 levels, the copy at 800, where it also pins that every level
//! left the arena. What separates them is what can be shown. The
//! teardown's list is forced to refuse, and a list that cannot grow
//! drops each child it could not take onto the recursive path, keeping
//! the outcome and losing only the bound; a copy whose list refuses
//! refuses the copy, so there is no recursive arm to force and its
//! bound stays arithmetic.

use super::*;

/// The deep copy's two halves, together because the pair is the
/// measurement: the depth runs on a thread the spawn sizes, and the
/// body that builds it is a function of its own, so a second copy of
/// either number would drift from this one silently.
const DEEP_COPY_DEPTH: usize = 800;
const DEEP_COPY_STACK: usize = 64 * 1024;

/// Nesting is worked through the list, so the copy of a deep arena
/// array touches one stack frame per *call*, not one per level. The
/// depth is well past `WorkList::FIRST`, so the chunk grows more than
/// once on the way through.
///
/// **On a stack that cannot hold the alternative**, which is what
/// makes the depth mean anything: 64 KiB against 800 levels leaves
/// 82 bytes a level, and the smallest frame set here is far above
/// that. The ordinary 8 MiB stack holds a recursive copy at this
/// depth, so the small stack is what makes the depth mean anything
/// rather than the depth itself.
///
/// Unlike the teardown below it, the bound is arithmetic rather than
/// exhibited: a teardown has no channel to refuse through, so
/// forcing its list to refuse returns it to the recursive path and
/// kills the thread, while a copy whose list refuses **refuses the
/// copy** — there is no recursive arm here to fall back to and none
/// to force.
#[test]
fn a_deep_arena_array_is_copied_out_through_the_work_list() {
    std::thread::Builder::new()
        .stack_size(DEEP_COPY_STACK)
        .spawn(deep_copy_body)
        .expect("the probe thread")
        .join()
        .expect("the deep copy ran out of machine stack");
}

/// The body of the test above, on its own small stack.
fn deep_copy_body() {
    const DEPTH: usize = DEEP_COPY_DEPTH;
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    // `[[[…]]]`, built from the inside out: each array holds the one
    // below it under key 0, and the source stays entirely in the
    // arena, where COW children are shared rather than copied.
    let mut levels = Vec::with_capacity(DEPTH);
    let innermost = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    levels.push(innermost);
    for _ in 1..DEPTH {
        let outer = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
        let inner = *levels.last().unwrap();
        unsafe {
            crate::refcount::ll_retain(inner as *mut RcHeader);
            crate::array::testing::insert(
                outer,
                Key::Int(0),
                Value::entity(crate::value::Tag::Array, inner as *mut RcHeader),
            );
        }

        levels.push(outer);
    }

    let src = *levels.last().unwrap();

    let copy = unsafe {
        separate(
            src,
            MemoryCategory::GcHeap,
            arena_ptr,
            CopyReason::Duplication,
        )
    };

    assert!(!copy.is_null(), "the deep copy was refused");

    // Every level is a heap array of its own, and none of them is the
    // arena array it was copied from.
    let mut level = copy;
    for depth in 0..DEPTH {
        assert_eq!(
            unsafe { crate::refcount::entity_category(level) },
            MemoryCategory::GcHeap,
            "level {depth} did not leave the arena"
        );
        assert_ne!(
            level,
            levels[DEPTH - 1 - depth],
            "level {depth} is the source array itself"
        );
        let entry = unsafe { crate::array::testing::get(level, Key::Int(0)) };
        if depth == DEPTH - 1 {
            assert!(entry.is_none(), "the innermost copy holds something");
            break;
        }

        level = entry
            .expect("the copy is shallower than its source")
            .entity_ptr() as *mut LLArray;
    }

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// Teardown of a deep array is a drain, not a recursion. The depth is
/// the caller's input — `$deep = [[[…]]]`, then one release — and a
/// frame set per level overflows the stack, which no arm of this
/// crate can catch: the guard page kills the process with no
/// unwinding and no record.
///
/// Measured on a thread whose stack is deliberately small, because a
/// depth that overflows the ordinary 8 MiB one costs minutes to build
/// and dominates the Miri run. 64 KiB against 2 000 levels leaves
/// under 33 bytes a level, and the smallest frame set here is far
/// above that; what the drain spends is a fixed frame and one list
/// entry, so the margin is total rather than per level. Verified by
/// forcing the list to refuse (`FORCE_REFUSE_LONGLIVED`), which
/// returns the teardown to the recursive path: the thread dies on the
/// guard page at this depth.
#[test]
#[cfg_attr(
    miri,
    ignore = "2000 levels of build and teardown is minutes under Miri, which would\
              dominate the whole-suite run; the drain's UB surface is covered by the\
              destructor-order tests and the deep-copy test, which Miri does run"
)]
fn a_deep_array_tears_down_without_the_machine_stack() {
    const DEPTH: usize = 2_000;
    const STACK: usize = 64 * 1024;

    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(|| {
            let _g = crate::memory::block_pool::test_guard();

            // `[[[…]]]`, built from the inside out and iteratively:
            // each level holds the one below it under key 0, so the
            // outermost is the chain's only holder. The creation
            // reference `ll_array_new` returns is what the entry
            // takes, so no level is retained twice and none is
            // released here.
            let mut level = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
            for _ in 1..DEPTH {
                let outer = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
                unsafe {
                    crate::array::testing::insert(
                        outer,
                        Key::Int(0),
                        Value::entity(crate::value::Tag::Array, level as *mut RcHeader),
                    );
                }

                level = outer;
            }

            // The chain is the thing under test, so its depth is
            // asserted rather than assumed: a refused insert would
            // leave a shallow array that tears down on any stack.
            let mut built = 1;
            let mut walk = level;
            while let Some(v) = unsafe { crate::array::testing::get(walk, Key::Int(0)) } {
                walk = v.entity_ptr() as *mut LLArray;
                built += 1;
            }

            assert_eq!(built, DEPTH, "the chain is shallower than it was built");

            unsafe {
                assert!(
                    ll_release(level as *mut RcHeader),
                    "the outermost array had a second holder"
                );
                crate::object::ll_entity_die(level as *mut RcHeader);
            }
        })
        .expect("the small-stack thread did not start")
        .join()
        .expect("teardown of the deep array killed its thread");
}

/// The list grows past its first chunk and hands pairs back in
/// reverse, which is all the copy asks of it. Growth copies what was
/// already there — losing it would drop whole subtrees of a deep copy
/// without any assertion firing.
#[test]
fn the_work_list_grows_and_keeps_what_it_held() {
    let _g = crate::memory::block_pool::test_guard();
    type Pairs = WorkList<(*mut LLArray, *mut LLArray)>;
    let n = Pairs::FIRST * 3;
    let mut list = Pairs::new();
    let pair = |i: usize| (i as *mut LLArray, (i + 1000) as *mut LLArray);
    for i in 0..n {
        assert!(list.push(pair(i)), "the list refused at {i}");
    }

    assert!(list.capacity >= n);
    for i in (0..n).rev() {
        assert_eq!(list.pop(), Some(pair(i)));
    }

    assert_eq!(list.pop(), None);
    list.dispose();
}

/// The refusal path, which is the exhaustion path: a list that cannot
/// grow drops each child it could not take onto the recursive one.
/// What must survive that is the outcome — everything is torn down,
/// in Zend's order — and what does not is the bound on depth, which
/// is why this shape is three levels rather than two thousand.
///
/// It was untestable until `FORCE_REFUSE_LONGLIVED` existed for the
/// pinned payload; the plan recorded it as owed on the strength
/// of `FORCE_OOM` alone, which the buffer arena can go around.
#[test]
fn a_refused_list_still_tears_everything_down_in_order() {
    assert_eq!(destructor_order_with(Shape::Mixed, true), "12345");
}
