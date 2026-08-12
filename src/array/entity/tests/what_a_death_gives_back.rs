//! The kind switch is the only door a bare entity pointer has, and
//! its Array arm walks elements and string keys once each, returns
//! the storage, and tears down a child the array held last rather
//! than only decrementing it: `ll_release`'s report is an
//! obligation, and dropping it leaves everything that child holds
//! unreachable.

use super::*;

#[test]
fn releasing_children_walks_elements_and_string_keys_once_each() {
    let _g = crate::memory::block_pool::test_guard();
    let a = hash_arr();
    let key = mk(b"k");
    let child = mk(b"v");
    unsafe {
        crate::refcount::ll_retain(key as *mut RcHeader);
        crate::refcount::ll_retain(child as *mut RcHeader);
        crate::array::testing::insert(
            a,
            Key::Str(key),
            Value::entity(crate::value::Tag::String, child as *mut RcHeader),
        );
        let k0 = (*(key as *mut RcHeader)).refcount;
        let c0 = (*(child as *mut RcHeader)).refcount;

        release_children(a);
        assert_eq!((*(key as *mut RcHeader)).refcount, k0 - 1);
        assert_eq!((*(child as *mut RcHeader)).refcount, c0 - 1);

        crate::array::entity::dispose_storage(a, category_of(a));
    }
}

/// Death through the kind switch, which is the only door a bare
/// entity pointer has. Before the Array arm existed this reached a
/// `debug_assert!(false)` and, in release, did nothing at all: the
/// children kept the references the array owed them and the storage
/// was never returned.
#[test]
fn dying_through_the_kind_switch_releases_the_children_and_the_storage() {
    use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK};
    use crate::memory::buffer::{PressureMode, set_pressure_mode};
    use crate::memory::buffer_arena::with_buffer_arena;
    use crate::refcount::{ll_release, ll_retain};
    let _g = crate::memory::block_pool::test_guard();

    let a = hash_arr();
    let key = mk(b"key");
    let value = mk(b"value");
    unsafe {
        // One reference each for the array, one for this test, so the
        // children outlive the array and can be read afterwards.
        ll_retain(key as *mut RcHeader);
        ll_retain(value as *mut RcHeader);
        crate::array::testing::insert(
            a,
            Key::Str(key),
            Value::entity(crate::value::Tag::String, value as *mut RcHeader),
        );
        let (storage, capacity) = crate::array::testing::storage_and_capacity(a);
        assert!(
            !storage.is_null(),
            "the insert allocated storage to release"
        );

        assert!(
            ll_release(a as *mut RcHeader),
            "the array was the last holder"
        );
        crate::object::ll_entity_die(a as *mut RcHeader);

        assert_eq!(
            (*(key as *mut RcHeader)).refcount,
            1,
            "the key's reference was not let go"
        );
        assert_eq!(
            (*(value as *mut RcHeader)).refcount,
            1,
            "the element's reference was not let go"
        );
        // The storage came back: in critical mode an allocation
        // searches the block's free list, so the same address
        // returning means teardown really disposed of the table
        // rather than only dropping the entity.
        let kind = *(((storage as usize) & !BLOCK_MASK) as *const u32);
        assert_eq!(
            kind, BLOCK_KIND_BUFFER,
            "the storage was not a buffer chunk"
        );
        set_pressure_mode(PressureMode::Critical);
        let (reused, _) = with_buffer_arena(|arena| arena.alloc(capacity));
        set_pressure_mode(PressureMode::Plenty);
        assert_eq!(reused, storage, "teardown left the storage unreturned");
        with_buffer_arena(|arena| arena.free(reused, capacity));

        // Released first: a slot freed while its header still reads
        // refcount 1 is enumerated as a live entity by every later
        // process-global walk (`dev/POSTMORTEM.md`, "an entity killed
        // at refcount 1").
        assert!(ll_release(key as *mut RcHeader));
        crate::object::ll_entity_die(key as *mut RcHeader);
        assert!(ll_release(value as *mut RcHeader));
        crate::object::ll_entity_die(value as *mut RcHeader);
    }
}

/// A child the array was the last holder of has to be torn down, not
/// merely decremented. `ll_release` reports the death and the report
/// is an obligation: dropping it leaves the child's own memory — and
/// everything *it* holds — unreachable and unfreed. Observed through a
/// nested array, whose storage is a buffer chunk that can be seen
/// coming back.
#[test]
fn a_child_the_array_held_last_is_torn_down_and_not_only_released() {
    use crate::memory::buffer::{PressureMode, set_pressure_mode};
    use crate::memory::buffer_arena::with_buffer_arena;
    use crate::refcount::ll_release;
    let _g = crate::memory::block_pool::test_guard();

    let outer = hash_arr();
    let inner = hash_arr();
    unsafe {
        crate::array::testing::insert(inner, Key::Int(1), Value::int(1));
        let (storage, capacity) = crate::array::testing::storage_and_capacity(inner);
        assert!(!storage.is_null(), "the inner array has storage to reclaim");

        // The inner array's only reference is the outer array's
        // element, so the outer's death is the inner's death.
        crate::array::testing::insert(
            outer,
            Key::Int(0),
            Value::entity(crate::value::Tag::Array, inner as *mut RcHeader),
        );

        assert!(ll_release(outer as *mut RcHeader));
        crate::object::ll_entity_die(outer as *mut RcHeader);

        // Both tables are freed by this teardown, the outer one
        // first: the drain disposes a level before it takes the next
        // one off the list. Which of the two the free list hands back
        // first is the allocator's business, so the assertion takes
        // either — what it is here to catch is the inner storage
        // never coming back at all.
        set_pressure_mode(PressureMode::Critical);
        let first = with_buffer_arena(|arena| arena.alloc(capacity));
        let second = with_buffer_arena(|arena| arena.alloc(capacity));
        set_pressure_mode(PressureMode::Plenty);
        assert!(
            first.0 == storage || second.0 == storage,
            "the inner array was released but never torn down: its storage never came back"
        );
        with_buffer_arena(|arena| {
            arena.free(first.0, first.1);
            arena.free(second.0, second.1);
        });
    }
}
