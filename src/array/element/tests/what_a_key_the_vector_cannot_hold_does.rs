//! A fresh array is the mixed vector, so the key decides the
//! representation: one the dense range cannot hold migrates the array
//! to the ordered hash before anything is stored, and one it can hold
//! leaves the vector standing. The migration is asked for by the
//! element layer through the tag (`element::representation_for`), so
//! neither entry point here (`set` with either key) names a representation.

use super::*;

/// The holder of an array at count one, so every store below writes
/// in place and the tag read afterwards is the array's own.
unsafe fn sole_holder(
    ctx: *mut crate::memory::context::LLContext,
    arena: *mut Arena,
    src: *mut LLArray,
    name: &str,
) -> (*mut crate::object::Object, *mut Value) {
    let class = ClassBuilder::new(name).prop("a", true).build();
    let h = unsafe { new_constructed(ctx, class, MemoryCategory::GcHeap) };
    let slot = unsafe { Object::prop_at(h, 16) };
    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena,
            h as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        ll_release(src as *mut RcHeader);
    }

    (h, slot)
}

fn tag(a: *mut LLArray) -> crate::array::head::StorageTag {
    unsafe { (*crate::array::entity::storage_head(a)).tag() }
}

/// A string key on a fresh array: the array arrives a vector, which
/// holds no key at all, and the store lands in a hash that also still
/// holds what the vector held under the integer key its position was.
#[test]
fn a_string_key_migrates_the_array_and_stores_into_the_hash() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    assert_eq!(tag(src), crate::array::head::StorageTag::Vector);
    let (h, slot) = unsafe { sole_holder(context_ptr, arena_ptr, src, "StringKeyHolder") };

    let key = mk(b"name");
    unsafe {
        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(0),
            Value::int(7),
        ));
        assert_eq!(
            tag(src),
            crate::array::head::StorageTag::Vector,
            "position zero is inside the dense range"
        );

        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Str(key),
            Value::int(9),
        ));
        assert_eq!(
            tag(src),
            crate::array::head::StorageTag::Hash,
            "a string key is what the dense range cannot hold"
        );
        assert_eq!(
            crate::array::element::get(slot, Key::Str(key))
                .unwrap()
                .as_int(),
            9
        );
        assert_eq!(
            crate::array::element::get(slot, Key::Int(0))
                .unwrap()
                .as_int(),
            7,
            "the migration lost what the vector held"
        );

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
        assert!(ll_release(key as *mut RcHeader));
        crate::object::ll_entity_die(key as *mut RcHeader);
    }
}

/// An integer key past the range: the same migration, reached by the
/// other half of the test `representation_for` makes. The key stored
/// is the one asked for rather than the next position, which is the
/// half a vector could otherwise fake by appending.
#[test]
fn an_integer_key_past_the_dense_range_migrates() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let (h, slot) = unsafe { sole_holder(context_ptr, arena_ptr, src, "SparseKeyHolder") };

    unsafe {
        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(0),
            Value::int(7),
        ));
        assert_eq!(
            tag(src),
            crate::array::head::StorageTag::Vector,
            "the array under test is the hash already, so the key decides nothing"
        );
        assert!(set(
            context_ptr,
            MemoryCategory::GcHeap,
            slot,
            Key::Int(5),
            Value::int(9),
        ));
        assert_eq!(
            tag(src),
            crate::array::head::StorageTag::Hash,
            "key 5 over a range of one is a hole a dense range has no bytes for"
        );
        assert_eq!(
            crate::array::element::get(slot, Key::Int(5))
                .unwrap()
                .as_int(),
            9
        );
        assert!(
            crate::array::element::get(slot, Key::Int(1)).is_none(),
            "the migration invented an entry between the two keys"
        );
        assert_eq!(
            crate::array::element::get(slot, Key::Int(0))
                .unwrap()
                .as_int(),
            7
        );

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
    }
}

/// The negative control, without which neither test above attributes
/// the migration to the key: a write the dense range can hold leaves
/// the array a vector, so what migrates is the key rather than the
/// store.
#[test]
fn a_key_inside_the_dense_range_leaves_the_vector_standing() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

    let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let (h, slot) = unsafe { sole_holder(context_ptr, arena_ptr, src, "DenseKeyHolder") };

    unsafe {
        for (key, value) in [(0i64, 7i64), (1, 8), (2, 9), (0, 10)] {
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(key),
                Value::int(value),
            ));
        }

        assert_eq!(
            tag(src),
            crate::array::head::StorageTag::Vector,
            "an append at the cursor and an overwrite are both the range's own"
        );
        assert_eq!(
            crate::array::element::get(slot, Key::Int(0))
                .unwrap()
                .as_int(),
            10,
            "the overwrite went somewhere else"
        );
        assert_eq!(crate::array::testing::used(src), 3);

        assert!(ll_release(h as *mut RcHeader));
        ll_object_die(h);
    }
}
