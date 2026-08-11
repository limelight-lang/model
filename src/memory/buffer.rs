//! Growable-buffer primitive: `{ data, len, capacity }`, no `RcHeader`.
//!
//! Not a heap entity (`rfc/model/memory/buffers.md`): three
//! words the owner embeds or keeps on the stack; only the `data` payload
//! moves. Entities that need a lifecycle (mutable strings) embed a
//! buffer and put their own header in front.
//!
//! This module covers the **request-arena** payload path
//! (`rfc/model/memory/buffers.md` "Per-Category Growth"): extend in
//! place when the payload is the top of the arena bump, else a fresh
//! payload and a copy — the abandoned payload is arena garbage,
//! reclaimed for free at reset. Payloads larger than a block go
//! OS-direct and are tracked by the arena, freed at its reset. The
//! long-lived buffer arena (`BLOCK_KIND_BUFFER`) is a separate layer.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::memory::arena::{Arena, round_up_8};
use crate::memory::block_pool::BLOCK_PAYLOAD;
use crate::memory::context::{LLContext, resolve_arena};

/// Global memory-pressure mode (`rfc/model/memory/buffers.md`): governs
/// growth slack. One load + branch, same shape as the GC activity bit.
/// Thresholds that switch it automatically are blocked on real
/// workloads; until then it is set manually.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PressureMode {
    Plenty = 0,
    Tight = 1,
    Critical = 2,
}

static PRESSURE_MODE: AtomicU32 = AtomicU32::new(PressureMode::Plenty as u32);

pub fn set_pressure_mode(mode: PressureMode) {
    PRESSURE_MODE.store(mode as u32, Ordering::Relaxed);
}

pub fn pressure_mode() -> PressureMode {
    // Safety of transmute: only the three variants are ever stored.
    unsafe { core::mem::transmute(PRESSURE_MODE.load(Ordering::Relaxed)) }
}

/// The three-word buffer. `capacity` is always as-allocated (8-rounded);
/// `data` is null iff `capacity == 0`.
#[repr(C)]
pub struct Buffer {
    pub data: *mut u8,
    pub len: usize,
    pub capacity: usize,
}

impl Buffer {
    pub const fn new() -> Self {
        Buffer {
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
        }
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Target capacity for a growth request: the mode decides the slack.
/// `hint` is the caller's growth recommendation (0 = unknown); the
/// active mode may override it, down to exact-size under `Critical`.
pub(crate) fn desired_capacity(current: usize, min_capacity: usize, hint: usize) -> usize {
    match pressure_mode() {
        PressureMode::Plenty => min_capacity.max(current.saturating_mul(2)).max(hint),
        PressureMode::Tight => min_capacity.max(hint),
        PressureMode::Critical => min_capacity,
    }
}

/// Ensure `buf` can hold `min_capacity` bytes; returns the (possibly
/// moved) payload pointer. Growth algorithm per `rfc/model/memory/buffers.md`:
/// enough capacity → nothing; top-of-bump → extend in place; otherwise
/// fresh payload + copy, the old payload dies with the arena.
///
/// **Null on exhaustion**, with `buf` left exactly as it was — the same
/// contract as every other allocation path here.
pub fn buffer_ensure(
    arena: &mut Arena,
    buf: &mut Buffer,
    min_capacity: usize,
    hint: usize,
) -> *mut u8 {
    if buf.capacity >= min_capacity {
        return buf.data;
    }

    let target = round_up_8(desired_capacity(buf.capacity, min_capacity, hint));

    // Extend in place: only possible for a block-sized payload sitting
    // at the arena's bump top.
    if !buf.data.is_null()
        && target <= BLOCK_PAYLOAD
        && arena.try_extend_in_place(buf.data, buf.capacity, target)
    {
        buf.capacity = target;
        return buf.data;
    }

    // Fresh payload: in-block when it fits, OS-direct (arena-tracked,
    // freed at reset) when it does not.
    let new_data = arena.alloc_body(target);

    if new_data.is_null() {
        // Out of memory. Leave the buffer exactly as it was — old payload,
        // old capacity, still valid — and report, the same contract as
        // `buffer_ensure_longlived`. Stamping the null in would give the
        // caller a buffer claiming capacity over no memory at all.
        return std::ptr::null_mut();
    }

    if buf.len > 0 {
        unsafe { std::ptr::copy_nonoverlapping(buf.data, new_data, buf.len) };
    }

    // The old payload is not freed: in-block it is arena garbage
    // (reclaimed at reset), OS-direct it is tracked by the arena.
    buf.data = new_data;
    buf.capacity = target;
    buf.data
}

/// Append `bytes` to `buf`, growing as needed. False when the growth
/// was refused: nothing is appended and `buf` is untouched.
pub fn buffer_append(arena: &mut Arena, buf: &mut Buffer, bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true; // nothing to grow for, and an empty buffer has no payload
    }

    let needed = buf.len + bytes.len();
    if buffer_ensure(arena, buf, needed, 0).is_null() {
        return false;
    }

    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.data.add(buf.len), bytes.len()) };
    buf.len = needed;
    true
}

// --- C ABI ---------------------------------------------------------------

/// Ensure the buffer holds `min_capacity`; returns the payload pointer,
/// or null if memory ran out (the buffer keeps its old payload).
/// `hint` is a growth recommendation (0 = let the mode decide).
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`]; `buf` must
/// point to a live, correctly-initialized `Buffer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_buffer_ensure(
    ctx: *mut LLContext,
    buf: *mut Buffer,
    min_capacity: usize,
    hint: usize,
) -> *mut u8 {
    // A leaf: it only allocates, never runs user code, so a borrow that
    // lasts just this call cannot overlap a reentrant one (audit H5).
    buffer_ensure(
        unsafe { &mut *resolve_arena(ctx) },
        unsafe { &mut *buf },
        min_capacity,
        hint,
    )
}

#[cfg(test)]
mod tests;
