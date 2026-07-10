//! Growable-buffer primitive: `{ data, len, capacity }`, no `RcHeader`.
//!
//! Not a heap entity (`docs/memory-manager.md` "Mutable Buffers"): three
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
/// moved) payload pointer. Growth algorithm per `docs/memory-manager.md`:
/// enough capacity → nothing; top-of-bump → extend in place; otherwise
/// fresh payload + copy, the old payload dies with the arena.
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
    let new_data = if target <= BLOCK_PAYLOAD {
        arena.alloc(target)
    } else {
        arena.alloc_large(target)
    };

    if buf.len > 0 {
        unsafe { std::ptr::copy_nonoverlapping(buf.data, new_data, buf.len) };
    }
    // The old payload is not freed: in-block it is arena garbage
    // (reclaimed at reset), OS-direct it is tracked by the arena.
    buf.data = new_data;
    buf.capacity = target;
    buf.data
}

/// Append `bytes` to `buf`, growing as needed.
pub fn buffer_append(arena: &mut Arena, buf: &mut Buffer, bytes: &[u8]) {
    let needed = buf.len + bytes.len();
    buffer_ensure(arena, buf, needed, 0);
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.data.add(buf.len), bytes.len()) };
    buf.len = needed;
}

// --- C ABI ---------------------------------------------------------------

/// Ensure the buffer holds `min_capacity`; returns the payload pointer.
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
    buffer_ensure(resolve_arena(ctx), unsafe { &mut *buf }, min_capacity, hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arena() -> Arena {
        Arena::new()
    }

    #[test]
    fn ensure_allocates_rounded_and_empty_stays_empty() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();
        let mut b = Buffer::new();

        let p = buffer_ensure(&mut a, &mut b, 20, 0);
        assert!(!p.is_null());
        assert_eq!(b.len, 0);
        assert!(b.capacity >= 20);
        assert_eq!(b.capacity % 8, 0);
    }

    #[test]
    fn top_of_bump_extends_in_place_without_copy() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();
        let mut b = Buffer::new();

        buffer_append(&mut a, &mut b, b"hello");
        let before = b.data;
        // Nobody allocated after us: growth must not move the payload.
        buffer_ensure(&mut a, &mut b, 4096, 0);
        assert_eq!(b.data, before, "top-of-bump growth must extend in place");
        assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 5) }, b"hello");
    }

    #[test]
    fn displaced_payload_moves_and_preserves_content() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();
        let mut b = Buffer::new();

        buffer_append(&mut a, &mut b, b"hello world");
        let _intruder = a.alloc(8); // now we are not the bump top
        let before = b.data;
        let over = b.capacity + 1;
        buffer_ensure(&mut a, &mut b, over, 0);
        assert_ne!(b.data, before, "must have moved");
        assert_eq!(
            unsafe { std::slice::from_raw_parts(b.data, b.len) },
            b"hello world"
        );
    }

    #[test]
    fn plenty_gives_slack_tight_and_critical_do_not() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();

        set_pressure_mode(PressureMode::Plenty);
        let mut b = Buffer::new();
        buffer_ensure(&mut a, &mut b, 100, 0);
        let over = b.capacity + 1;
        buffer_ensure(&mut a, &mut b, over, 0);
        assert!(b.capacity >= 200, "plenty must double, got {}", b.capacity);

        set_pressure_mode(PressureMode::Critical);
        let mut c = Buffer::new();
        buffer_ensure(&mut a, &mut c, 100, 4096);
        assert_eq!(c.capacity, 104, "critical ignores hint, exact-size only");

        set_pressure_mode(PressureMode::Tight);
        let mut d = Buffer::new();
        buffer_ensure(&mut a, &mut d, 100, 4096);
        assert_eq!(d.capacity, 4096, "tight honors the hint, no doubling");

        set_pressure_mode(PressureMode::Plenty);
    }

    #[test]
    fn payload_larger_than_a_block_goes_os_direct_and_survives_until_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();
        let mut b = Buffer::new();

        buffer_append(&mut a, &mut b, b"start");
        let big = BLOCK_PAYLOAD * 3;
        buffer_ensure(&mut a, &mut b, big, 0);
        assert!(b.capacity >= big);

        // The whole payload is writable and the prefix survived the move.
        unsafe {
            std::ptr::write_bytes(b.data.add(b.len), 0xEE, b.capacity - b.len);
        }
        assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 5) }, b"start");

        a.reset(|_| {}); // frees the tracked OS-direct payload; no crash
    }

    #[test]
    fn growth_in_large_payload_copies_between_runs() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();
        let mut b = Buffer::new();

        buffer_ensure(&mut a, &mut b, BLOCK_PAYLOAD * 2, 0);
        unsafe { std::ptr::write_bytes(b.data, 0xAB, BLOCK_PAYLOAD * 2) };
        b.len = BLOCK_PAYLOAD * 2;

        buffer_ensure(&mut a, &mut b, BLOCK_PAYLOAD * 5, 0);
        assert_eq!(unsafe { *b.data }, 0xAB);
        assert_eq!(unsafe { *b.data.add(BLOCK_PAYLOAD * 2 - 1) }, 0xAB);

        a.reset(|_| {});
    }

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
}
