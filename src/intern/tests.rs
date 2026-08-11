use super::*;
use crate::refcount::{COW, ENTITY_KIND_MASK, EntityKind};

/// One pointer per content, and the entity behind it is an ordinary
/// immortal copy-on-write string rather than a shape of its own.
mod what_an_interned_name_is {
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
        assert_eq!(
            s.rc.flags & ENTITY_KIND_MASK,
            EntityKind::String.to_flags(),
            "an interned name is a string entity, not an object"
        );
        assert_eq!(s.hash, hash_bytes(b"hello"), "hash precomputed");
        assert_ne!(s.hash, 0);
    }
}

/// The table is process-wide, so two threads interning the same name
/// have to end on one entity rather than two.
mod under_concurrency {
    use super::*;

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
