//! Allocation context and the extern "C" ABI surface.
//!
//! Generated code carries `LLContext*` in a dedicated register and
//! passes it as the first argument. `NULL` is legal: the runtime falls
//! back to the thread-local current context (calls from the C++ layer,
//! host code, FFI).

use std::cell::Cell;

use crate::memory::arena::Arena;
use crate::memory::buffer::Buffer;
use crate::refcount::RcHeader;

/// The runtime context of the executing request. Grows over time
/// (exceptions, fibers); for now it owns the allocation state.
#[repr(C)]
pub struct LLContext {
    pub arena: *mut Arena,
}

thread_local! {
    static CURRENT_CONTEXT: Cell<*mut LLContext> = const { Cell::new(std::ptr::null_mut()) };
}

/// Install the current context for this thread (host/server loop).
pub fn set_current_context(ctx: *mut LLContext) {
    CURRENT_CONTEXT.with(|c| c.set(ctx));
}

#[inline]
fn resolve<'a>(ctx: *mut LLContext) -> &'a mut Arena {
    let ctx = if ctx.is_null() {
        CURRENT_CONTEXT.with(|c| c.get())
    } else {
        ctx
    };
    assert!(!ctx.is_null(), "no allocation context on this thread");

    let arena = unsafe { (*ctx).arena };
    assert!(!arena.is_null(), "context has no arena");
    unsafe { &mut *arena }
}

/// # Safety
/// `ctx` must be null or point to a live `LLContext` whose arena is live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_arena_alloc(ctx: *mut LLContext, size: usize) -> *mut u8 {
    resolve(ctx).alloc(size)
}

/// # Safety
/// Same contract as [`ll_arena_alloc`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_arena_reserve(ctx: *mut LLContext, bytes: usize) {
    resolve(ctx).reserve(bytes)
}

/// # Safety
/// Same contract as [`ll_arena_alloc`]; `obj` must point to a live
/// entity beginning with `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_arena_track_destructor(ctx: *mut LLContext, obj: *mut RcHeader) {
    resolve(ctx).track_destructor(obj)
}

/// # Safety
/// Same contract as [`ll_arena_alloc`]. Destructor execution is wired
/// by the object-lifecycle layer; until it lands the list is drained
/// without calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_arena_reset(ctx: *mut LLContext) {
    resolve(ctx).reset(|_obj| {
        // TODO(object-lifecycle): call the pre-destructor vtable slot.
    })
}

/// Initialize a mutable buffer into caller-provided storage `buf` (a
/// 3-word `Buffer` slot the caller owns — e.g. a field of a mutable
/// string entity), allocating its initial payload in the arena.
///
/// # Safety
/// Same contract as [`ll_arena_alloc`]; `buf` must point to writable
/// storage for one [`Buffer`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_buffer_init(ctx: *mut LLContext, buf: *mut Buffer, capacity: usize) {
    let b = Buffer::new_in(resolve(ctx), capacity);
    unsafe { buf.write(b) };
}

/// Ensure a buffer has at least `min_capacity` bytes; returns the
/// (possibly relocated) data pointer.
///
/// # Safety
/// Same contract as [`ll_arena_alloc`]; `buf` must point to a live
/// [`Buffer`] whose payload lives in this context's arena.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_buffer_ensure(
    ctx: *mut LLContext,
    buf: *mut Buffer,
    min_capacity: usize,
) -> *mut u8 {
    let arena = resolve(ctx);
    unsafe { (*buf).ensure(arena, min_capacity) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_context_falls_back_to_thread_local() {
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
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { ll_arena_alloc(&mut ctx, 8) };
        let b = unsafe { ll_arena_alloc(&mut ctx, 8) };
        assert_eq!(b as usize - a as usize, 8);

        unsafe { ll_arena_reset(&mut ctx) };
    }
}
