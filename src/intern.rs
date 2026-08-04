//! Interned names: compile-time-known strings (class, method, property
//! names) as immortal string entities.
//!
//! One string = one address for the process lifetime
//! (`rfc/model/classes.md` "Interned Names"): equality is pointer
//! compare, the hash is computed once and stored inline. The entity
//! uses the string layout from `rfc/model/strings.md` —
//! `RcHeader | len | hash | bytes inline` — so an interned name *is* a
//! valid immortal string, usable by the future string machinery as-is.
//! `len` at +8 and `hash` at +16 are the offsets the dynamic layout will
//! share, which is what lets either be read without first deciding which
//! layout this is; `layout_matches_the_string_design` pins them. `len` is
//! 32 bits — a string is capped at 4 GiB so that the dynamic layout's
//! capacity fits in the padding the hash's alignment creates anyway.
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
    pub len: u32,
    // +12: four bytes of padding before the 8-aligned `hash`. The dynamic
    // layout spends them on its capacity; an inline string leaves them.
    pub hash: u64,
    // bytes follow inline
}

impl LLString {
    /// The inline string bytes.
    ///
    /// Takes a raw pointer rather than `&self` for the same reason as
    /// [`crate::class::Class::vtbl`]: the bytes live past
    /// `size_of::<LLString>()`, which a `&LLString` does not cover
    /// (audit `class.rs:115`). Interned strings are immortal, hence the
    /// free lifetime.
    ///
    /// # Safety
    /// `s` must point to a live string entity allocated with its bytes.
    #[inline]
    pub unsafe fn bytes<'a>(s: *const LLString) -> &'a [u8] {
        unsafe {
            let base = (s as *const u8).add(size_of::<LLString>());
            std::slice::from_raw_parts(base, (*s).len as usize)
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
/// content. Same content → same pointer, forever. **Null when the
/// immortal region cannot grow**, and null for content past the 4 GiB
/// string cap (`rfc/model/strings.md`); nothing is recorded in either
/// case, so the call can simply be retried.
///
/// The cap cannot be reached by an interned name — they are
/// compile-time-known identifiers — but the check is here rather than
/// assumed away, because the failure it prevents is a length truncated
/// to 32 bits and a subsequent write past the buffer.
pub fn intern(bytes: &[u8]) -> *const LLString {
    if bytes.len() > u32::MAX as usize {
        return std::ptr::null();
    }

    let mut guard = INTERN.lock().unwrap();
    let table = guard.get_or_insert_with(|| InternTable(HashMap::new()));

    if let Some(&p) = table.0.get(bytes) {
        return p as *const LLString;
    }

    let total = size_of::<LLString>() + bytes.len();
    let s = immortal_alloc(total) as *mut LLString;
    if s.is_null() {
        // Nothing was published: the table is unchanged, so a later call
        // with the same content still interns it correctly.
        return std::ptr::null();
    }
    unsafe {
        s.write(LLString {
            rc: RcHeader::new(MemoryCategory::Immortal, COW),
            len: bytes.len() as u32,
            hash: hash_bytes(bytes),
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
        // Keep the raw pointer: `&*` would narrow provenance to the fixed
        // fields, and the bytes live past them.
        let p = intern_str("hello");
        let s = unsafe { &*p };
        assert_eq!(unsafe { LLString::bytes(p) }, b"hello");
        assert_eq!(s.len, 5);
        assert_eq!(s.rc.memory_category(), MemoryCategory::Immortal);
        assert_ne!(s.rc.flags & COW, 0, "immortal strings are COW-flagged");
        assert_eq!(s.hash, hash_bytes(b"hello"), "hash precomputed");
        assert_ne!(s.hash, 0);
    }

    /// The offsets the second layout has to match: a dynamic string
    /// (`rfc/model/strings.md`) puts `len` and `hash` in the same places,
    /// so reading either does not require deciding which layout this is.
    /// Swapping the two fields still compiles and still passes every other
    /// test in this module, which is why the contract is pinned here.
    #[test]
    fn layout_matches_the_string_design() {
        assert_eq!(size_of::<RcHeader>(), 8, "header must stay 8 bytes");
        assert_eq!(std::mem::offset_of!(LLString, rc), 0);
        assert_eq!(std::mem::offset_of!(LLString, len), 8);
        let probe = LLString {
            rc: RcHeader::new(MemoryCategory::Immortal, COW),
            len: 0,
            hash: 0,
        };
        assert_eq!(
            std::mem::size_of_val(&probe.len),
            4,
            "len is 32-bit: the 4 GiB cap"
        );
        assert_eq!(
            std::mem::offset_of!(LLString, hash),
            16,
            "+12 stays free for the dynamic layout's capacity"
        );
        assert_eq!(size_of::<LLString>(), 24, "bytes start right after");
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
