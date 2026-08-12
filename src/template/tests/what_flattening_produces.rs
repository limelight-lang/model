//! Parts and values alternate, part first and part last, which is
//! what lets an empty part be ordinary and needs no offset map. An
//! integer and `true` convert as PHP converts them, and `false` and
//! null render as empty text rather than being refused the way a float
//! and an object are.

use super::*;

/// Parts and values alternate, part first and part last, and an
/// integer and `true` convert as PHP converts them.
#[test]
fn flattening_alternates_parts_and_values() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["id = ", ", name = ", ", ok = ", "!"]);

    with_ctx(|ctx| {
        let name =
            unsafe { ll_string_new(ctx, MemoryCategory::RequestArena, "édouard".as_bytes()) };
        let held = [
            Value::int(-42),
            Value::entity(Tag::String, name as *mut RcHeader),
            Value::bool(true),
        ];
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
        let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
        assert!(!out.is_null());
        assert_eq!(
            unsafe { crate::string::string_bytes(out) },
            "id = -42, name = édouard, ok = 1!".as_bytes()
        );
    });
}

/// An empty part is ordinary — `"$a$b"` is three parts, two of them
/// empty — and it is what makes the alternation need no offset map.
#[test]
fn empty_parts_are_ordinary() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["", "", ""]);

    with_ctx(|ctx| {
        let held = [Value::int(7), Value::int(8)];
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
        let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
        assert_eq!(unsafe { crate::string::string_bytes(out) }, b"78");
    });
}

/// `false` is empty text in PHP, and empty is a length the measuring
/// pass answers rather than a value it declines: a `false` dropped
/// from that arm falls to the refusal a float and an object take, and
/// the whole flattening reports null. The parts around the hole are
/// there to give the emptiness somewhere to show — they do not
/// separate a hole measured at zero from one skipped in both passes,
/// which no output can.
#[test]
fn false_is_empty_text_between_its_parts() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["<", ">"]);

    with_ctx(|ctx| {
        let held = [Value::bool(false)];
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
        let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
        assert!(!out.is_null(), "`false` is rendered, not refused");
        assert_eq!(unsafe { crate::string::string_bytes(out) }, b"<>");
    });
}

/// Null the same, and separately: the two share one arm today, and a
/// single test over both would go on passing if one of them left it.
#[test]
fn null_is_empty_text_between_its_parts() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["<", ">"]);

    with_ctx(|ctx| {
        let held = [Value::null()];
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
        let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
        assert!(!out.is_null(), "null is rendered, not refused");
        assert_eq!(unsafe { crate::string::string_bytes(out) }, b"<>");
    });
}

/// The one integer whose absolute value does not fit its own type.
#[test]
fn the_smallest_integer_writes_all_of_itself() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("InterpolatedString").template().build();
    let shape = shape_of(&["", ""]);

    with_ctx(|ctx| {
        let held = [Value::int(i64::MIN)];
        let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
        let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
        assert_eq!(
            unsafe { crate::string::string_bytes(out) },
            b"-9223372036854775808"
        );
    });
}
