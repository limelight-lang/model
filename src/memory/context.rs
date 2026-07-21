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

/// The mounted arena, as a raw pointer.
///
/// Deliberately **not** `&mut Arena`. Destructors are documented to run
/// reentrantly on the reset path (`rfc/model/memory/arena-reset.md`):
/// `arena_reset_full` is working on the arena when a `__destruct` it
/// invoked calls back in here and resolves the very same arena. Handing
/// out `&mut Arena` made those two borrows overlap — two live `&mut` to
/// one object, which is UB regardless of the machine code being fine
/// (audit H5; Miri rejects it as soon as it can reach the path).
///
/// A raw pointer has no such exclusivity claim. Callers materialize a
/// `&mut` for one leaf operation at a time and must never hold it across
/// a call that can run user code.
#[inline]
pub(crate) fn resolve_arena(ctx: *mut LLContext) -> *mut Arena {
    resolve(ctx)
}

#[inline]
fn resolve(ctx: *mut LLContext) -> *mut Arena {
    let ctx = if ctx.is_null() {
        CURRENT_CONTEXT.with(|c| c.get())
    } else {
        ctx
    };
    if ctx.is_null() {
        no_context();
    }

    let arena = unsafe { (*ctx).arena };
    if arena.is_null() {
        no_arena();
    }
    arena
}

/// The two ways [`resolve`] can fail, out of line.
///
/// These were `assert!`s. The message is built at the call site, so the
/// panic machinery inlined into every hot ABI entry that resolves a
/// context — `ll_arena_alloc`, `ll_ref_store`, `ll_object_new` — as two
/// `alloca [48 x i8]`, on functions that otherwise need no stack at all.
/// `#[cold]` + `#[inline(never)]` moves only the panic out; both checks
/// still run on every call. Measured on `ll_arena_alloc`: prologue
/// 88 -> 40 bytes, body 58 -> 35 instructions. The remaining 40 is
/// shadow space the Windows x64 ABI demands for these two calls, so the
/// entry does not become frameless — that would need the checks gone,
/// not merely moved.
#[cold]
#[inline(never)]
fn no_context() -> ! {
    panic!("no allocation context on this thread");
}

#[cold]
#[inline(never)]
fn no_arena() -> ! {
    panic!("context has no arena");
}

/// # Safety
/// `ctx` must be null or point to a live `LLContext` whose arena is live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_arena_alloc(ctx: *mut LLContext, size: usize) -> *mut u8 {
    unsafe { (*resolve(ctx)).alloc(size) }
}

/// # Safety
/// Same contract as [`ll_arena_alloc`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_arena_reserve(ctx: *mut LLContext, bytes: usize) {
    unsafe { (*resolve(ctx)).reserve(bytes) }
}

/// # Safety
/// Same contract as [`ll_arena_alloc`]; `obj` must point to a live
/// entity beginning with `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_arena_track_destructor(ctx: *mut LLContext, obj: *mut RcHeader) {
    unsafe { (*resolve(ctx)).track_destructor(obj) }
}

/// # Safety
/// Same contract as [`ll_arena_alloc`]; running PHP code must no
/// longer reference the arena (destructors invoked from here may).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_arena_reset(ctx: *mut LLContext) {
    // The full discipline: destructor/escape fixpoint, promotion by
    // retention, deferred releases (rfc/model/memory/arena-reset.md).
    unsafe { crate::promote::arena_reset_full(resolve(ctx)) }
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
    unsafe { with_thread_heap(|heap| heap.alloc(size)) }
}

/// Free a small object previously returned by [`ll_heap_alloc`].
///
/// # Safety
/// `ptr` must have come from [`ll_heap_alloc`] on this thread and not
/// been freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_heap_free(_ctx: *mut LLContext, ptr: *mut u8) {
    unsafe { with_thread_heap(|heap| heap.free(ptr)) }
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
