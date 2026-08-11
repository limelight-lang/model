//! Interned names: compile-time-known strings (class, method, property
//! names) as immortal string entities.
//!
//! One string = one address for the process lifetime
//! (`rfc/model/classes.md` "Interned Names"): equality is pointer
//! compare, the hash is computed once and stored inline. The entity is a
//! [`LLString`] like any other — same layout, same accessors, built
//! through the same `init_at` — so an interned name *is* a valid
//! immortal string and the string machinery reads it as-is
//! (`dev/ARCHITECTURE.md`, invariant 13). Immortal + COW: retain/release
//! are no-ops and a write always separates (`rfc/model/values.md`).
//!
//! **The hash is eager here**, unlike a heap string's. An interned name
//! is shared by every thread, and the lazy path writes the field on
//! first read; hashing while the immortal entity is still private to the
//! interning thread is what keeps that write off a shared entity.
//!
//! The lookup table is runtime-internal metadata (Rust-owned), not
//! managed memory; the entities themselves live in the immortal region.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::hash::hash_bytes;
use crate::memory::immortal::immortal_alloc;
use crate::refcount::MemoryCategory;
use crate::string::{LLString, fits, init_at};

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
    if !fits(bytes.len()) {
        return std::ptr::null();
    }

    let mut guard = INTERN.lock().unwrap();
    let table = guard.get_or_insert_with(|| InternTable(HashMap::new()));

    if let Some(&p) = table.0.get(bytes) {
        return p as *const LLString;
    }

    let total = size_of::<LLString>() + bytes.len();
    let mem = immortal_alloc(total);
    if mem.is_null() {
        // Nothing was published: the table is unchanged, so a later call
        // with the same content still interns it correctly.
        return std::ptr::null();
    }

    let s = unsafe { init_at(mem, MemoryCategory::Immortal, bytes, hash_bytes(bytes)) };

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
    use crate::refcount::{COW, ENTITY_KIND_MASK, EntityKind};

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
        assert_eq!(
            s.rc.flags & ENTITY_KIND_MASK,
            EntityKind::String.to_flags(),
            "an interned name is a string entity, not an object"
        );
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
