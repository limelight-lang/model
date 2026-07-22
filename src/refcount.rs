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
/// Idle for arena-category entities — no strategy ever sees them — so
/// arena reset borrows its low bit as a transient mark, see
/// [`ARENA_RESET_MARK`].
pub const GC_STATE_SHIFT: u32 = 2;
pub const GC_STATE_MASK: u32 = 0b11 << GC_STATE_SHIFT;

/// Transient mark set on an arena entity while arena reset traces its
/// escaped subgraph. Arena entities never run a GC strategy, so the
/// GC-state field is idle for them and reset borrows its low bit here
/// (`rfc/model/classes.md`, "Flags layout"; `rfc/model/memory/arena-reset.md`).
/// Cleared when a survivor is promoted to the heap, where its whole
/// category + GC-state is rewritten. Replaces the old dedicated `ESCAPED`
/// flag bit, freed by the 2026-07-22 flags compaction.
pub const ARENA_RESET_MARK: u32 = 1 << GC_STATE_SHIFT;

/// Cycle-collector color (bits 4-5) + buffered bit (6).
pub const CYCLE_COLLECTOR_COLOR_SHIFT: u32 = 4;
pub const CYCLE_COLLECTOR_BUFFERED: u32 = 1 << 6;

/// Entity has weak references (side table exists).
pub const HAS_WEAK_REFERENCES: u32 = 1 << 7;
/// This instance owes a `__destruct`: set only when the user constructor
/// has returned successfully, and only for a class that has a destructor.
/// What every teardown path dispatches on (`rfc/runtime/object-lifecycle.md`).
/// Was `HAS_DESTRUCTOR` before the 2026-07-22 flags compaction.
pub const DESTRUCTOR_PENDING: u32 = 1 << 8;
/// `__destruct` has already run (exactly-once guard),
/// `rfc/runtime/object-lifecycle.md`. Was `DESTRUCTED`, and now adjacent
/// to [`DESTRUCTOR_PENDING`].
pub const DESTRUCTOR_RAN: u32 = 1 << 9;
/// Copy-on-write semantics: refcount is always maintained,
/// writes with refcount > 1 must separate (`rfc/model/values.md`).
pub const COW: u32 = 1 << 10;

/// The entity is a live **escapee**: a request-arena object that one or
/// more longer-lived containers currently reference
/// (`rfc/model/memory/arenas.md`, "The dangerous direction"). While set,
/// `refcount` holds the **escape hold-count** (how many such containers
/// point at it) instead of a lifetime count — arena objects are not
/// lifetime-counted, so the field is free. Maintained incrementally by the
/// store barrier and by holder teardown; consumed at arena reset to decide
/// promotion. Cleared when the count returns to zero or the survivor's
/// category is rewritten at promotion.
pub const IS_ESCAPEE: u32 = 1 << 11;

/// Entity kind (bits 12-14): what makes a bare heap pointer
/// self-describing for freeing and for a `mixed` conversion. `0` object is
/// the zero default, so an entity built with no kind bits is an object;
/// strings, arrays and the other kinds set theirs explicitly. Authoritative
/// table: `rfc/model/classes.md`, "Flags layout". Replaces the old
/// dedicated `ENTITY_OBJECT` flag bit.
pub const ENTITY_KIND_SHIFT: u32 = 12;
pub const ENTITY_KIND_MASK: u32 = 0b111 << ENTITY_KIND_SHIFT;

/// The seven entity kinds (code `7` is reserved). A value context `Box`
/// and the FFI wrapper `Box` share the [`EntityKind::Box`] tag,
/// distinguished by context (`rfc/model/values.md`, `rfc/model/memory/ffi.md`).
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntityKind {
    Object = 0,
    String = 1,
    Array = 2,
    Reference = 3,
    Box = 4,
    WeakRef = 5,
    Lazy = 6,
}

impl EntityKind {
    /// The kind bits for construction, positioned at [`ENTITY_KIND_SHIFT`].
    #[inline]
    pub const fn to_flags(self) -> u32 {
        (self as u32) << ENTITY_KIND_SHIFT
    }
}

/// True when the entity kind field is `Object` (the zero default). The
/// dispatch every teardown and trace path makes on a bare header; replaces
/// the old dedicated `ENTITY_OBJECT` flag test. Kept as a flags-word
/// predicate because most call sites hold a raw `*mut RcHeader` and have
/// the flags in a register already.
#[inline]
pub fn is_object(flags: u32) -> bool {
    flags & ENTITY_KIND_MASK == 0
}

/// Where the entity sits in the cycle collector's candidate buffer,
/// stored as `index + 1` so that zero means "position unknown" (bits
/// 15-31, the top of the flags word). Zend keeps the same thing in
/// `gc_info` for the same reason: without it, forgetting a candidate
/// means a linear scan of the whole buffer. Zero is always safe — the
/// collector falls back to that scan (`crate::gc::forget_candidate`).
pub const CANDIDATE_INDEX_SHIFT: u32 = 15;
pub const CANDIDATE_INDEX_MASK: u32 = 0x0001_FFFF << CANDIDATE_INDEX_SHIFT;
/// Largest buffer position the field can hold. Beyond it the index is
/// stored as zero: 131 070 candidates is many full thresholds without a
/// single collection point, and the fallback costs only speed.
pub const CANDIDATE_INDEX_MAX: usize = 0x0001_FFFF - 1;

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
    //
    // The "already buffered" test is here rather than only inside
    // `buffer_candidate`, because `flags` is in a register on this line
    // and an object is buffered at most once per collection: without it
    // every later decrement of the same object paid a call and a reload
    // to be told nothing had changed. The callee keeps its own copy of
    // the test — it has other callers, and this one is an optimization,
    // not the invariant.
    // Object kind is the zero kind field, so "an object that is not yet
    // buffered" is exactly "kind bits and buffered bit all clear" — one
    // masked compare, the same single test the old `ENTITY_OBJECT` bit gave.
    if header.memory_category() == MemoryCategory::GcHeap
        && header.flags & (ENTITY_KIND_MASK | CYCLE_COLLECTOR_BUFFERED) == 0
    {
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

    /// The flags word layout is a contract with the compiler and the C
    /// mirror in `rfc/model/lowering.md`: generated code stamps these exact
    /// bit positions. Pin them so the 2026-07-22 compaction cannot drift.
    #[test]
    fn flags_layout_is_the_compacted_design() {
        assert_eq!(MEMORY_CATEGORY_MASK, 0b11, "category: bits 0-1");
        assert_eq!(GC_STATE_MASK, 0b11 << 2, "gc state: bits 2-3");
        assert_eq!(ARENA_RESET_MARK, 1 << 2, "reset mark borrows gc-state bit 2");
        assert_eq!(CYCLE_COLLECTOR_BUFFERED, 1 << 6);
        assert_eq!(HAS_WEAK_REFERENCES, 1 << 7);
        assert_eq!(DESTRUCTOR_PENDING, 1 << 8);
        assert_eq!(DESTRUCTOR_RAN, 1 << 9);
        assert_eq!(COW, 1 << 10);
        assert_eq!(IS_ESCAPEE, 1 << 11);
        assert_eq!(ENTITY_KIND_SHIFT, 12);
        assert_eq!(ENTITY_KIND_MASK, 0b111 << 12, "entity kind: bits 12-14");
        assert_eq!(CANDIDATE_INDEX_SHIFT, 15);
        assert_eq!(CANDIDATE_INDEX_MASK, 0x0001_FFFF << 15, "candidate index: bits 15-31, 17 wide");
        assert_eq!(CANDIDATE_INDEX_MAX, 131_070);

        // The kind field and the candidate index must not overlap, and the
        // whole word must stay 32 bits wide.
        assert_eq!(ENTITY_KIND_MASK & CANDIDATE_INDEX_MASK, 0, "kind and index are disjoint");
        assert_eq!(CANDIDATE_INDEX_MASK >> 15 << 15, CANDIDATE_INDEX_MASK, "index reaches the top bit");
        assert_eq!(0x8000_0000u32 & CANDIDATE_INDEX_MASK, 0x8000_0000, "and includes bit 31");
    }

    /// `Object` is the zero kind field, so a header built with no kind bits
    /// reads as an object — the property the whole `ENTITY_OBJECT`-bit
    /// removal rests on — while every other kind sits inside the field.
    #[test]
    fn object_is_the_zero_kind() {
        assert_eq!(EntityKind::Object.to_flags(), 0);
        assert!(is_object(0));
        assert!(is_object(MemoryCategory::GcHeap as u32 | COW), "non-kind bits do not confuse it");

        for kind in [
            EntityKind::String,
            EntityKind::Array,
            EntityKind::Reference,
            EntityKind::Box,
            EntityKind::WeakRef,
            EntityKind::Lazy,
        ] {
            let bits = kind.to_flags();
            assert_ne!(bits, 0, "{kind:?} is a non-zero kind");
            assert_eq!(bits & !ENTITY_KIND_MASK, 0, "{kind:?} lands inside the kind field");
            assert!(!is_object(bits), "{kind:?} is not an object");
        }
    }
}
