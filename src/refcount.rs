//! Common refcounted header — offset 0 of every heap entity.
//!
//! Layout and flag bits per `rfc/model/classes.md`; retain/release fast
//! path per `rfc/model/lowering.md`. Phase 1: one thread per request, no
//! atomics (as in Zend).

/// Memory category, flags bits 0-1. Non-zero category => not counted
/// (except COW types, which always count — see `rfc/model/values.md`).
pub const MEMORY_CATEGORY_MASK: u32 = 0b11;
pub const MEMORY_CATEGORY_GC_HEAP: u32 = 0b00;
pub const MEMORY_CATEGORY_REQUEST_ARENA: u32 = 0b01;
pub const MEMORY_CATEGORY_LONG_LIVED: u32 = 0b10;
pub const MEMORY_CATEGORY_IMMORTAL: u32 = 0b11;

/// GC state for the CAS handoff (bits 2-3), `rfc/model/gc/heap-design.md`.
pub const GC_STATE_SHIFT: u32 = 2;
pub const GC_STATE_MASK: u32 = 0b11 << GC_STATE_SHIFT;

/// Cycle-collector color (bits 4-5) + buffered bit (6).
pub const CYCLE_COLLECTOR_COLOR_SHIFT: u32 = 4;
pub const CYCLE_COLLECTOR_BUFFERED: u32 = 1 << 6;

/// Entity has weak references (side table exists).
pub const HAS_WEAK_REFERENCES: u32 = 1 << 7;
/// Entity has a destructor with side effects (arena must call it).
pub const HAS_DESTRUCTOR: u32 = 1 << 8;
/// Copy-on-write semantics: refcount is always maintained,
/// writes with refcount > 1 must separate (`rfc/model/values.md`).
pub const COW: u32 = 1 << 9;
/// `__destruct` has already run (exactly-once guard),
/// `rfc/runtime/object-lifecycle.md`.
pub const DESTRUCTED: u32 = 1 << 10;

/// The 8-byte header at offset 0 of every heap entity.
#[repr(C)]
pub struct RcHeader {
    pub refcount: u32,
    pub flags: u32,
}

impl RcHeader {
    /// Initial header: logical refcount 1, given category and flags.
    /// (The off-by-one encoding trick is deferred until the GC lands;
    /// for now the count is stored literally.)
    #[inline]
    pub fn new(category: u32, extra_flags: u32) -> Self {
        debug_assert_eq!(category & !MEMORY_CATEGORY_MASK, 0);
        RcHeader { refcount: 1, flags: category | extra_flags }
    }

    #[inline]
    pub fn memory_category(&self) -> u32 {
        self.flags & MEMORY_CATEGORY_MASK
    }

    /// Is this entity refcounted for *lifetime* purposes?
    #[inline]
    pub fn lifetime_counted(&self) -> bool {
        self.flags & MEMORY_CATEGORY_MASK == MEMORY_CATEGORY_GC_HEAP
    }
}

/// Increment the reference count.
///
/// Fast path per `rfc/model/lowering.md`: one branch on the category
/// bits covers arenas and immortals. COW entities always count
/// (`rfc/model/values.md`) — their category is checked only on release.
///
/// # Safety
/// `header` must point to a live heap entity beginning with `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_retain(header: *mut RcHeader) {
    let header = unsafe { &mut *header };
    if header.flags & MEMORY_CATEGORY_MASK != 0 && header.flags & COW == 0 {
        return; // arena or immortal, not COW: not counted
    }
    if header.flags & MEMORY_CATEGORY_MASK == MEMORY_CATEGORY_IMMORTAL {
        return; // immortal COW entities are no-ops too
    }
    header.refcount += 1;
}

/// Decrement the reference count. Returns `true` when the entity died
/// (count reached zero and it is lifetime-managed by counting) — the
/// caller must then run teardown (`ll_object_die` for objects).
///
/// # Safety
/// `header` must point to a live heap entity beginning with `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_release(header: *mut RcHeader) -> bool {
    let header = unsafe { &mut *header };
    if header.flags & MEMORY_CATEGORY_MASK != 0 && header.flags & COW == 0 {
        return false;
    }
    if header.flags & MEMORY_CATEGORY_MASK == MEMORY_CATEGORY_IMMORTAL {
        return false;
    }
    debug_assert!(header.refcount > 0, "release of dead entity");
    header.refcount -= 1;
    if header.refcount == 0 {
        // Lifetime reaction depends on category: GC heap frees, arenas
        // do nothing (arena reset reclaims). Cycle-root buffering for
        // non-zero decrements arrives with the cycle collector.
        return header.memory_category() == MEMORY_CATEGORY_GC_HEAP;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retain(header: &mut RcHeader) {
        unsafe { ll_retain(header) }
    }
    fn release(header: &mut RcHeader) -> bool {
        unsafe { ll_release(header) }
    }

    #[test]
    fn heap_entity_counts_and_dies() {
        let mut header = RcHeader::new(MEMORY_CATEGORY_GC_HEAP, 0);
        retain(&mut header);
        assert_eq!(header.refcount, 2);
        assert!(!release(&mut header));
        assert!(release(&mut header), "second release must report death");
    }

    #[test]
    fn arena_object_is_not_counted() {
        let mut header = RcHeader::new(MEMORY_CATEGORY_REQUEST_ARENA, 0);
        retain(&mut header);
        assert_eq!(header.refcount, 1, "arena objects skip counting");
        assert!(!release(&mut header));
        assert_eq!(header.refcount, 1);
    }

    #[test]
    fn immortal_is_never_touched() {
        let mut header = RcHeader::new(MEMORY_CATEGORY_IMMORTAL, COW);
        retain(&mut header);
        assert!(!release(&mut header));
        assert_eq!(header.refcount, 1);
    }

    #[test]
    fn cow_in_arena_still_counts() {
        // rfc/model/values.md: refcount is part of COW value semantics,
        // maintained in every category; zero in an arena is not a death.
        let mut header = RcHeader::new(MEMORY_CATEGORY_REQUEST_ARENA, COW);
        retain(&mut header);
        assert_eq!(header.refcount, 2, "COW entities count everywhere");
        assert!(!release(&mut header));
        assert!(!release(&mut header), "zero in arena: no free, reset reclaims");
        assert_eq!(header.refcount, 0);
    }

    #[test]
    fn cow_on_heap_dies_at_zero() {
        let mut header = RcHeader::new(MEMORY_CATEGORY_GC_HEAP, COW);
        assert!(release(&mut header));
    }

    #[test]
    fn header_is_8_bytes_at_offset_zero() {
        assert_eq!(size_of::<RcHeader>(), 8);
        assert_eq!(align_of::<RcHeader>(), 4);
        assert_eq!(core::mem::offset_of!(RcHeader, refcount), 0);
        assert_eq!(core::mem::offset_of!(RcHeader, flags), 4);
    }
}
