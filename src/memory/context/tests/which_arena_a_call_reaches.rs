//! The context pointer is the compiler's channel to the arena, and a
//! null one is the shape a call from outside a request has. These pin
//! which arena a call lands in, not what the arena then does.

use super::*;

#[test]
fn null_context_falls_back_to_thread_local() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    set_current_context(&mut ctx);

    let p = unsafe { ll_arena_alloc(std::ptr::null_mut(), 40) };
    assert!(!p.is_null());

    set_current_context(std::ptr::null_mut());
    unsafe { ll_arena_reset(&mut ctx) };
}

#[test]
fn explicit_context_wins() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let a = unsafe { ll_arena_alloc(&mut ctx, 8) };
    let b = unsafe { ll_arena_alloc(&mut ctx, 8) };
    assert_eq!(b as usize - a as usize, 8);

    unsafe { ll_arena_reset(&mut ctx) };
}
