//! Common refcounted header — offset 0 of every heap entity.
//!
//! Layout and flag bits per `rfc/model/classes.md`; retain/release fast
//! path per `rfc/model/lowering.md`. One thread per request, and no
//! atomic read-modify-write anywhere. The header accesses are
//! **relaxed atomics** all the same — the same instructions on x86-64
//! and AArch64 — because a collector thread reads published headers
//! while the owner mutates them, and without the annotation that race
//! is undefined behaviour.
//!
//! That reader does not exist until S38 builds it (`PLAN.md`): the
//! annotation is kept across the gap rather than taken out and put
//! back. `rc-cycle` collects in-line on the owning thread and adds the
//! collector thread as an accelerator over the same headers
//! (`rfc/model/gc/rc-cycle.md`, "Decision summary" and "Concurrency").
//!
//! **Flags bits 15 and 16-31 are unclaimed.** The region above 15 is the
//! collector's own, laid out as epoch at 16-17, maturation age at 18-19
//! and reserve at 20-23. Until the step that lays each one lands,
//! nothing reads or writes them, and
//! `refcount::tests::the_header_the_compiler_shares` is what keeps a
//! constant from drifting in meanwhile.

use crate::journal::kinds::journal_event;

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
    /// **Out of use: stamp it on nothing new.** As the category of an
    /// entity this code has no mechanism behind it. `ll_retain` and
    /// `ll_release` return early on it, so it is not counted, and the
    /// same early return keeps it out of every candidate set: `rc-cycle`
    /// registers candidates on the release path
    /// (`rfc/model/gc/rc-cycle.md`), which this category never reaches. No
    /// reset or teardown pass frees it — `rfc/model/memory/arenas.md`
    /// still records the reclamation strategy as undecided, and no
    /// long-lived arena exists in this crate. What it does instead is take
    /// its memory from the same entity blocks as `GcHeap`, so an entity
    /// marked here is an immortal entity housed in the collected heap: it
    /// lives to process exit like `Immortal` while occupying a slot every
    /// trace over that block must step past, paying that visit forever and
    /// buying nothing for it.
    ///
    /// **As `owner_cat` it works, and that use stays.** The question there
    /// is how long the slot receiving a store lives, and a static block or
    /// a global has no owning entity to answer it (`static_block.rs`;
    /// `rfc/model/memory/arena-promotion.md` on a slot that is long-lived
    /// by construction). That is a comparison the store barrier makes, not
    /// an allocation.
    ///
    /// Renaming the code after an owner was considered and deferred: the
    /// name has to wait for the mechanism, or it promises what nothing
    /// delivers (`dev/DECISIONS.md`, "`LongLived` goes out of use, and its
    /// rename waits for a mechanism").
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

/// Copy-on-write semantics: refcount is always maintained,
/// writes with refcount > 1 must separate (`rfc/model/values.md`).
pub const COW: u32 = 1 << 6;

/// Transient mark set on an arena entity while arena reset traces its
/// escaped subgraph (`rfc/model/memory/arena-reset.md`). Cleared when a
/// survivor is promoted to the heap.
///
/// It shares no bit with the collector's fields although a reset and a
/// collection are both traces, because the two never run against the same
/// entity: an arena entity is never a candidate
/// (`rfc/model/classes.md`, "Flags layout").
pub const ARENA_RESET_MARK: u32 = 1 << 7;

/// This instance's class is proven unable to hold a reference to any
/// kind that can close a ring, so no ring passes through it and it never
/// becomes a candidate (`rfc/model/gc/rc-cycle.md`). Stamped by the
/// factory from the class's own answer; **no producer yet**, which is
/// S37.2's, and it waits on `rfc` declaring a target per pointer slot.
pub const ACYCLIC_GATE: u32 = 1 << 8;

/// This entity's owner is proven, so no trace need consider it
/// (`rfc/model/gc/rc-cycle.md`). **No producer yet** — the compiler's
/// stamp and the factory-side write are S37.3's.
pub const OWNERSHIP_MARK: u32 = 1 << 9;

/// A root-queue entry names this entity. Set by the release path before
/// it writes the entry (`crate::cycle::queue`), and cleared by the owner
/// at death and at no other point — **never when a trace finds it
/// externally referenced**, because candidate registration is
/// edge-triggered and clearing it there is a permanent miss (S34.2,
/// `rfc/model/gc/rc-cycle.md`). The one other clearing is the undo of a
/// registration whose entry was never written, which is the same owner
/// reducing the same incomplete state.
pub const CANDIDATE_BIT: u32 = 1 << 10;

/// Entity has weak references (side table exists).
pub const HAS_WEAK_REFERENCES: u32 = 1 << 12;
/// This instance owes a `__destruct`: set only when the user constructor
/// has returned successfully, and only for a class that has a destructor.
/// What every teardown path dispatches on (`rfc/runtime/object-lifecycle.md`).
pub const DESTRUCTOR_PENDING: u32 = 1 << 13;
/// `__destruct` has already run (exactly-once guard),
/// `rfc/runtime/object-lifecycle.md`.
pub const DESTRUCTOR_RAN: u32 = 1 << 14;

/// The entity is torn down and its memory has gone back nowhere: it died
/// inside a trace window whose withheld-return chain had no room for a
/// record, so the death was written here instead (`PLAN.md` S43.2, S43.3).
/// What finds it is the window's own list: the marker links each block it
/// marks through one word of that block's header, and the close walks the
/// list and returns every marked slot
/// (`crate::cycle::deferred_slot_reuse`). Whoever returns the memory is
/// what clears the bit — its block's owner for a slot, and the thread whose
/// trace holds the rows for the other two, which have no owner
/// (`dev/DECISIONS.md`, "the stamp is the whole condition where the return
/// is not the owner's").
///
/// **Three headers carry it**, and what each one holds back differs: a
/// size-class slot, which is on no free list and below its block's bump
/// cursor; a retained survivor, whose block still counts it as a live
/// occupant and so cannot go home; and the one entity of a large block,
/// pooled or OS-direct, whose block or mapping waits with it.
///
/// **The refcount stays zero under it**, which is what a queue reader
/// depends on (`rfc/model/gc/rc-cycle.md`, "Zero-count entities pending
/// slot reuse"), so the bit is the only thing that separates such a header
/// from an unoccupied one — an unoccupied header's first eight bytes are a
/// zero count and whatever flags its last occupant left.
///
/// Three writes keep it from going stale: commissioning zeroes the headers
/// of the memory it cuts, whether that is every slot of a size-class block
/// or the one entity of a large one, [`publish_header`] replaces all eight
/// bytes of a new occupant's header, and the return clears it. **A mark
/// never outlives the window that took it**, which is what keeps a thread
/// exit and an adoption out of this list: the close returns every mark, and
/// a thread cannot exit inside a window
/// (`crate::cycle::deferred_slot_reuse::dispose_thread_state`). Abandonment
/// and adoption assert the ordering rather than answer it.
pub const DEAD_IN_PLACE: u32 = 1 << 15;

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

/// The copy-on-write barrier's test, in the order `rfc/model/values.md`
/// fixes it and for the reasons it gives:
///
/// 1. **Not COW at all → write in place.** The compiler only emits the
///    barrier for entities it typed as COW, and a dynamic string carries
///    `COW = 0` precisely to be outside this rule
///    (`rfc/model/strings.md`) — so the flag is tested first rather than
///    third, and a non-COW entity arriving here writes in place as its
///    layout demands.
/// 2. **Immortal or long-lived → separate.** Category before count, and
///    for a different reason in each half. **Immortal**: `ll_retain` and
///    `ll_release` return early on it, so its count sits at 1 forever,
///    and reading that 1 as "sole owner" would overwrite an interned
///    string shared by the whole process. **Long-lived**: its count *is*
///    maintained (a COW entity takes neither early return above), so
///    `values.md`'s "the count is pinned" does not describe it — the
///    reason is instead that the count is non-atomic while the entity is
///    reachable from more than one request context, which makes it no
///    kind of sharing signal, and that `string_die` frees only `GcHeap`,
///    so an in-place write would land in something nothing can reclaim.
/// 3. **Count above one → separate**, otherwise the holder is alone and
///    writes in place.
///
/// `values.md` prints an `IS_ESCAPEE` arm between 2 and 3 which this does
/// not have: a COW entity cannot carry that bit, the store barrier copying
/// a COW value out of the arena instead of counting an escape into it
/// (`dev/DECISIONS.md`, "a COW value is copied out of the arena, and the
/// store barrier can say no").
///
/// Takes the header word's two halves rather than a pointer, so a caller
/// that already has the flags in a register spends nothing.
#[inline]
pub fn cow_separation_needed(flags: u32, refcount: u32) -> bool {
    if flags & COW == 0 {
        return false;
    }

    debug_assert!(
        flags & IS_ESCAPEE == 0,
        "a COW entity is copied out of the arena, so it never holds an escape count"
    );
    match MemoryCategory::from_flags(flags) {
        MemoryCategory::Immortal | MemoryCategory::LongLived => true,
        MemoryCategory::GcHeap | MemoryCategory::RequestArena => refcount > 1,
    }
}

/// Where a separated COW copy lands — decided by **the holder's**
/// category, not the original's and not the writing context's.
///
/// The rule of every COW kind rather than of any one of them. It lived in
/// `string.rs` while the string was the only COW entity that could be
/// written; every reason it gives was checked against the array and holds
/// there word for word.
///
/// The copy is a fresh entity nothing has registered anywhere, so the
/// only thing that can go wrong is a holder outliving it. An arena
/// holder can hold an arena copy, and not because the holder dies at the
/// reset — it may escape and be promoted. It is safe because the reset's
/// survivor trace reaches the copy through the holder's slot and promotes
/// it too, the same way it reaches any other arena child; every
/// other holder needs something that outlives the request, so the copy
/// goes to the GC heap. That also keeps a copy out of the two categories
/// that cannot own a written value at all: immortal is shared
/// process-wide, and teardown frees only `GcHeap`, so a long-lived
/// copy could never be reclaimed.
///
/// The original's category does not enter into it. An arena holder
/// writing to an interned string gets an arena copy — a bump and a reset,
/// rather than a heap allocation plus a release-at-reset record for a
/// value that dies at the reset regardless.
#[inline]
pub(crate) fn separation_category(owner_cat: MemoryCategory) -> MemoryCategory {
    match owner_cat {
        MemoryCategory::RequestArena => MemoryCategory::RequestArena,
        _ => MemoryCategory::GcHeap,
    }
}

/// Entity kind (bits 2-5): what makes a bare heap pointer
/// self-describing for freeing and for a `mixed` conversion. `0` object is
/// the zero default, so an entity built with no kind bits is an object;
/// strings, arrays and the other kinds set theirs explicitly. Authoritative
/// table: `rfc/model/classes.md`, "Flags layout".
///
/// Four bits rather than three, and adjacent to the category rather than
/// above the destructor state, because the order is what turns three
/// questions and the candidate gate into mask tests: the codes are
/// assigned so that each answer is a range, and a range is a comparison
/// only while the field's high bits carry it
/// ([`kind_may_close_a_cycle`], [`carries_a_class_word`], [`is_string`]).
pub const ENTITY_KIND_SHIFT: u32 = 2;
pub const ENTITY_KIND_MASK: u32 = 0b1111 << ENTITY_KIND_SHIFT;

/// The kind field's top bit. Zero exactly for the codes below eight,
/// which is the range held for kinds that can close a ring
/// ([`EntityKind::closes_a_ring`]).
const KIND_ABOVE_THE_RING_RESERVE: u32 = 0b1 << (ENTITY_KIND_SHIFT + 3);

/// The kind field's top three bits. Equal within the code pairs `{0, 1}`
/// and `{8, 9}`, which is what lets one comparison name a pair: zero for
/// the two kinds carrying a class word at `+8`, and
/// `EntityKind::String.to_flags()` for the two string layouts.
const KIND_TOP_THREE: u32 = 0b111 << (ENTITY_KIND_SHIFT + 1);

/// The eight entity kinds. A value context `Box` and the FFI wrapper
/// `Box` share the [`EntityKind::Box`] tag, distinguished by context
/// (`rfc/model/values.md`, `rfc/model/memory/ffi.md`).
///
/// **The codes are a range assignment, not an enumeration order.** Which
/// code a kind holds decides which questions about it are one mask test,
/// so a code is chosen by the ranges above and never for convenience:
/// `0-7` are held for the kinds that close a ring, `0-1` carry a class
/// word at `+8`, `8-9` are the two string layouts. Four codes of the ring
/// reserve stand free — `4-7` — and so do `12-15` for a kind that closes
/// no ring; the free half of the reserve is what
/// [`EntityKind::closes_a_ring`] exists to defend.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntityKind {
    Object = 0,
    Lazy = 1,
    Array = 2,
    Reference = 3,
    String = 8,
    /// The string whose bytes lie outside the body, behind `data`,
    /// rather than inline after the fixed fields. **Outside the body
    /// whatever put them there** — a compiler proof of single ownership
    /// or a size past the category's slot limit — so this code does not
    /// mean "growable": the second sort keeps [`COW`]
    /// (`rfc/model/strings.md`, "Two Layouts Behind `StringInterface`").
    StringDynamic = 9,
    Box = 10,
    WeakRef = 11,
}

impl EntityKind {
    /// The kind bits for construction, positioned at [`ENTITY_KIND_SHIFT`].
    #[inline]
    pub const fn to_flags(self) -> u32 {
        // Every entity's flags word passes here at birth, which makes it
        // the one site that can catch a kind classified on one side of
        // the reserve and coded on the other. The `const` battery below
        // catches the same thing earlier for every kind it names; this
        // catches a kind the battery was not extended to.
        debug_assert!(
            self.closes_a_ring() == ((self as u32) < 8),
            "a kind's ring classification and its code disagree, so the \
             candidate gate answers the opposite of the classification"
        );
        (self as u32) << ENTITY_KIND_SHIFT
    }

    /// Whether this kind holds counted slots a ring can close through: an
    /// object's properties, a Lazy proxy's object slots, an array's
    /// elements and string keys, and a ReferenceBox's one Value.
    /// `ll_entity_die` sends `Lazy` through `ll_object_die` and
    /// `cells::trace_cells` strides it like an object, which is why it
    /// answers yes before any factory stamps it (`dev/DECISIONS.md`, "a
    /// kind's ring classification is written at its declaration, before a
    /// factory stamps it").
    /// A string, an FFI Box and a weak cell own nothing a ring can pass
    /// through.
    ///
    /// **This is the classification; [`kind_may_close_a_cycle`] is the
    /// test the release path runs**, and the two agree by the assertion
    /// below rather than by anyone's care. The match takes no `_` arm on
    /// purpose: a kind added to the enum stops the build here, in the
    /// file that owns the answer, rather than being refused registration
    /// forever by a mask that never heard of it
    /// (`rfc/model/gc/cycle/questions.md`, Y6).
    #[inline]
    pub const fn closes_a_ring(self) -> bool {
        match self {
            EntityKind::Object | EntityKind::Lazy | EntityKind::Array | EntityKind::Reference => {
                true
            }
            EntityKind::String
            | EntityKind::StringDynamic
            | EntityKind::Box
            | EntityKind::WeakRef => false,
        }
    }
}

// The classification and the code agree, in both directions: a
// ring-closing kind coded at eight or above would be refused registration by
// the mask, and an inert kind coded below eight would be registered and
// traced for children it does not have.
const _: () = {
    let kinds = [
        EntityKind::Object,
        EntityKind::Lazy,
        EntityKind::Array,
        EntityKind::Reference,
        EntityKind::String,
        EntityKind::StringDynamic,
        EntityKind::Box,
        EntityKind::WeakRef,
    ];
    let mut i = 0;
    while i < kinds.len() {
        assert!(
            kinds[i].closes_a_ring() == ((kinds[i] as u32) < 8),
            "a kind's ring classification and its code disagree"
        );
        i += 1;
    }
};

/// True when the kind in this flags word closes a ring — the kinds whose
/// non-zero decrement can leave a garbage ring behind. Takes the flags
/// word rather than the entity, because every caller holds it in a
/// register already.
///
/// **No production caller, and kept as one of the three questions the kind
/// codes were assigned to answer in a single mask test** — the other two
/// are [`carries_a_class_word`] and [`is_string`], and that assignment is
/// the whole argument for a four-bit field ([`ENTITY_KIND_SHIFT`]). The
/// release path reads this same bit inside [`CANDIDATE_GATE_MASK`], which
/// answers the kind clause and four others in one test; here the clause
/// stands alone, and `refcount::tests::the_header_the_compiler_shares` is
/// what holds it to [`EntityKind::closes_a_ring`].
#[inline]
pub fn kind_may_close_a_cycle(flags: u32) -> bool {
    flags & KIND_ABOVE_THE_RING_RESERVE == 0
}

/// The five conditions a non-zero decrement must satisfy before the
/// entity is registered as a cycle candidate, as one mask: **each of them
/// is "this bit is zero"**, which is what the flags layout was chosen
/// for (`rfc/model/classes.md`, "Flags layout").
///
/// - the category is `GcHeap` — an arena entity outlives no reset in a
///   queue, and the zero-count rule would read the count of the slot's next
///   occupant (`rfc/model/gc/rc-cycle.md`, "Zero-count entities pending
///   slot reuse");
/// - the kind is below eight, so a ring can close through it;
/// - the class is not proven acyclic;
/// - the owner is not proven;
/// - a queue entry does not already name it.
///
/// Composed from the named constants rather than written as the literal
/// the design names, so that moving a bit moves the gate with it; the
/// assertion below is what ties the composition back to that literal.
pub const CANDIDATE_GATE_MASK: u32 = MEMORY_CATEGORY_MASK
    | KIND_ABOVE_THE_RING_RESERVE
    | ACYCLIC_GATE
    | OWNERSHIP_MARK
    | CANDIDATE_BIT;

const _: () = assert!(
    CANDIDATE_GATE_MASK == 0x723,
    "the composed gate and the value `rfc/model/classes.md` names have parted"
);

/// True when this flags word passes every clause of
/// [`CANDIDATE_GATE_MASK`] at once, which is the entity a non-final
/// decrement may register as a candidate.
#[inline]
pub fn may_become_a_candidate(flags: u32) -> bool {
    flags & CANDIDATE_GATE_MASK == 0
}

/// True when the entity carries a class word at `+8` — an object or a
/// Lazy proxy, the two kinds a class descriptor and therefore a
/// specialized `dispose` can belong to.
#[inline]
pub fn carries_a_class_word(flags: u32) -> bool {
    flags & KIND_TOP_THREE == 0
}

/// True for a string entity in either layout, the bytes inline after the
/// fixed fields or out of line behind `data`.
///
/// One mask rather than two comparisons because the two codes differ in
/// the kind field's low bit alone, which is what their assignment was
/// chosen for.
///
/// **No production caller, and kept on the same footing as
/// [`kind_may_close_a_cycle`]**: it is the second of the three questions
/// that assignment answers. The string paths ask the narrower
/// `string::bytes_are_out_of_line` instead, and every other site reaches
/// the kind through a `match` on the whole field, so the pair-wide
/// question has no site of its own today;
/// `refcount::tests::the_header_the_compiler_shares` is what holds it to
/// the two codes.
#[inline]
pub fn is_string(flags: u32) -> bool {
    flags & KIND_TOP_THREE == EntityKind::String.to_flags()
}

/// True when the entity kind field is `Object` (the zero default). A
/// flags-word predicate rather than a header one because a caller holding
/// a raw `*mut RcHeader` has the flags in a register already.
///
/// **No production caller**, on the same footing as
/// [`kind_may_close_a_cycle`] and [`is_string`]: teardown and promotion
/// dispatch on the whole kind field through a `match` with an arm per
/// kind (`object::ll_entity_die`, `promote::external_memory`), which
/// needs no separate test for the zero code.
#[inline]
pub fn is_object(flags: u32) -> bool {
    flags & ENTITY_KIND_MASK == 0
}

#[cfg(not(target_endian = "little"))]
compile_error!(
    "the header is one 8-byte word with the refcount in the low half, so \
     the flags half sitting at byte offsets 4-7 assumes a little-endian \
     target"
);

/// The 8-byte header at offset 0 of every heap entity.
///
/// Aligned to 8: the factory publishes it as one 8-byte relaxed-atomic
/// store, which needs the address 8-aligned; the narrow accesses that
/// follow it need 4 and 2. Every real entity already was (the smallest
/// heap slot is 16 bytes); the attribute makes stack-built headers in
/// tests honest too.
#[repr(C, align(8))]
pub struct RcHeader {
    refcount: u32,
    flags: u32,
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
}

/// Publish a fully-built entity's header as **one 8-byte store** — never
/// refcount and flags separately: a torn pair would expose garbage kind
/// bits behind a live count. Until this store the slot reads refcount 0
/// — block commissioning zeroed it, or the previous occupant's death
/// left it — so a trace crossing the block classifies the slot as free
/// rather than reading a half-built entity. The store is a relaxed
/// atomic because that trace may run on a collector thread
/// (`rfc/model/gc/rc-cycle.md`, "Concurrency"), and without the
/// annotation the race is undefined behaviour.
///
/// **This is the one eight-byte access to a header the crate makes**,
/// and it is legal precisely because the entity is not published yet: no
/// collector can be writing byte 6 of a slot it reads as free, so the
/// wide store overlaps nothing. Every access after this point is narrow
/// — four bytes for the counter, two for the mutator's half of the
/// flags — because a wide one would be a mixed-size atomic access
/// against the collector's byte stores.
///
/// # Safety
/// `slot` must be 8-aligned, writable, and not yet published as a live
/// entity (the body must already be fully formed).
#[inline]
pub(crate) unsafe fn publish_header(slot: *mut RcHeader, header: RcHeader) {
    debug_assert_eq!(slot as usize % 8, 0);
    // Kept out of the record's arguments because the store below consumes
    // the header, and behind the same feature as the site that reads it:
    // without `debug-journal` there is no site and no copy
    // (`dev/design/debug-modes.md` §9.6).
    #[cfg(feature = "debug-journal")]
    let born_with = header.flags;
    let word = unsafe { core::mem::transmute::<RcHeader, u64>(header) };

    unsafe {
        (*(slot as *const core::sync::atomic::AtomicU64))
            .store(word, core::sync::atomic::Ordering::Relaxed)
    };

    // After the publication, never before it: a reader resolving the
    // record's subject must find a live entity there, and until the store
    // above the slot reads refcount 0.
    journal_event!(
        crate::journal::kinds::KIND_ENTITY_BIRTH,
        slot as u64,
        ((born_with & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT) as u64,
        (born_with & MEMORY_CATEGORY_MASK) as u64
    );
}

/// Increment the reference count.
///
/// Fast path per `rfc/model/lowering.md`: one branch on the category
/// bits covers arenas and immortals. COW entities always count
/// (`rfc/model/values.md`) — their category is checked only on release.
///
/// The header is read in two narrow relaxed loads — the flags half for
/// the category tests, then the counter — and only the 4-byte counter
/// half is stored back. That is the narrow-mutator rule: no flags store,
/// nothing beyond the counter itself. Why narrow beats wide on both
/// sides is [`refcount_load`]'s argument, measured in
/// `dev/BENCHMARKS.md`, 2026-07-27.
///
/// # Safety
/// `header` must point to a live heap entity beginning with `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_retain(header: *mut RcHeader) {
    {
        let flags = unsafe { flags_load(header) };

        if flags & MEMORY_CATEGORY_MASK != 0 && flags & COW == 0 {
            return; // arena or immortal, not COW: not counted
        }

        if MemoryCategory::from_flags(flags) == MemoryCategory::Immortal {
            return; // immortal COW entities are no-ops too
        }

        let refcount = unsafe { refcount_load(header) };
        // With `checked-refcount`, saturate rather than wrap. Wrapping to
        // zero would make the next release think the entity died and free
        // it while it is still referenced. Saturating leaks it instead,
        // which is the safe direction. See the feature's note in
        // `Cargo.toml` for why this is optional and not a default.
        #[cfg(feature = "checked-refcount")]
        if refcount == u32::MAX {
            return;
        }

        // Narrow loads, narrow counter store, no flags store — so this
        // path cannot bury a concurrent stamp in the collector's byte.
        unsafe { refcount_store(header, refcount + 1) };
    }
}

/// Store only the 4-byte refcount half, relaxed — the narrow-mutator
/// store (`dev/BENCHMARKS.md`, 2026-07-27). Must stay an aligned atomic
/// store: the collector reads the containing word concurrently.
#[inline]
unsafe fn refcount_store(header: *mut RcHeader, value: u32) {
    unsafe {
        (*(header as *const core::sync::atomic::AtomicU32))
            .store(value, core::sync::atomic::Ordering::Relaxed)
    };
}

/// The narrow 4-byte loads matching [`refcount_store`]. The hot paths
/// must not mix a narrow store with the 8-byte word load: the next
/// operation's wide load over a fresh narrow store defeats
/// store-to-load forwarding — measured at ~3x on the retain/release
/// pair (`dev/BENCHMARKS.md`, "the narrow mutator lands: retain/release
/// reach parity with rc-trace (and past it)"). Narrow stores demand
/// narrow loads.
#[inline]
unsafe fn refcount_load(header: *const RcHeader) -> u32 {
    unsafe {
        (*(header as *const core::sync::atomic::AtomicU32))
            .load(core::sync::atomic::Ordering::Relaxed)
    }
}

/// The mutator's half of the flags word — **bits 0-15, bytes 4-5** —
/// zero-extended, as a relaxed 16-bit atomic load.
///
/// Two bytes rather than four because the collector's fields start at
/// bit 16 and it writes byte 6 on its own. A 32-bit load at +4 and a
/// 1-byte store at +6 overlap without covering each other, which is a
/// mixed-size atomic access: Rust's memory model does not define it and
/// Miri rejects it outright, so the width is the contract rather than an
/// economy (`rfc/model/classes.md`, "Flags layout").
///
/// **Every constant a mutator path reads lives below bit 16**, which is
/// what makes the narrow read lossless; the assertion below is what
/// holds that true as constants are added.
#[inline]
unsafe fn flags_load(header: *const RcHeader) -> u32 {
    unsafe {
        (*((header as *const u8).add(4) as *const core::sync::atomic::AtomicU16))
            .load(core::sync::atomic::Ordering::Relaxed) as u32
    }
}

/// The store twin of [`flags_load`], and the only way a published
/// header's flags are written after publication.
#[inline]
unsafe fn flags_store(header: *mut RcHeader, flags: u32) {
    debug_assert_eq!(
        flags & 0xFFFF_0000,
        0,
        "the mutator writes flags bits 0-15; bit 16 and above are the \
         collector's and it writes them a byte at a time"
    );
    unsafe {
        (*((header as *mut u8).add(4) as *const core::sync::atomic::AtomicU16))
            .store(flags as u16, core::sync::atomic::Ordering::Relaxed)
    }
}

const _: () = assert!(
    (MEMORY_CATEGORY_MASK
        | ENTITY_KIND_MASK
        | COW
        | ARENA_RESET_MARK
        | ACYCLIC_GATE
        | OWNERSHIP_MARK
        | CANDIDATE_BIT
        | IS_ESCAPEE
        | HAS_WEAK_REFERENCES
        | DESTRUCTOR_PENDING
        | DESTRUCTOR_RAN
        | DEAD_IN_PLACE)
        & 0xFFFF_0000
        == 0,
    "a mutator-visible flag above bit 15 would read back as zero, because \
     the mutator loads two bytes of the flags half and not four"
);

/// Decrement the reference count. Returns `true` when the entity died
/// (count reached zero and it is lifetime-managed by counting) — the
/// caller must then run teardown: `ll_entity_die` for a bare pointer
/// (the kind switch), or `ll_object_die` directly where the caller
/// statically knows an object.
///
/// **Every death takes the ordinary path**: teardown runs at the point
/// the count reaches zero, and no collector is consulted to decide it.
/// Compiler-emitted runs of releases use [`ll_release_batch`] bracketed
/// by one [`ll_gc_checkpoint_ack`](crate::gc::ll_gc_checkpoint_ack)
/// before the run and one
/// [`ll_gc_checkpoint`](crate::gc::ll_gc_checkpoint) after it
/// (`rfc/model/memory/bulk-operations.md`).
///
/// # Safety
/// `header` must point to a live heap entity beginning with `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_release(entity: *mut RcHeader) -> bool {
    // A non-zero decrement is where `rc-cycle` registers a candidate
    // (`rfc/model/gc/rc-cycle.md`); the queue that receives it is S34,
    // so nothing is registered yet and a garbage ring is retained until
    // it lands. The epoch handshake this branch used to acknowledge died
    // with the collector that owned it.
    unsafe { release_word(entity) }
}

/// The decrement itself: the shared core of [`ll_release`] and
/// [`ll_release_batch`]. Returns the ABI verdict — the caller must run
/// teardown. There is no unreachable test and no deferral, so the death
/// branch is the same narrow counter store as every other release.
#[inline]
unsafe fn release_word(entity: *mut RcHeader) -> bool {
    let flags = unsafe { flags_load(entity) };

    if flags & MEMORY_CATEGORY_MASK != 0 && flags & COW == 0 {
        return false;
    }

    if MemoryCategory::from_flags(flags) == MemoryCategory::Immortal {
        return false;
    }

    let refcount = unsafe { refcount_load(entity) };
    debug_assert!(refcount > 0, "release of dead entity");
    let refcount = refcount - 1;
    // Narrow-mutator store: counter half only, flags never touched.
    unsafe { refcount_store(entity, refcount) };

    // A decrement that does not reach zero is the one event that can
    // leave a garbage ring behind, so it is where a candidate is
    // registered (`rfc/model/gc/rc-cycle.md`).
    if refcount != 0 && may_become_a_candidate(flags) {
        note_admission();

        // The bit goes down **before** the queue write, which Y12
        // clause 4 requires: set after it, a second decrement landing in
        // the window between the two registers the same entity twice. The
        // word written is the one loaded above, so this store carries
        // every other mutator flag forward unchanged — which is sound
        // for the same reason the counter store above is, the entity's
        // own thread being its only writer.
        unsafe { flags_store(entity, flags | CANDIDATE_BIT) };

        // No undo, because there is nothing to undo: the registration
        // cannot fail. Every allocation path refusing writes the entry to
        // the queue's overflow buffer, and the report is the next safepoint
        // poll's, from a frame that has one (`rfc/dev/DECISIONS.md`,
        // "an enrolment cannot fail"). A bit set here therefore always names
        // an entry.
        unsafe { crate::cycle::queue::register_candidate(entity) };
    }

    refcount == 0 && MemoryCategory::from_flags(flags) == MemoryCategory::GcHeap
}

#[cfg(test)]
thread_local! {
    /// How many decrements this thread's release path admitted through
    /// [`CANDIDATE_GATE_MASK`]. Per thread rather than global because the
    /// harness runs tests in parallel and a shared counter would charge
    /// one test's releases to another; `Cell<usize>` has no drop glue,
    /// which is the rule for anything a thread exit can reach
    /// (`memory::heap::ll_thread_exit`).
    static ADMITTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Record one admission. A scenario test observes a pair of conditions
/// at once — it sees an entity registered or not — and can never show that
/// a clause it did not vary rejects on its own. The counter is what sees
/// a condition that never fires.
#[inline]
fn note_admission() {
    #[cfg(test)]
    ADMITTED.with(|c| c.set(c.get() + 1));
}

/// Admissions on this thread since [`take_admissions`] last ran, and
/// zero the count.
#[cfg(test)]
pub(crate) fn take_admissions() -> usize {
    ADMITTED.with(|c| c.replace(0))
}

/// [`ll_release`] for a compiler-emitted run of releases (a scope
/// exit): lowering emits one
/// [`ll_gc_checkpoint_ack`](crate::gc::ll_gc_checkpoint_ack) before the
/// run, releases each reference with this variant, then one full
/// [`ll_gc_checkpoint`](crate::gc::ll_gc_checkpoint) after it, so the
/// run pays the safepoint once and pays it where its transients are
/// back at their true counts (`rfc/model/memory/bulk-operations.md`).
/// The two functions are the same today and the variant is kept because
/// the emitted bracket names it.
///
/// # Safety
/// As [`ll_release`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_release_batch(entity: *mut RcHeader) -> bool {
    return unsafe { release_word(entity) };
}

/// **The mutator's half** of a published header's flags — bits 0-15,
/// zero-extended — as a relaxed atomic. The one read helper: teardown
/// paths and the weak machinery share it rather than owning private
/// copies.
///
/// **It does not answer for bits 16 and above**, and the name says
/// mutator for that reason. Those are the collector's, written a byte at
/// a time, and a caller asking this what epoch an entity carries gets
/// zero in every build with nothing red. The byte reader they need is
/// S36.6's and S37.1's, and it is not written yet — **change this, and
/// give those steps their helper.**
#[inline]
pub(crate) unsafe fn mutator_flags(header: *const RcHeader) -> u32 {
    unsafe { flags_load(header) }
}

/// Read the refcount of a **published** header, same dispatch rule and
/// the same width rule — the counter twin of [`mutator_flags`].
#[inline]
pub(crate) unsafe fn header_refcount(header: *const RcHeader) -> u32 {
    unsafe { refcount_load(header) }
}

/// Write the refcount of a **published** header — the store twin of
/// [`header_refcount`], and narrow for the same reason: the flags half
/// is neither read nor written, so a byte the collector puts there
/// cannot be buried by this store.
///
/// A count changed by a delta reads with [`header_refcount`] and writes
/// here. There is no read-modify-write helper, because the two halves
/// are separate relaxed accesses either way — which is what [`ll_retain`]
/// spells out inline.
#[inline]
pub(crate) unsafe fn set_header_refcount(header: *mut RcHeader, value: u32) {
    unsafe { refcount_store(header, value) };
}

/// The count and the mutator's half of the flags together, for a caller
/// that wants both — [`cow_separation_needed`] is the predicate over the
/// pair.
///
/// **Two narrow loads rather than one wide one**, which is a change of
/// width and not of economy: an 8-byte load at +0 overlaps the
/// collector's byte store at +6 without covering it, and a mixed-size
/// atomic access is undefined in Rust's model whatever it costs. The wide
/// form was measured cheaper where nothing narrow precedes it
/// (`dev/BENCHMARKS.md`, "the barrier's header reads go narrow"), and what
/// that measurement priced is no longer on offer. What the second load
/// costs beside the first is unmeasured.
#[inline]
pub(crate) unsafe fn header_pair(header: *const RcHeader) -> (u32, u32) {
    unsafe { (refcount_load(header), flags_load(header)) }
}

/// What a walker of a slot's first eight bytes finds there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SlotState {
    /// An entity is in the slot and its count is above zero.
    Live,
    /// The occupant died inside a trace window that could not record the
    /// return, and the slot is neither the allocator's nor an entity's
    /// until that window's close returns it ([`DEAD_IN_PLACE`]).
    DeadInPlace,
    /// The allocator may hand the slot out: either it is on the block's
    /// free list or it stands above the block's bump cursor.
    Free,
}

/// Which of the three states `header` is in.
///
/// **The one definition of the occupancy test**, and every walker that
/// reads a slot's first word goes through it: `heap::for_each_entity_slot`
/// and the census over it, `heap::describe_slot`, and
/// `retained::is_occupied`, which `register` counts a retained block's
/// live occupants through. A count above zero answers without the
/// second load, so a live slot pays what it paid before the third state
/// existed.
///
/// **Change this, change `cycle::deferred_slot_reuse::dispose_marks_of` too:**
/// this call stands inside a walk that a panic can resume, and a panic site
/// added here would be repeated by the resumed walk inside an unwind, where a
/// second panic aborts. The walk names the calls it needs kept quiet.
///
/// # Safety
/// `header` addresses a slot of a commissioned entity block, readable at
/// its first eight bytes.
#[inline]
pub(crate) unsafe fn slot_state(header: *const RcHeader) -> SlotState {
    if unsafe { refcount_load(header) } != 0 {
        return SlotState::Live;
    }

    if unsafe { flags_load(header) } & DEAD_IN_PLACE != 0 {
        return SlotState::DeadInPlace;
    }

    SlotState::Free
}

/// Write the mark of [`SlotState::DeadInPlace`] into the header of an
/// entity that has been torn down.
///
/// The caller owns the memory at this instant: the teardown has finished, no
/// queue entry names it, and it has gone back to no free list, no pool and no
/// mapping, so nothing else reads or writes these bytes until the return
/// clears the mark.
///
/// # Safety
/// `header` addresses a dead entity whose count reads zero, in memory a trace
/// has stamped, and the window that marks it returns it at its close —
/// through the block's place on that window's list, or through the window's
/// stack of foreign slots
/// (`crate::cycle::deferred_slot_reuse`, `classify_past_the_region`).
#[inline]
pub(crate) unsafe fn mark_dead_in_place(header: *mut RcHeader) {
    debug_assert_eq!(
        unsafe { refcount_load(header) },
        0,
        "a slot is marked dead only after its teardown has left the count at zero"
    );
    unsafe { update_header_flags(header, |flags| flags | DEAD_IN_PLACE) };
}

/// Take the mark off, which the thread whose window took it does as it makes
/// the return the mark deferred — the block's owner where the window listed
/// the block, and the marking thread itself where it stacked the slot in a
/// block another thread owns, or where the return is a retained survivor's or
/// a large entity's and has no owner at all (`dev/DECISIONS.md`, "a death the
/// collection never met is returned at once, and a foreign slot is stacked").
///
/// **One thread writes this half of the flags word of one dead slot**, and it
/// is the thread whose window marked it. The word is written by load and
/// store rather than by a read-modify-write, so a second writer loses one of
/// the two; what keeps the marking thread alone with the slot is that the
/// slot reached no free list, so the block's owner has no reason to touch it,
/// and that at most one window is open in the process (`PLAN.md`, S38's
/// token). It never runs on a collector worker, which
/// `rfc/model/gc/rc-cycle.md`, "Zero-count entities pending slot reuse"
/// refuses the neighbouring clear of the candidate bit for the same
/// reason.
///
/// # Safety
/// As [`mark_dead_in_place`].
#[inline]
pub(crate) unsafe fn clear_dead_in_place(header: *mut RcHeader) {
    unsafe { update_header_flags(header, |flags| flags & !DEAD_IN_PLACE) };
}

/// Rewrite the flags of a **published** header — the write twin of
/// [`mutator_flags`]. A post-publish flag write must not be a plain
/// store: the header may be under a concurrent trace.
///
/// **`f` sees and returns bits 0-15 only**, which [`flags_store`]
/// enforces: a bit it sets above 15 is a caller error, not a write. What
/// the collector holds up there is not disturbed, because the load and
/// the store are both two bytes wide, so whatever it writes to byte 6
/// between them survives. That is the whole content of the narrow rule —
/// a wide store here writes back a flags half read before the
/// collector's store, and buries it.
#[inline]
pub(crate) unsafe fn update_header_flags(header: *mut RcHeader, f: impl FnOnce(u32) -> u32) {
    let flags = unsafe { flags_load(header) };
    unsafe { flags_store(header, f(flags)) };
}

/// Whether a queue entry names this entity — the bit the release path
/// set before it wrote the entry ([`CANDIDATE_BIT`]).
///
/// The free path asks it of every dying entity: a slot a queue entry
/// names is withheld from the allocator instead of returned, because the
/// entry is a raw pointer and whoever retires it reads the count out of
/// the body (`rfc/model/gc/rc-cycle.md`, "Zero-count entities pending slot
/// reuse").
#[inline]
pub(crate) unsafe fn is_registered_candidate(header: *const RcHeader) -> bool {
    unsafe { mutator_flags(header) & CANDIDATE_BIT != 0 }
}

/// Take the candidate bit down, which only the retirement of the entry
/// that set it may do.
///
/// **Never when a trace finds the entity externally referenced**: candidate
/// registration is edge-triggered, so an entity whose bit is cleared while it
/// is still alive is one no later decrement can register again, and the ring it
/// closes is a permanent miss (`rfc/model/gc/cycle/questions.md`, Y6). The two
/// lawful clearings are the zero-count member retirement in
/// [`crate::cycle::queue::release_queue_segments`] and the free a collection's
/// commit performs, which is `PLAN.md` S36.6's.
#[inline]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the retirement that clears it is `PLAN.md` S39.1's, and the commit's free S36.6's"
    )
)]
pub(crate) unsafe fn clear_candidate_bit(header: *mut RcHeader) {
    unsafe { update_header_flags(header, |flags| flags & !CANDIDATE_BIT) };
}

/// The teardown guard's `+1`, as a narrow counter store: the flags half
/// is not read and not written, so nothing the collector puts there can
/// be buried by it.
#[inline]
pub(crate) unsafe fn mutator_guard_retain(header: *mut RcHeader) {
    let refcount = unsafe { refcount_load(header) };
    unsafe { refcount_store(header, refcount + 1) };
}

/// The teardown guard's `-1`, the counter twin of
/// [`mutator_guard_retain`]: returns the new refcount. A collection
/// landing mid-destructor changes nothing here — teardown always
/// finishes, and the zero-count rule drops a component holding a member
/// already at zero (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and
/// reclamation", step 1).
#[inline]
pub(crate) unsafe fn mutator_unguard_release(header: *mut RcHeader) -> u32 {
    let refcount = unsafe { refcount_load(header) } - 1;
    unsafe { refcount_store(header, refcount) };
    refcount
}

/// The refcount of the entity at `entity`, for a fixture holding a typed
/// pointer — `*mut Object`, `*mut LLArray` — rather than a header one.
/// `RcHeader` is at offset 0 of every entity, which is what makes the cast
/// sound.
///
/// **A raw pointer and a narrow load, both by ruling.** A shorthand shaped
/// `fn refcount(&self)` would form the `&RcHeader` the field privacy exists
/// to ban, and one whose body was a plain read would leave the fixtures in
/// the population a ThreadSanitizer run reaches first — they are the sites
/// that touch published entities (`dev/DECISIONS.md`, "`RcHeader`'s fields
/// go private, and the source grep is re-aimed rather than retired").
///
/// **`*mut` because the load needs write provenance.** An atomic access
/// retags SharedReadWrite, so a pointer that grants only SharedReadOnly
/// stops it under Miri and nowhere else (`dev/POSTMORTEM.md`, "an atomic
/// read needs write provenance"). `*mut` is what refuses the two spellings
/// that carry the weaker one: `&raw const local`, and a `&T` binding, which
/// coerces to `*const T` at a call site with no cast to notice. A cast to
/// `*mut` still compiles, so this narrows the accident rather than closing
/// the hole.
///
/// # Safety
/// `entity` points at a live entity, whose first field is its header.
#[cfg(test)]
#[inline]
pub(crate) unsafe fn entity_refcount<T>(entity: *mut T) -> u32 {
    unsafe { refcount_load(entity as *const RcHeader) }
}

/// The mutator's half of the flags of the entity at `entity` — the twin of
/// [`entity_refcount`], and [`mutator_flags`]'s answer, so bits 16 and above
/// read zero.
///
/// # Safety
/// As [`entity_refcount`].
#[cfg(test)]
#[inline]
pub(crate) unsafe fn entity_flags<T>(entity: *mut T) -> u32 {
    unsafe { flags_load(entity as *const RcHeader) }
}

/// The memory category of the entity at `entity`, read through
/// [`entity_flags`].
///
/// # Safety
/// As [`entity_refcount`].
#[cfg(test)]
#[inline]
pub(crate) unsafe fn entity_category<T>(entity: *mut T) -> MemoryCategory {
    MemoryCategory::from_flags(unsafe { entity_flags(entity) })
}

#[cfg(test)]
mod tests;
