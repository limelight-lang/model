//! Mutable buffers — a first-class allocation kind.
//!
//! A growable region for in-place-mutable strings and byte buffers
//! (`docs/memory-manager.md`, Mutable Buffers). Growth collides with the
//! non-moving invariant (entities never change address), so a buffer is
//! two parts:
//!
//! ```text
//! handle (address is eternal):            payload (may be replaced):
//! RcHeader | len | capacity | data  ───→  [bytes.................]
//! ```
//!
//! References hold the handle; only the payload moves. When the payload
//! happens to sit at the arena's bump top (the classic `$s .= "x"`
//! builder loop with nothing else allocating), growth extends in place
//! with **zero copies** — an arena-only trick.

use crate::memory::arena::{Arena, round_up_8};
use crate::refcount::{MemoryCategory, RcHeader};

/// The eternal handle. `#[repr(C)]`, `RcHeader` at offset 0 like every
/// heap entity. Buffers are mutable, so they are **not** COW.
#[repr(C)]
pub struct Buffer {
    pub rc: RcHeader,
    pub len: usize,
    pub capacity: usize,
    pub data: *mut u8,
}

impl Buffer {
    /// Allocate a handle plus an initial payload in `arena`. The handle
    /// is allocated first, then the payload, so the payload starts at
    /// the bump top — the first growth can extend in place.
    pub fn new_in(arena: &mut Arena, capacity: usize) -> *mut Buffer {
        let cap = round_up_8(capacity.max(8));

        let handle = arena.alloc(size_of::<Buffer>()) as *mut Buffer;
        let data = arena.alloc(cap);

        unsafe {
            handle.write(Buffer {
                // Phase 1: buffers live in the request arena.
                rc: RcHeader::new(MemoryCategory::RequestArena, 0),
                len: 0,
                capacity: cap,
                data,
            });
        }
        handle
    }

    /// Ensure at least `min_capacity` bytes, returning the (possibly
    /// new) data pointer. Grows by amortized doubling.
    pub fn ensure(&mut self, arena: &mut Arena, min_capacity: usize) -> *mut u8 {
        if self.capacity >= min_capacity {
            return self.data;
        }

        let new_cap = round_up_8(min_capacity.max(self.capacity * 2));

        // Fast path: the payload is still the last thing allocated, so
        // just move the bump — no copy, the data pointer is unchanged.
        if arena.try_extend_in_place(self.data, self.capacity, new_cap) {
            self.capacity = new_cap;
            return self.data;
        }

        // Slow path: something allocated after us. New payload, copy.
        let new_data = arena.alloc(new_cap);
        unsafe {
            std::ptr::copy_nonoverlapping(self.data, new_data, self.len);
        }
        self.data = new_data;
        self.capacity = new_cap;
        self.data
    }

    /// Append bytes, growing as needed.
    pub fn push_bytes(&mut self, arena: &mut Arena, bytes: &[u8]) {
        let needed = self.len + bytes.len();
        if needed > self.capacity {
            self.ensure(arena, needed);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.data.add(self.len), bytes.len());
        }
        self.len += bytes.len();
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.data, self.len) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_push_read_back() {
        let mut arena = Arena::new();
        let buf = unsafe { &mut *Buffer::new_in(&mut arena, 4) };

        buf.push_bytes(&mut arena, b"hello");
        assert_eq!(buf.as_slice(), b"hello");
        assert_eq!(buf.len, 5);
    }

    #[test]
    fn sufficient_capacity_is_a_noop() {
        let mut arena = Arena::new();
        let buf = unsafe { &mut *Buffer::new_in(&mut arena, 64) };
        let data0 = buf.data;

        let data1 = buf.ensure(&mut arena, 32);
        assert_eq!(data0, data1, "already big enough — no work");
        assert_eq!(buf.capacity, 64);
    }

    #[test]
    fn grows_in_place_at_bump_top() {
        let mut arena = Arena::new();
        let buf = unsafe { &mut *Buffer::new_in(&mut arena, 8) };
        let data0 = buf.data;

        // Nothing allocated after the payload: extend in place, same ptr.
        let data1 = buf.ensure(&mut arena, 64);
        assert_eq!(data0, data1, "payload at bump top must extend in place");
        assert!(buf.capacity >= 64);
    }

    #[test]
    fn copies_when_not_at_bump_top() {
        let mut arena = Arena::new();
        let buf = unsafe { &mut *Buffer::new_in(&mut arena, 8) };
        buf.push_bytes(&mut arena, b"12345678");
        let data0 = buf.data;

        // Someone else allocates — payload is no longer the bump top.
        let _other = arena.alloc(16);

        let data1 = buf.ensure(&mut arena, 64);
        assert_ne!(data0, data1, "must relocate the payload");
        assert_eq!(buf.as_slice(), b"12345678", "content preserved on copy");
    }

    #[test]
    fn builder_loop_grows_in_place_zero_copies() {
        // The `$s = ""; for (...) $s .= "ab";` pattern with nothing else
        // allocating: the payload stays at the bump top the whole time,
        // so it never moves — O(1) amortized append with no copies.
        let mut arena = Arena::new();
        let buf = unsafe { &mut *Buffer::new_in(&mut arena, 2) };
        let initial_data = buf.data;

        for _ in 0..1000 {
            buf.push_bytes(&mut arena, b"ab");
        }

        assert_eq!(buf.data, initial_data, "in-place growth, zero copies");
        assert_eq!(buf.len, 2000);
        assert_eq!(buf.as_slice().len(), 2000);
        assert!(buf.as_slice().iter().all(|&b| b == b'a' || b == b'b'));
    }

    #[test]
    fn header_layout_matches_abi() {
        // RcHeader at offset 0, then len, capacity, data.
        assert_eq!(core::mem::offset_of!(Buffer, rc), 0);
        assert_eq!(core::mem::offset_of!(Buffer, len), 8);
        assert_eq!(core::mem::offset_of!(Buffer, capacity), 16);
        assert_eq!(core::mem::offset_of!(Buffer, data), 24);
    }
}
