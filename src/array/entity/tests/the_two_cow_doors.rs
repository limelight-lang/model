//! One body serves both doors with the destination category
//! supplying the depth: a shared array hands back a different array
//! with the same order and the children shared, while an arena array
//! taken by a longer-lived holder is copied out and its arena COW
//! children with it. The replay copies live entries only, so the
//! append cursor is carried rather than derived — a hole under the
//! highest key spent leaves no witness in the copy.

use super::*;

/// The COW door. A shared array asked to separate must hand back a
/// **different** array; returning the original is a write into a value
/// two holders share, which in release happens with no signal at all.
#[test]
fn a_shared_array_separates_into_a_copy_of_its_own() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    let key = mk(b"k");
    let value = mk(b"v");
    unsafe {
        // `insert` writes the entry raw and leaves the counting to the
        // caller, so these are the source array's own references — and
        // they are taken first, because an entry a walker can reach
        // must already be backed by a count.
        crate::refcount::ll_retain(key as *mut RcHeader);
        crate::refcount::ll_retain(value as *mut RcHeader);
        crate::array::testing::insert(
            src,
            Key::Str(key),
            Value::entity(crate::value::Tag::String, value as *mut RcHeader),
        );
    }

    // A second holder is what makes the write a separation.
    unsafe { crate::refcount::ll_retain(src as *mut RcHeader) };

    let copy = unsafe {
        crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, src as *mut RcHeader)
    } as *mut LLArray;
    assert_ne!(copy, src, "the shared array was written in place");
    assert_eq!(
        unsafe { crate::array::testing::used(copy) },
        1,
        "the entry did not survive"
    );
    // Three each: this test, the source array, and the copy.
    assert_eq!(
        unsafe { (*(key as *mut RcHeader)).refcount },
        3,
        "the copy did not take a reference to the key"
    );
    assert_eq!(
        unsafe { (*(value as *mut RcHeader)).refcount },
        3,
        "the copy did not take a reference to the element"
    );

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(!ll_release(src as *mut RcHeader));
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
        assert!(ll_release(key as *mut RcHeader));
        crate::object::ll_entity_die(key as *mut RcHeader);
        assert!(ll_release(value as *mut RcHeader));
        crate::object::ll_entity_die(value as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// The rule reads the category before the count: a heap array at
/// count 1 is exclusively owned and writes in place.
#[test]
fn separation_is_needed_only_when_the_array_is_shared() {
    let _g = crate::memory::block_pool::test_guard();
    let a = hash_arr();
    unsafe {
        assert!(!needs_separation(a), "count 1 writes in place");
        crate::refcount::ll_retain(a as *mut RcHeader);
        assert!(needs_separation(a), "a second holder forces a copy");
        crate::refcount::ll_release(a as *mut RcHeader);
        crate::array::entity::dispose_storage(a, category_of(a));
    }
}

#[test]
fn separation_copies_the_order_and_shares_the_children() {
    let _g = crate::memory::block_pool::test_guard();
    let src = hash_arr();
    let key = mk(b"shared");
    let child = mk(b"child-value");
    unsafe {
        crate::refcount::ll_retain(key as *mut RcHeader);
        crate::refcount::ll_retain(child as *mut RcHeader);
        crate::array::testing::insert(src, Key::Int(1), Value::int(10));
        crate::array::testing::insert(
            src,
            Key::Str(key),
            Value::entity(crate::value::Tag::String, child as *mut RcHeader),
        );
        crate::array::testing::insert(src, Key::Int(2), Value::int(20));
    }

    let before_key = unsafe { (*(key as *mut RcHeader)).refcount };
    let before_child = unsafe { (*(child as *mut RcHeader)).refcount };

    let dst = unsafe {
        separate(
            src,
            MemoryCategory::GcHeap,
            std::ptr::null_mut(),
            CopyReason::Duplication,
        )
    };

    assert!(!dst.is_null());

    unsafe {
        // Order survives.
        let order: Vec<i64> = crate::array::testing::iter(dst)
            .map(|e| {
                if e.is_int_key() {
                    e.hash_or_key as i64
                } else {
                    -1
                }
            })
            .collect();
        assert_eq!(order, vec![1, -1, 2]);

        // The children are shared, each counted once more.
        assert_eq!((*(key as *mut RcHeader)).refcount, before_key + 1);
        assert_eq!((*(child as *mut RcHeader)).refcount, before_child + 1);

        // Writing the copy does not touch the source.
        crate::array::testing::insert(dst, Key::Int(1), Value::int(999));
        assert_eq!(
            crate::array::testing::get(src, Key::Int(1))
                .unwrap()
                .as_int(),
            10
        );
        assert_eq!(
            crate::array::testing::get(dst, Key::Int(1))
                .unwrap()
                .as_int(),
            999
        );

        release_children(dst);
        crate::array::entity::dispose_storage(dst, category_of(dst));
        release_children(src);
        crate::array::entity::dispose_storage(src, category_of(src));
    }
}

#[test]
fn separation_carries_holes_away_rather_than_copying_them() {
    let _g = crate::memory::block_pool::test_guard();
    let src = hash_arr();
    unsafe {
        for i in 0..10i64 {
            crate::array::testing::insert(src, Key::Int(i), Value::int(i));
        }

        for i in [2i64, 5, 8] {
            let _ = crate::array::testing::remove(src, Key::Int(i));
        }

        let dst = separate(
            src,
            MemoryCategory::GcHeap,
            std::ptr::null_mut(),
            CopyReason::Duplication,
        );
        assert!(!dst.is_null());
        assert_eq!(crate::array::testing::table(dst).len(), 7);
        assert_eq!(
            crate::array::testing::used(dst),
            7,
            "the copy starts dense: a hole is not worth copying"
        );
        let order: Vec<i64> = crate::array::testing::iter(dst)
            .map(|e| e.hash_or_key as i64)
            .collect();
        assert_eq!(order, vec![0, 1, 3, 4, 6, 7, 9]);

        crate::array::entity::dispose_storage(dst, category_of(dst));
        crate::array::entity::dispose_storage(src, category_of(src));
    }
}

/// The escape door. An arena array taken by a longer-lived holder is
/// copied out, and its arena COW children are copied with it — a hold
/// on arena memory in a heap slot dangles at the reset.
#[test]
fn an_arena_array_taken_by_a_heap_holder_is_copied_out_with_its_children() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    // `ll_array_new` takes no context and resolves this thread's, so
    // an arena array needs one mounted. One raw pointer, reused: a
    // fresh `&mut` per call retags and invalidates what TLS holds
    // (`dev/WORKFLOW.md`, Miri).
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let holder_class = crate::class::ClassBuilder::new("ArrayHolder")
        .prop("a", true)
        .build();
    let holder = unsafe {
        crate::object::new_constructed(context_ptr, holder_class, MemoryCategory::GcHeap)
    };

    let src = unsafe { crate::array::testing::hash_array(MemoryCategory::RequestArena) };
    let element =
        unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"in the arena") };
    unsafe {
        crate::refcount::ll_retain(element as *mut RcHeader);
        crate::array::testing::insert(
            src,
            Key::Int(1),
            Value::entity(crate::value::Tag::String, element as *mut RcHeader),
        );
    }

    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            holder as *mut RcHeader,
            crate::object::Object::prop_at(holder, 16),
            std::ptr::null_mut(),
            Value::entity(crate::value::Tag::Array, src as *mut RcHeader),
        ));
    }

    let stored =
        unsafe { (*crate::object::Object::prop_at(holder, 16)).entity_ptr() } as *mut LLArray;
    assert_ne!(stored, src, "the heap slot took the arena array itself");
    assert_eq!(
        unsafe { (*stored).rc.memory_category() },
        MemoryCategory::GcHeap,
        "the copy did not land in the heap"
    );
    let copied_element = unsafe { crate::array::testing::entry(stored, 0).value().entity_ptr() };
    assert_ne!(
        copied_element, element as *mut RcHeader,
        "the copy still holds the arena string"
    );
    assert_eq!(
        unsafe { crate::object::header_category(copied_element) },
        MemoryCategory::GcHeap,
        "the copied element did not leave the arena"
    );

    unsafe {
        assert!(ll_release(holder as *mut RcHeader));
        crate::object::ll_object_die(holder);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}

/// The append cursor survives the copy, where the replay cannot
/// carry it: `fill_from` copies live entries only, so a hole under
/// the highest key ever inserted has no witness in the copy — PHP
/// appends `[9 => 'x']` minus its 9 at 10, and a copy that answered
/// 0 would hand back keys the source already spent.
#[test]
fn a_copy_inherits_the_append_cursor_over_a_hole() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

    let src = hash_arr();
    unsafe {
        crate::array::testing::insert(src, Key::Int(9), Value::int(1));
        let _ = crate::array::testing::remove(src, Key::Int(9));
        assert_eq!(crate::array::testing::table(src).append_key(), Some(10));
    }

    let copy = unsafe {
        separate(
            src,
            MemoryCategory::GcHeap,
            arena_ptr,
            CopyReason::Duplication,
        )
    };

    assert!(!copy.is_null());
    unsafe {
        assert_eq!(
            crate::array::testing::table(copy).len(),
            0,
            "a hole is not worth copying"
        );
        assert_eq!(
            crate::array::testing::table(copy).append_key(),
            Some(10),
            "the copy rewound the append cursor past a removed key"
        );
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
    }
}

/// The copy takes the source's representation, so separating a vector
/// hands back a vector (`rfc/model/arrays.md`, "External Contract":
/// "Separation copies the storage in its current representation"). A copy
/// into the ordered hash would answer every later question the same way
/// and pay twice the bytes for it.
#[test]
fn a_shared_vector_separates_into_a_vector_copy() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

    let src = unsafe { crate::array::entity::new_vector_array(MemoryCategory::GcHeap) };
    let first = mk(b"first");
    let second = mk(b"second");
    unsafe {
        // The vector's own references, taken before the elements are
        // published, for the reason the hash's tests take them: an
        // element a walker can reach is already backed by a count.
        crate::refcount::ll_retain(first as *mut RcHeader);
        crate::refcount::ll_retain(second as *mut RcHeader);
        assert!(crate::array::testing::push(
            src,
            Value::entity(crate::value::Tag::String, first as *mut RcHeader)
        ));
        assert!(crate::array::testing::push(
            src,
            Value::entity(crate::value::Tag::String, second as *mut RcHeader)
        ));
    }

    unsafe { crate::refcount::ll_retain(src as *mut RcHeader) };
    let copy = unsafe {
        crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, src as *mut RcHeader)
    } as *mut LLArray;
    assert_ne!(copy, src, "the shared array was written in place");

    let (vector, head) = unsafe { crate::array::entity::as_vector(copy) };
    assert_eq!(
        head.tag(),
        crate::array::head::StorageTag::Vector,
        "the copy is in the source's representation"
    );
    assert_eq!(head.used(), 2);
    assert_eq!(
        vector.get(head, 0).unwrap().entity_ptr(),
        first as *mut RcHeader,
        "the children are shared, not copied"
    );
    assert_eq!(
        vector.get(head, 1).unwrap().entity_ptr(),
        second as *mut RcHeader
    );
    // Three each: this test, the source vector, and the copy.
    assert_eq!(
        unsafe { crate::refcount::header_refcount(first as *mut RcHeader) },
        3,
        "the copy took a reference of its own on the shared child"
    );

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(!ll_release(src as *mut RcHeader));
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
        assert!(ll_release(first as *mut RcHeader));
        crate::object::ll_entity_die(first as *mut RcHeader);
        assert!(ll_release(second as *mut RcHeader));
        crate::object::ll_entity_die(second as *mut RcHeader);
    }
}

/// The copy's representation comes from the source and from nothing
/// else. The source here is built by [`new_with_storage`] rather than by
/// `ll_array_new`, so the pin holds whichever representation the factory
/// stamps for a fresh array — a destination taken from the factory
/// instead answers the question "what does an empty array start as",
/// which is a different question with a different answer.
#[test]
fn a_hash_source_is_copied_into_a_hash_whatever_the_factory_stamps() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

    let src = unsafe {
        crate::array::entity::new_with_storage(
            MemoryCategory::GcHeap,
            crate::array::head::StorageTag::Hash,
            crate::array::entity::Storage::hash(),
        )
    };
    let value = mk(b"v");
    unsafe {
        crate::refcount::ll_retain(value as *mut RcHeader);
        crate::array::testing::insert(
            src,
            Key::Int(7),
            Value::entity(crate::value::Tag::String, value as *mut RcHeader),
        );
    }

    unsafe { crate::refcount::ll_retain(src as *mut RcHeader) };
    let copy = unsafe {
        crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, src as *mut RcHeader)
    } as *mut LLArray;
    assert_ne!(copy, src, "the shared array was written in place");

    let (_, copy_head) = unsafe { crate::array::entity::as_table(copy) };
    assert_eq!(
        copy_head.tag(),
        crate::array::head::StorageTag::Hash,
        "the copy is in the source's representation"
    );
    assert_eq!(copy_head.used(), 1, "the entry did not survive");

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(!ll_release(src as *mut RcHeader));
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
        assert!(ll_release(value as *mut RcHeader));
        crate::object::ll_entity_die(value as *mut RcHeader);
    }

    arena.reset(|_| {});
}

/// A duplication collapses a reference the source's entry alone names
/// and carries one a second name holds, and a vector's positions meet
/// that rule as often as a hash's keys do. Position 0 holds the box
/// nobody else names, position 1 the box a second name holds.
#[test]
fn a_duplicated_vector_collapses_the_box_its_entry_alone_names() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

    let src = unsafe { crate::array::entity::new_vector_array(MemoryCategory::GcHeap) };
    let inner = mk(b"inner");
    let alone = crate::reference::ll_reference_new();
    let shared_box = crate::reference::ll_reference_new();
    unsafe {
        // The box's own reference on what it holds. The boxes themselves
        // arrive at count one, and the entries take that count over.
        crate::refcount::ll_retain(inner as *mut RcHeader);
        crate::memory::barrier::write_value_slot(
            &raw mut (*alone).value,
            Value::entity(crate::value::Tag::String, inner as *mut RcHeader),
        );
        crate::memory::barrier::write_value_slot(&raw mut (*shared_box).value, Value::int(5));
        assert!(crate::array::testing::push(
            src,
            Value::entity(crate::value::Tag::Reference, alone as *mut RcHeader)
        ));
        assert!(crate::array::testing::push(
            src,
            Value::entity(crate::value::Tag::Reference, shared_box as *mut RcHeader)
        ));
        // The second name on the second box, which is what stops the
        // duplication from collapsing it.
        crate::refcount::ll_retain(shared_box as *mut RcHeader);
    }

    unsafe { crate::refcount::ll_retain(src as *mut RcHeader) };
    let copy = unsafe {
        crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, src as *mut RcHeader)
    } as *mut LLArray;
    assert_ne!(copy, src, "the shared array was written in place");

    let (vector, head) = unsafe { crate::array::entity::as_vector(copy) };
    assert_eq!(head.used(), 2, "the copy stopped short of the source");
    let collapsed = vector.get(head, 0).unwrap();
    assert_eq!(
        collapsed.tag(),
        crate::value::Tag::String,
        "the copy kept a box nobody else names"
    );
    assert_eq!(
        collapsed.entity_ptr(),
        inner as *mut RcHeader,
        "the collapse handed back something other than what the box held"
    );
    let carried = vector.get(head, 1).unwrap();
    assert_eq!(
        carried.tag(),
        crate::value::Tag::Reference,
        "the copy collapsed a box a second name holds"
    );
    assert_eq!(carried.entity_ptr(), shared_box as *mut RcHeader);
    assert_eq!(
        unsafe { crate::refcount::header_refcount(shared_box as *mut RcHeader) },
        3,
        "the copy did not take a reference of its own on the carried box"
    );

    unsafe {
        assert!(ll_release(copy as *mut RcHeader));
        crate::object::ll_entity_die(copy as *mut RcHeader);
        assert!(!ll_release(src as *mut RcHeader));
        assert!(ll_release(src as *mut RcHeader));
        crate::object::ll_entity_die(src as *mut RcHeader);
        assert!(ll_release(inner as *mut RcHeader));
        crate::object::ll_entity_die(inner as *mut RcHeader);
        assert!(ll_release(shared_box as *mut RcHeader));
        crate::object::ll_entity_die(shared_box as *mut RcHeader);
    }
}

/// The deep door over a vector: an arena vector taken by a heap holder is
/// copied out, and the nested arena vector inside it is copied in turn
/// through the work list rather than shared.
#[test]
fn an_arena_vector_taken_by_a_heap_holder_is_copied_out_with_its_children() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = crate::memory::arena::Arena::new();
    let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
    let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
    let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    crate::memory::context::set_current_context(context_ptr);

    let holder_class = crate::class::ClassBuilder::new("VectorHolder")
        .prop("a", true)
        .build();
    let holder = unsafe {
        crate::object::new_constructed(context_ptr, holder_class, MemoryCategory::GcHeap)
    };

    let src = unsafe { crate::array::entity::new_vector_array(MemoryCategory::RequestArena) };
    let nested = unsafe { crate::array::entity::new_vector_array(MemoryCategory::RequestArena) };
    let leaf = unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"deep") };
    // A second element behind the nested array, so the pass is pinned
    // past its first position: a fill that stopped after the recursion
    // would otherwise answer every assertion below.
    let tail = unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"tail") };
    unsafe {
        crate::refcount::ll_retain(leaf as *mut RcHeader);
        crate::refcount::ll_retain(tail as *mut RcHeader);
        assert!(crate::array::testing::push(
            nested,
            Value::entity(crate::value::Tag::String, leaf as *mut RcHeader)
        ));
        assert!(crate::array::testing::push(
            src,
            Value::entity(crate::value::Tag::Array, nested as *mut RcHeader)
        ));
        assert!(crate::array::testing::push(
            src,
            Value::entity(crate::value::Tag::String, tail as *mut RcHeader)
        ));
    }

    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            holder as *mut RcHeader,
            crate::object::Object::prop_at(holder, 16),
            std::ptr::null_mut(),
            Value::entity(crate::value::Tag::Array, src as *mut RcHeader),
        ));
    }

    let stored =
        unsafe { (*crate::object::Object::prop_at(holder, 16)).entity_ptr() } as *mut LLArray;
    assert_ne!(stored, src, "the heap slot took the arena array itself");
    let (copy, copy_head) = unsafe { crate::array::entity::as_vector(stored) };
    assert_eq!(
        copy_head.tag(),
        crate::array::head::StorageTag::Vector,
        "the copy is a vector too"
    );
    assert_eq!(
        copy_head.used(),
        2,
        "the copy stopped at the element that recursed"
    );

    let copied_tail = copy.get(copy_head, 1).unwrap().entity_ptr();
    assert_ne!(
        copied_tail, tail as *mut RcHeader,
        "the element after the recursion stayed in the arena"
    );
    assert_eq!(
        unsafe { crate::object::header_category(copied_tail) },
        MemoryCategory::GcHeap,
        "the element after the recursion did not leave the arena"
    );

    let copied_child = copy.get(copy_head, 0).unwrap().entity_ptr();
    assert_ne!(
        copied_child, nested as *mut RcHeader,
        "the nested arena array was shared rather than copied"
    );
    assert_eq!(
        unsafe { crate::object::header_category(copied_child) },
        MemoryCategory::GcHeap,
        "the nested copy did not leave the arena"
    );

    let (deep, deep_head) =
        unsafe { crate::array::entity::as_vector(copied_child as *mut LLArray) };
    let copied_leaf = deep.get(deep_head, 0).unwrap().entity_ptr();
    assert_ne!(
        copied_leaf, leaf as *mut RcHeader,
        "the leaf string stayed in the arena"
    );

    unsafe {
        assert!(ll_release(holder as *mut RcHeader));
        crate::object::ll_object_die(holder);
    }

    crate::memory::context::set_current_context(std::ptr::null_mut());
    arena.reset(|_| {});
}
