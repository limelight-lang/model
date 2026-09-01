//! The C entry takes the context explicitly, which is the shape
//! generated code has and the one a null context falls back from.

use super::*;

#[test]
fn abi_entry_works_with_explicit_context() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = arena();
    let mut ctx = LLContext { arena: &mut a };
    let mut b = Buffer::new();

    let p = unsafe { ll_buffer_ensure(&mut ctx, &mut b, 64, 0) };
    assert!(!p.is_null());
    assert_eq!(p, b.data);
    unsafe { crate::memory::context::ll_arena_reset(&mut ctx) };
}
