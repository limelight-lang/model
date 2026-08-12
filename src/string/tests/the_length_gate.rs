//! The 4 GiB gate every creation and growth path passes refuses
//! rather than truncating, and the string it refused is left exactly
//! as it was.

use super::*;

#[test]
fn an_append_past_the_cap_is_refused_with_the_string_untouched() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"near", 0) };
    // Claim a length just under the cap without owning the bytes:
    // the gate is arithmetic on `len`, and this is the only way to
    // reach it without four gigabytes.
    unsafe { (*s).len = MAX_LEN as u32 - 1 };
    assert!(
        !unsafe { ll_string_append(&mut ctx, s, b"xx") },
        "4 GiB is a refusal, not a truncation"
    );
    assert_eq!(unsafe { (*s).len }, MAX_LEN as u32 - 1, "untouched");

    unsafe {
        (*s).len = 4;
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    arena.reset(|_| {});
}

#[test]
fn content_past_the_cap_is_refused_rather_than_truncated() {
    assert!(fits(MAX_LEN));
    assert!(!fits(MAX_LEN + 1));
}
