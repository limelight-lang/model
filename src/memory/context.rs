//! Allocation context and the extern "C" ABI surface.
//!
//! Generated code carries `LLContext*` in a dedicated register and
//! passes it as the first argument. `NULL` is legal: the runtime falls
//! back to the thread-local current context (calls from the C++ layer,
//! host code, FFI).

use std::cell::Cell;

use crate::memory::arena::Arena;
use crate::memory::heap::with_thread_heap;
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
pub(crate) fn resolve_arena<'a>(ctx: *mut LLContext) -> &'a mut Arena {
    resolve(ctx)
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

/// Allocate immortal memory (never freed: class metadata, interned
/// strings). `ctx` is accepted for ABI uniformity but ignored — the
/// immortal region is process-global.
///
/// # Safety
/// Callable from any thread with an initialized runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_immortal_alloc(_ctx: *mut LLContext, size: usize) -> *mut u8 {
    crate::memory::immortal::immortal_alloc(size)
}

/// Allocate a small long-lived object on the thread heap (individually
/// freeable, unlike arena objects). `ctx` is accepted for ABI uniformity
/// but the heap is thread-persistent, not per-request.
///
/// # Safety
/// Callable from any thread with an initialized runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_heap_alloc(_ctx: *mut LLContext, size: usize) -> *mut u8 {
    with_thread_heap(|heap| heap.alloc(size))
}

/// Free a small object previously returned by [`ll_heap_alloc`].
///
/// # Safety
/// `ptr` must have come from [`ll_heap_alloc`] on this thread and not
/// been freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_heap_free(_ctx: *mut LLContext, ptr: *mut u8) {
    with_thread_heap(|heap| unsafe { heap.free(ptr) })
}

#[cfg(test)]
mod tests {
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
}
