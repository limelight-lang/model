//! Everything is measured before anything is allocated, so a value
//! whose text the crate cannot yet produce stops the whole
//! flattening rather than leaving a partial result. The factory
//! assembles in place, which makes it a second maker of the layout
//! choice `ll_string_new` makes when it copies, and a result in a
//! category another thread can reach is hashed before publication
//! because two threads would race to fill the lazy field.

use super::*;

/// A result past what the category packs in one slot takes the
/// out-of-line layout: this is the assemble-in-place factory's half
/// of the choice `ll_string_new` makes when it copies, and the two
/// write to different places, so it needs its own test.
#[test]
fn a_flattened_result_past_the_slot_limit_is_out_of_line() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["head:", ":tail"]);

    with_ctx(|ctx| {
        let long = vec![b'v'; crate::memory::heap::MAX_SMALL];
        let value = unsafe { ll_string_new(ctx, MemoryCategory::GcHeap, &long) };
        let held = [Value::entity(Tag::String, value as *mut RcHeader)];
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::GcHeap) };
        let out = unsafe { flatten(ctx, t, MemoryCategory::GcHeap) };
        assert!(!out.is_null());
        assert!(
            crate::string::bytes_are_out_of_line(unsafe {
                crate::refcount::header_flags(out as *const RcHeader)
            }),
            "the assembled result did not fit one slot"
        );

        let mut want = b"head:".to_vec();
        want.extend_from_slice(&long);
        want.extend_from_slice(b":tail");
        assert_eq!(unsafe { crate::string::string_bytes(out) }, &want[..]);

        unsafe {
            assert!(ll_release(out as *mut RcHeader));
            crate::object::ll_entity_die(out as *mut RcHeader);
            assert!(ll_release(t as *mut RcHeader));
            crate::object::ll_entity_die(t as *mut RcHeader);
            assert!(ll_release(value as *mut RcHeader));
            crate::object::ll_entity_die(value as *mut RcHeader);
        }
    });
}

/// A value whose text needs machinery the crate does not have stops
/// the whole flattening, before anything is allocated — a partial
/// result would be a wrong string rather than a missing one.
#[test]
fn a_value_with_no_text_yet_refuses_the_whole_flattening() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let plain = ClassBuilder::new("Plain").build();
    let shape = shape_of(&["v = ", ""]);

    with_ctx(|ctx| {
        let obj = unsafe { crate::object::ll_object_new(ctx, plain, MemoryCategory::RequestArena) };
        let held = [Value::entity(Tag::Object, obj as *mut RcHeader)];
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
        assert!(
            unsafe { flatten(ctx, t, MemoryCategory::RequestArena) }.is_null(),
            "an object needs __toString, which is user code with no call path yet"
        );

        let float = shape_of(&["", ""]);
        let held = [Value::float(1.5)];
        let t2 = unsafe { ll_template_new(ctx, cls, &*float, &held, MemoryCategory::RequestArena) };
        assert!(
            unsafe { flatten(ctx, t2, MemoryCategory::RequestArena) }.is_null(),
            "a float needs the language's precision rules, which are undecided"
        );
    });
}

/// A result in a category another thread can reach cannot carry the
/// lazy hash — two threads would race to fill one field — so it is
/// hashed before it is published, and by content.
#[test]
fn a_shared_result_is_hashed_before_it_is_published() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["a", "b"]);

    with_ctx(|ctx| {
        let held = [Value::int(1)];
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
        let out = unsafe { flatten(ctx, t, MemoryCategory::LongLived) };
        assert_eq!(unsafe { crate::string::string_bytes(out) }, b"a1b");
        assert_eq!(
            unsafe { (*out).hash },
            crate::hash::hash_bytes(b"a1b"),
            "a shared string must arrive already hashed"
        );
    });
}
