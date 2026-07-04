//! Mutable buffers — a low-level growable-memory primitive.
//!
//! A `Buffer` is **not** a heap entity: it has no `RcHeader`, no class,
//! no lifecycle. It is just three words — `{ data, len, capacity }` —
//! describing a growable payload allocated in an arena. Higher-level
//! types that *are* refcounted entities (a mutable string) embed a
//! `Buffer` and put their own `RcHeader` in front of it.
//!
//! ```text
//! Buffer (3 words, caller owns — stack or embedded):   payload (arena):
//! { data, len, capacity }  ─────────────────────────→  [bytes.......]
//! ```
//!
//! Growth collides with the non-moving invariant, but a `Buffer` is not
//! an entity anyone references by address — only its `data` payload
//! moves, and the owner updates it. When the payload sits at the arena's
//! bump top (the `$s .= "x"` builder loop with nothing else allocating),
//! growth extends in place with **zero copies** — an arena-only trick.

use crate::memory::arena::{Arena, round_up_8};

/// Low-level growable region. `#[repr(C)]` so a higher-level entity can
/// embed it at a known offset. No `RcHeader` — this is a mechanism, not
/// an object.
#[repr(C)]
pub struct Buffer {
    pub data: *mut u8,
    pub len: usize,
    pub capacity: usize,
}

impl Buffer {
    /// Create a buffer with an initial payload allocated in `arena`.
    /// Returned by value — the caller owns the three words and stores
    /// them wherever it likes (stack, or embedded in a larger entity).
    pub fn new_in(arena: &mut Arena, capacity: usize) -> Buffer {
        let cap = round_up_8(capacity.max(8));
        let data = arena.alloc(cap);
        Buffer {
            data,
            len: 0,
            capacity: cap,
        }
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
        let mut buf = Buffer::new_in(&mut arena, 4);

        buf.push_bytes(&mut arena, b"hello");
        assert_eq!(buf.as_slice(), b"hello");
        assert_eq!(buf.len, 5);
    }

    #[test]
    fn sufficient_capacity_is_a_noop() {
        let mut arena = Arena::new();
        let mut buf = Buffer::new_in(&mut arena, 64);
        let data0 = buf.data;

        let data1 = buf.ensure(&mut arena, 32);
        assert_eq!(data0, data1, "already big enough — no work");
        assert_eq!(buf.capacity, 64);
    }

    #[test]
    fn grows_in_place_at_bump_top() {
        let mut arena = Arena::new();
        let mut buf = Buffer::new_in(&mut arena, 8);
        let data0 = buf.data;

        // Nothing allocated after the payload: extend in place, same ptr.
        let data1 = buf.ensure(&mut arena, 64);
        assert_eq!(data0, data1, "payload at bump top must extend in place");
        assert!(buf.capacity >= 64);
    }

    #[test]
    fn copies_when_not_at_bump_top() {
        let mut arena = Arena::new();
        let mut buf = Buffer::new_in(&mut arena, 8);
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
        let mut buf = Buffer::new_in(&mut arena, 2);
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
    fn is_three_words_no_header() {
        // A buffer is a mechanism, not an entity: exactly data/len/cap.
        assert_eq!(size_of::<Buffer>(), 3 * size_of::<usize>());
        assert_eq!(core::mem::offset_of!(Buffer, data), 0);
        assert_eq!(core::mem::offset_of!(Buffer, len), 8);
        assert_eq!(core::mem::offset_of!(Buffer, capacity), 16);
    }
}
