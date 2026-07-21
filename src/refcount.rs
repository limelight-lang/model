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
/// Transient mark used during arena reset: the entity is part of the
/// escaped subgraph (`rfc/model/memory/arena-reset.md`). Cleared when
/// the survivor's category is rewritten.
pub const ESCAPED: u32 = 1 << 11;
/// The entity is an [`crate::object::Object`] (has a class pointer at
/// +8). Teardown paths that only have a bare `RcHeader` dispatch on
/// this. Strings/arrays will claim sibling bits; flags-table extension
/// to be reflected in rfc/model/classes.md.
pub const ENTITY_OBJECT: u32 = 1 << 12;

/// The entity is a live **escapee**: a request-arena object that one or
/// more longer-lived containers currently reference
/// (`rfc/model/memory/arenas.md`, "The dangerous direction"). While set,
/// `refcount` holds the **escape hold-count** (how many such containers
/// point at it) instead of a lifetime count — arena objects are not
/// lifetime-counted, so the field is free. Maintained incrementally by the
/// store barrier and by holder teardown; consumed at arena reset to decide
/// promotion. Cleared when the count returns to zero or the survivor's
/// category is rewritten at promotion.
pub const IS_ESCAPEE: u32 = 1 << 13;

/// Where the entity sits in the cycle collector's candidate buffer,
/// stored as `index + 1` so that zero means "position unknown" (bits
/// 14-31, the top of the flags word). Zend keeps the same thing in
/// `gc_info` for the same reason: without it, forgetting a candidate
/// means a linear scan of the whole buffer. Zero is always safe — the
/// collector falls back to that scan (`crate::gc::forget_candidate`).
pub const CANDIDATE_INDEX_SHIFT: u32 = 14;
pub const CANDIDATE_INDEX_MASK: u32 = 0x0003_FFFF << CANDIDATE_INDEX_SHIFT;
/// Largest buffer position the field can hold. Beyond it the index is
/// stored as zero: 262 142 candidates is 26 full thresholds without a
/// single collection point, and the fallback costs only speed.
pub const CANDIDATE_INDEX_MAX: usize = 0x0003_FFFF - 1;

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

    // With `checked-refcount`, saturate rather than wrap. Wrapping to
    // zero would make the next release think the entity died and free it
    // while it is still referenced. Saturating leaks it instead, which is
    // the safe direction. See the feature's note in `Cargo.toml` for why
    // this is optional and not a default.
    #[cfg(feature = "checked-refcount")]
    if header.refcount == u32::MAX {
        return;
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
pub unsafe extern "C" fn ll_release(entity: *mut RcHeader) -> bool {
    let header = unsafe { &mut *entity };

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
        // do nothing (arena reset reclaims).
        return header.memory_category() == MemoryCategory::GcHeap;
    }

    // Non-zero decrement on a heap object: a possible cycle root
    // (`ll_buffer_cycle_root` of rfc/model/lowering.md). Only objects
    // buffer — only they carry traceable reference slots. In a NoGC or
    // pure-RC build this call compiles away with the strategy.
    if header.memory_category() == MemoryCategory::GcHeap && header.flags & ENTITY_OBJECT != 0 {
        // `entity`, not `header`: the buffered pointer outlives this call
        // and the collector casts it back to `*mut Object` to read the
        // class word and the property slots. A pointer derived from
        // `&mut RcHeader` carries provenance over the 8-byte header only,
        // so every one of those reads would be out of bounds of the tag
        // it came from (audit `class.rs:115`, same family).
        unsafe { crate::gc::buffer_candidate(entity) };
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

    /// With `checked-refcount`, a count at the ceiling stops moving and
    /// the entity is effectively immortal. Without the guard the `+= 1`
    /// wraps to zero, and the next release frees an entity that is still
    /// referenced — the failure this trades a leak for.
    ///
    /// Only meaningful with the feature on:
    /// `cargo test --features checked-refcount`.
    #[cfg(feature = "checked-refcount")]
    #[test]
    fn a_saturated_refcount_never_wraps_to_zero() {
        let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
        h.refcount = u32::MAX;

        unsafe { ll_retain(&mut h) };
        assert_eq!(h.refcount, u32::MAX, "saturated, not wrapped");

        // And it stays alive: a release from the ceiling must not be able
        // to reach zero in one step either.
        let died = unsafe { ll_release(&mut h) };
        assert!(!died, "an entity at the ceiling does not die of one release");
    }

    #[test]
    fn header_is_8_bytes_at_offset_zero() {
        assert_eq!(size_of::<RcHeader>(), 8);
        assert_eq!(align_of::<RcHeader>(), 4);
        assert_eq!(core::mem::offset_of!(RcHeader, refcount), 0);
        assert_eq!(core::mem::offset_of!(RcHeader, flags), 4);
    }
}
