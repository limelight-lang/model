//! Common refcounted header — offset 0 of every heap entity.
//!
//! Layout and flag bits per `rfc/model/classes.md`; retain/release fast
//! path per `rfc/model/lowering.md`. Phase 1: one thread per request, no
//! atomics (as in Zend).

/// Mask of the memory-category *field* — flags bits 0-1.
pub const MEMORY_CATEGORY_MASK: u32 = 0b11;

/// Memory category: a 2-bit field value, **not** independent bit flags.
/// The four variants are codes of one field — they must never be OR-ed
/// together (that is why this is an enum and not constants). Extract
/// with [`MemoryCategory::from_flags`], compare for equality.
///
/// Non-zero category => not lifetime-counted (except COW entities,
/// which always count — see `rfc/model/values.md`).
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemoryCategory {
    GcHeap = 0b00,
    RequestArena = 0b01,
    LongLived = 0b10,
    Immortal = 0b11,
}

impl MemoryCategory {
    /// Extract the category field from a flags word.
    #[inline]
    pub fn from_flags(flags: u32) -> Self {
        // Safety: masked to 2 bits; all four values are variants.
        unsafe { core::mem::transmute(flags & MEMORY_CATEGORY_MASK) }
    }
}

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
    pub fn new(category: MemoryCategory, extra_flags: u32) -> Self {
        debug_assert_eq!(extra_flags & MEMORY_CATEGORY_MASK, 0);
        RcHeader {
            refcount: 1,
            flags: category as u32 | extra_flags,
        }
    }

    #[inline]
    pub fn memory_category(&self) -> MemoryCategory {
        MemoryCategory::from_flags(self.flags)
    }

    /// Is this entity refcounted for *lifetime* purposes?
    #[inline]
    pub fn lifetime_counted(&self) -> bool {
        self.memory_category() == MemoryCategory::GcHeap
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

    if header.memory_category() == MemoryCategory::Immortal {
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

    if header.memory_category() == MemoryCategory::Immortal {
        return false;
    }

    debug_assert!(header.refcount > 0, "release of dead entity");
    header.refcount -= 1;

    if header.refcount == 0 {
        // Lifetime reaction depends on category: GC heap frees, arenas
        // do nothing (arena reset reclaims). Cycle-root buffering for
        // non-zero decrements arrives with the cycle collector.
        return header.memory_category() == MemoryCategory::GcHeap;
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
        let mut header = RcHeader::new(MemoryCategory::GcHeap, 0);
        retain(&mut header);
        assert_eq!(header.refcount, 2);
        assert!(!release(&mut header));
        assert!(release(&mut header), "second release must report death");
    }

    #[test]
    fn arena_object_is_not_counted() {
        let mut header = RcHeader::new(MemoryCategory::RequestArena, 0);
        retain(&mut header);
        assert_eq!(header.refcount, 1, "arena objects skip counting");
        assert!(!release(&mut header));
        assert_eq!(header.refcount, 1);
    }

    #[test]
    fn immortal_is_never_touched() {
        let mut header = RcHeader::new(MemoryCategory::Immortal, COW);
        retain(&mut header);
        assert!(!release(&mut header));
        assert_eq!(header.refcount, 1);
    }

    #[test]
    fn cow_in_arena_still_counts() {
        // rfc/model/values.md: refcount is part of COW value semantics,
        // maintained in every category; zero in an arena is not a death.
        let mut header = RcHeader::new(MemoryCategory::RequestArena, COW);
        retain(&mut header);
        assert_eq!(header.refcount, 2, "COW entities count everywhere");
        assert!(!release(&mut header));
        assert!(
            !release(&mut header),
            "zero in arena: no free, reset reclaims"
        );
        assert_eq!(header.refcount, 0);
    }

    #[test]
    fn cow_on_heap_dies_at_zero() {
        let mut header = RcHeader::new(MemoryCategory::GcHeap, COW);
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
