//! Interned names: compile-time-known strings (class, method, property
//! names) as immortal string entities.
//!
//! One string = one address for the process lifetime
//! (`rfc/model/classes.md` "Interned Names"): equality is pointer
//! compare, the hash is computed once and stored inline. The entity
//! uses the string layout from `rfc/model/strings.md` —
//! `RcHeader | hash | len | bytes inline` — so an interned name *is* a
//! valid immortal string, usable by the future string machinery as-is.
//! Immortal + COW: retain/release are no-ops and a write always
//! separates (`rfc/model/values.md`).
//!
//! The lookup table is runtime-internal metadata (Rust-owned), not
//! managed memory; the entities themselves live in the immortal region.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::memory::immortal::immortal_alloc;
use crate::refcount::{COW, MemoryCategory, RcHeader};

/// String entity layout (`rfc/model/strings.md`): bytes inline after
/// the fixed fields, one allocation, no second hop.
#[repr(C)]
pub struct LLString {
    pub rc: RcHeader,
    pub hash: u64,
    pub len: u64,
    // bytes follow inline
}

impl LLString {
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        unsafe {
            let base = (self as *const LLString).add(1) as *const u8;
            std::slice::from_raw_parts(base, self.len as usize)
        }
    }
}

/// FNV-1a; the hash function is an internal detail, stored once.
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Table value is a raw pointer; entries are immortal, so the pointers
/// never dangle.
struct InternTable(HashMap<Vec<u8>, usize>);
unsafe impl Send for InternTable {}

static INTERN: Mutex<Option<InternTable>> = Mutex::new(None);

/// Intern `bytes`: returns the unique immortal string entity for this
/// content. Same content → same pointer, forever.
pub fn intern(bytes: &[u8]) -> *const LLString {
    let mut guard = INTERN.lock().unwrap();
    let table = guard.get_or_insert_with(|| InternTable(HashMap::new()));

    if let Some(&p) = table.0.get(bytes) {
        return p as *const LLString;
    }

    let total = size_of::<LLString>() + bytes.len();
    let s = immortal_alloc(total) as *mut LLString;
    unsafe {
        s.write(LLString {
            rc: RcHeader::new(MemoryCategory::Immortal, COW),
            hash: hash_bytes(bytes),
            len: bytes.len() as u64,
        });
        let dst = s.add(1) as *mut u8;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
    }

    table.0.insert(bytes.to_vec(), s as usize);
    s
}

/// Convenience for static names in runtime code and tests.
pub fn intern_str(s: &str) -> *const LLString {
    intern(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_content_same_pointer() {
        let _g = crate::memory::block_pool::test_guard();
        let a = intern_str("Order");
        let b = intern_str("Order");
        let c = intern_str("Customer");
        assert_eq!(a, b, "equality must be pointer compare");
        assert_ne!(a, c);
    }

    #[test]
    fn entity_is_a_valid_immortal_cow_string() {
        let _g = crate::memory::block_pool::test_guard();
        let s = unsafe { &*intern_str("hello") };
        assert_eq!(s.bytes(), b"hello");
        assert_eq!(s.len, 5);
        assert_eq!(s.rc.memory_category(), MemoryCategory::Immortal);
        assert_ne!(s.rc.flags & COW, 0, "immortal strings are COW-flagged");
        assert_eq!(s.hash, hash_bytes(b"hello"), "hash precomputed");
        assert_ne!(s.hash, 0);
    }

    #[test]
    fn interning_is_thread_safe() {
        let _g = crate::memory::block_pool::test_guard();
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| intern_str("shared-name") as usize))
            .collect();
        let ptrs: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(
            ptrs.windows(2).all(|w| w[0] == w[1]),
            "all threads must agree on one address"
        );
    }
}
