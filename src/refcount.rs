//! Common refcounted header — offset 0 of every heap entity.
//!
//! Layout and flag bits per `rfc/model/classes.md`; retain/release fast
//! path per `rfc/model/lowering.md`. Phase 1: one thread per request, no
//! atomics (as in Zend). Under the `rc-walk` feature the header accesses
//! compile as **relaxed atomics** instead — same instructions on x86-64
//! and AArch64, but the collector thread reads headers concurrently and
//! without the annotation that race is undefined behaviour
//! (`rfc/model/gc/rc-walk.md`, "The two header bytes"). Still no atomic
//! read-modify-write anywhere.

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
    /// `ll_release` return early on it, so it is not counted; the census
    /// enrolls only `GcHeap` (`walk.rs`), so it is not collected; and no
    /// reset or teardown pass frees it — `rfc/model/memory/arenas.md`
    /// still records the reclamation strategy as undecided, and no
    /// long-lived arena exists in this crate. What it does instead is take
    /// its memory from the same entity blocks as `GcHeap`, so an entity
    /// marked here is an immortal entity housed in the collected heap: it
    /// lives to process exit like `Immortal` while occupying a slot the
    /// collector strides over on every walk, paying that visit forever and
    /// buying nothing for it.
    ///
    /// **As `owner_cat` it works, and that use stays.** The question there
    /// is how long the slot receiving a store lives, and a static block or
    /// a global has no owning entity to answer it (`static_block.rs`;
    /// `rfc/model/memory/arena-promotion.md` on a slot that is long-lived
    /// by construction). That is a comparison the store barrier makes, not
    /// an allocation.
    ///
    /// Renaming the code after an owner was considered on 2026-08-06 and
    /// deferred — the name has to wait for the mechanism, or it promises
    /// what nothing delivers (`dev/DECISIONS.md`).
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
/// The `IS_ESCAPEE` arm `values.md` used to print between 2 and 3 is
/// gone (2026-08-04). It existed because bit 11 makes the count field
/// hold an escape hold-count rather than a reference count, and a COW
/// entity can no longer carry that bit at all: the store barrier copies
/// a COW value out of the arena instead of counting an escape into it
/// (`memory/barrier::store_category_barrier`). Testing a bit that cannot
/// be set is a branch on the write path and a claim in the rule that
/// nothing can produce.
///
/// Takes the header word's two halves rather than a pointer so a caller
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

/// The entity kinds that may enter the cycle collector's candidate
/// buffer, as a set indexed by the kind code. **A kind belongs here
/// exactly when it holds counted slots a cycle can close through**: an
/// object's properties, an array's elements and string keys, a
/// ReferenceBox's one Value, and a Lazy proxy's object slots —
/// `ll_entity_die` sends `Lazy` through `ll_object_die` and
/// `walk::trace_cells` strides it like an object. String, Box and WeakRef
/// own nothing a ring can pass through and stay out by that same
/// criterion.
///
/// **A set rather than a mask over the kind bits**, because no mask can
/// express this one: admitting `Reference 011` needs bit 0 clear in the
/// mask, and excluding `String 001` needs it set. A mask admits only kind
/// sets closed under clearing a bit, so the contradiction belongs to
/// masking rather than to this pair of codes. Membership costs a shift
/// and a bit test on a word the release path already holds
/// ([`kind_may_close_a_cycle`]).
///
/// Built from [`EntityKind`] rather than written as a literal, so the
/// consolidation of codes 4-6 that `rfc/model/classes.md` argues for moves
/// the value at compile time and leaves the meaning alone. The entity-kind
/// renumbering that would have bought these four kinds a single masked
/// compare is rejected on its own grounds, and this set does not need it.
///
/// `Lazy` is admitted although no factory stamps that kind yet: the
/// criterion is the slots the kind can hold, and an absent producer is no
/// reason to exclude a kind. A kind left out until its producer arrives
/// leaks every ring that closes through it in the meantime.
///
/// **rc-trace only**, like the buffer itself.
pub const CANDIDATE_KINDS: u32 = (1 << EntityKind::Object as u32)
    | (1 << EntityKind::Array as u32)
    | (1 << EntityKind::Reference as u32)
    | (1 << EntityKind::Lazy as u32);

/// True when the kind in this flags word is in [`CANDIDATE_KINDS`] — the
/// kinds whose non-zero decrement is buffered as a possible cycle root.
/// For the reserved code 7 the answer is false, as for every kind outside
/// the set. Takes the flags word rather than the entity, because every
/// caller holds it in a register already.
#[inline]
pub fn kind_may_close_a_cycle(flags: u32) -> bool {
    (CANDIDATE_KINDS >> ((flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT)) & 1 != 0
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
///
/// **rc-trace only.** In an `rc-walk` build the field is dead — nothing
/// ever feeds the candidate buffer — and its bits belong to the epoch
/// byte below. The two strategies cannot share the top half of the
/// word, which is why selection is a build-time feature
/// (`rfc/model/gc/strategies.md`).
pub const CANDIDATE_INDEX_SHIFT: u32 = 15;
pub const CANDIDATE_INDEX_MASK: u32 = 0x0001_FFFF << CANDIDATE_INDEX_SHIFT;

/// **String entities only:** the bytes live out of line, through the
/// `data` pointer of `string::LLStringDynamic`, rather than inline after
/// the fixed fields. Set once by the factory and never flipped — nothing
/// promotes between the two layouts at run time.
///
/// It shares bit 15 with the candidate index above and the two never
/// meet, which is what lets a kind-scoped bit sit here the way
/// [`ARENA_RESET_MARK`] sits in the GC-state field. [`COW`] carries
/// copy-on-write and nothing else: one bit could not say both for an
/// oversize string, which is out of line by size and copy-on-write by
/// semantics (`dev/DECISIONS.md`, 2026-08-10;
/// `rfc/model/memory/large-entities.md`).
pub const STRING_OUT_OF_LINE: u32 = 1 << 15;
/// Largest buffer position the field can hold. Beyond it the index is
/// stored as zero: 131 070 candidates is many full thresholds without a
/// single collection point, and the fallback costs only speed.
pub const CANDIDATE_INDEX_MAX: usize = 0x0001_FFFF - 1;

/// rc-walk's **epoch byte** — header byte 6, flags bits 16-23
/// (`rfc/model/gc/rc-walk.md`, "The one header byte"). The collector's
/// maturity stamp: 0 on every fresh header (the factory writes the flags
/// word with this byte zero at no extra cost), the current epoch number
/// once the walker has met the entity. Written by the **collector only**,
/// as a plain byte store; the mutator's whole-word header stores may bury
/// a concurrent stamp, which costs one epoch of latency, never a verdict.
///
/// This is the collector's only claim on the header since the
/// eager-death amendment (2026-07-27): the condemned byte (bits 24-31)
/// is retired — condemnation is collector-private, and the mutator's
/// death path never consults the collector at all.
#[cfg(feature = "rc-walk")]
pub const EPOCH_BYTE_SHIFT: u32 = 16;
#[cfg(feature = "rc-walk")]
pub const EPOCH_BYTE_MASK: u32 = 0xFF << EPOCH_BYTE_SHIFT;

#[cfg(all(feature = "rc-walk", not(target_endian = "little")))]
compile_error!(
    "rc-walk handles the header as one 8-byte word with refcount in the \
     low half; the byte offset 6 of the epoch byte assumes a \
     little-endian target"
);

/// The 8-byte header at offset 0 of every heap entity.
///
/// Aligned to 8: the factory publishes it as one 8-byte store, and under
/// `rc-walk` every access compiles as a relaxed atomic on the whole word
/// — both need the address 8-aligned. Every real entity already was (the
/// smallest heap slot is 16 bytes); the attribute makes stack-built
/// headers in tests honest too.
#[repr(C, align(8))]
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

/// Publish a fully-built entity's header as **one 8-byte store** — never
/// refcount and flags separately (`rfc/model/gc/rc-walk.md`, Phase 1: a
/// torn pair would expose garbage kind bits behind a live count). Until
/// this store the slot reads refcount 0 — block commissioning zeroed it,
/// or the previous occupant's death left it — so a walker classifies the
/// slot as free rather than reading a half-built entity. Under `rc-walk`
/// the store is a relaxed atomic: the collector thread reads headers
/// concurrently, and without the annotation the race is undefined
/// behaviour.
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
    #[cfg(not(feature = "rc-walk"))]
    unsafe {
        (slot as *mut u64).write(word)
    };

    #[cfg(feature = "rc-walk")]
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

/// Relaxed-atomic load of the whole header word: refcount in the low
/// half, flags in the high (little-endian, enforced above). Same
/// instruction as a plain load on x86-64 and AArch64; the annotation is
/// what makes the cross-thread race with the collector's byte stores
/// defined (`rfc/model/gc/rc-walk.md`, "The one header byte").
#[cfg(feature = "rc-walk")]
#[inline]
unsafe fn header_word_load(header: *mut RcHeader) -> u64 {
    unsafe {
        (*(header as *const core::sync::atomic::AtomicU64))
            .load(core::sync::atomic::Ordering::Relaxed)
    }
}

/// Relaxed-atomic store of the whole header word; pair of
/// [`header_word_load`].
#[cfg(feature = "rc-walk")]
#[inline]
unsafe fn header_word_store(header: *mut RcHeader, word: u64) {
    unsafe {
        (*(header as *const core::sync::atomic::AtomicU64))
            .store(word, core::sync::atomic::Ordering::Relaxed)
    }
}

/// Collector-side whole-header read: refcount in the low half, flags in
/// the high. Stale by design; Phases 3–4 repair what it misreads.
///
/// # Safety
/// `header` must point into a live entity-block slot (occupied or free —
/// a free slot legitimately reads refcount 0).
#[cfg(feature = "rc-walk")]
#[inline]
pub(crate) unsafe fn collector_load_header(header: *mut RcHeader) -> u64 {
    unsafe { header_word_load(header) }
}

/// The collector's maturity stamp: one plain byte store into header
/// byte 6. A concurrent mutator whole-word store may bury it — the
/// entity then reads "new" one more epoch: latency, never a verdict
/// (`rfc/model/gc/rc-walk.md`, "The two header bytes").
///
/// # Safety
/// `header` must point to an occupied entity-block slot.
#[cfg(feature = "rc-walk")]
#[inline]
pub(crate) unsafe fn collector_stamp_epoch(header: *mut RcHeader, epoch_number: u8) {
    debug_assert_ne!(
        epoch_number, 0,
        "0 means never-stamped; numbers cycle 1-255"
    );
    unsafe {
        (*((header as *mut u8).add(6) as *const core::sync::atomic::AtomicU8))
            .store(epoch_number, core::sync::atomic::Ordering::Relaxed)
    };
}

/// Increment the reference count.
///
/// Fast path per `rfc/model/lowering.md`: one branch on the category
/// bits covers arenas and immortals. COW entities always count
/// (`rfc/model/values.md`) — their category is checked only on release.
///
/// Under `rc-walk` the header is loaded once as a relaxed atomic word
/// (the category tests need the flags anyway) and only the 4-byte
/// counter half is stored back — the narrow-mutator amendment: no flags
/// store, nothing beyond the counter itself
/// (`rfc/model/gc/rc-walk.md`, "What the mutator pays").
///
/// # Safety
/// `header` must point to a live heap entity beginning with `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_retain(header: *mut RcHeader) {
    #[cfg(not(feature = "rc-walk"))]
    {
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

    #[cfg(feature = "rc-walk")]
    {
        let flags = unsafe { flags_load(header) };

        if flags & MEMORY_CATEGORY_MASK != 0 && flags & COW == 0 {
            return; // arena or immortal, not COW: not counted
        }

        if MemoryCategory::from_flags(flags) == MemoryCategory::Immortal {
            return; // immortal COW entities are no-ops too
        }

        let refcount = unsafe { refcount_load(header) };
        // Saturation rationale as in the rc-trace arm above.
        #[cfg(feature = "checked-refcount")]
        if refcount == u32::MAX {
            return;
        }

        // Narrow-mutator amendment (rfc, 2026-07-27): narrow loads,
        // narrow counter store, no flags store — the collector's
        // concurrent epoch stamps can no longer be buried by this
        // path.
        unsafe { refcount_store(header, refcount + 1) };
    }
}

/// Store only the 4-byte refcount half, relaxed — the narrow-mutator
/// store (`rfc/model/gc/rc-walk.md`, "The narrow mutator"). Must stay an
/// aligned atomic store: the collector reads the containing word
/// concurrently.
#[cfg(feature = "rc-walk")]
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
/// pair (`dev/BENCHMARKS.md`, 2026-07-27). Narrow stores demand narrow
/// loads.
#[cfg(feature = "rc-walk")]
#[inline]
unsafe fn refcount_load(header: *const RcHeader) -> u32 {
    unsafe {
        (*(header as *const core::sync::atomic::AtomicU32))
            .load(core::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(feature = "rc-walk")]
#[inline]
unsafe fn flags_load(header: *const RcHeader) -> u32 {
    unsafe {
        (*((header as *const u8).add(4) as *const core::sync::atomic::AtomicU32))
            .load(core::sync::atomic::Ordering::Relaxed)
    }
}

/// Decrement the reference count. Returns `true` when the entity died
/// (count reached zero and it is lifetime-managed by counting) — the
/// caller must then run teardown: `ll_entity_die` for a bare pointer
/// (the kind switch), or `ll_object_die` directly where the caller
/// statically knows an object.
///
/// Under `rc-walk` there is no candidate buffering — candidates are
/// computed by the collector's walk, which is the design's advertised
/// net reduction on this path — and **every death takes the ordinary
/// path** (the eager-death amendment, 2026-07-27,
/// `rfc/model/gc/rc-walk.md`, Phase 4): teardown runs at the natural
/// point, condemned or not, with only the memory parked while an epoch
/// is in flight. The drain protects itself with the corpse rule — a
/// posted component containing an `rc 0` member is dropped whole.
///
/// Under `rc-walk` the death branch also **acks the epoch handshake**
/// ([`crate::epoch::checkpoint_ack`]) — ack only: message pickup rides
/// the outermost dispose's exit, because between this release's zero
/// store and the dispose no user code may observe the entity
/// (review finding, 2026-07-27). Compiler-emitted runs of releases use
/// [`ll_release_batch`] bracketed by one
/// [`ll_gc_checkpoint_ack`](crate::gc::ll_gc_checkpoint_ack) before
/// the run and one [`ll_gc_checkpoint`](crate::gc::ll_gc_checkpoint)
/// after it instead.
///
/// # Safety
/// `header` must point to a live heap entity beginning with `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_release(entity: *mut RcHeader) -> bool {
    #[cfg(not(feature = "rc-walk"))]
    {
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

        // Non-zero decrement on a heap entity: a possible cycle root
        // (`ll_buffer_cycle_root` of rfc/model/lowering.md). The kinds
        // that buffer are those holding counted slots a cycle can close
        // through ([`CANDIDATE_KINDS`]). In a NoGC or pure-RC build this
        // call compiles away with the strategy.
        //
        // The "already buffered" test is here rather than only inside
        // `buffer_candidate`, because `flags` is in a register on this line
        // and an entity is buffered at most once per collection: hoisting
        // it keeps the thread-local access and the `Vec` push out of this
        // function, and `buffer_candidate` is `#[inline(never)]` so they
        // stay out. It saves no call — the callee was inlined before the
        // split (`dev/BENCHMARKS.md`, 2026-07-21). The callee keeps its own
        // copy of the test: it has other callers, and this one is an
        // optimization, not the invariant.
        //
        // The buffered bit and the kind are two tests rather than one
        // masked compare, because the admitted kinds are not a mask
        // ([`CANDIDATE_KINDS`]).
        if header.memory_category() == MemoryCategory::GcHeap
            && header.flags & CYCLE_COLLECTOR_BUFFERED == 0
            && kind_may_close_a_cycle(header.flags)
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

    #[cfg(feature = "rc-walk")]
    {
        let tear = unsafe { release_word(entity) };
        if tear {
            // The death branch acks the epoch handshake (decision
            // 2026-07-27, amended same day: ack ONLY): after this
            // release's own header store, before any teardown, so
            // every free this death performs observes the epoch in
            // program order. Message pickup waits for the outermost
            // dispose's exit — between the zero store and the dispose
            // the entity is committed-dead with a live weak cell, and
            // drain user code could reach it through `WeakRef::get`.
            // The fast paths — allocation, free, non-final release —
            // carry no test.
            crate::epoch::checkpoint_ack();
        }

        tear
    }
}

/// The rc-walk decrement: the shared core of [`ll_release`] and
/// [`ll_release_batch`]. Returns the ABI verdict — the caller must run
/// teardown. Since the eager-death amendment there is no condemned
/// test and no deferral: the death branch is the same narrow counter
/// store as every other release.
#[cfg(feature = "rc-walk")]
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
    refcount == 0 && MemoryCategory::from_flags(flags) == MemoryCategory::GcHeap
}

/// [`ll_release`] without the epoch checkpoint, for compiler-emitted
/// runs of releases (a scope exit): lowering emits one
/// [`ll_gc_checkpoint_ack`](crate::gc::ll_gc_checkpoint_ack) before
/// the run, releases each reference with this variant, then one full
/// [`ll_gc_checkpoint`](crate::gc::ll_gc_checkpoint) after it — the
/// run pays the test once, and the pickup lands where the run's
/// transients are back at their true counts
/// (`rfc/model/gc/rc-walk.md`, "Batched releases", amendment
/// 2026-07-28). Identical to [`ll_release`] in every other respect;
/// in an rc-trace build the two are the same function.
///
/// # Safety
/// As [`ll_release`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_release_batch(entity: *mut RcHeader) -> bool {
    #[cfg(not(feature = "rc-walk"))]
    return unsafe { ll_release(entity) };

    #[cfg(feature = "rc-walk")]
    return unsafe { release_word(entity) };
}

/// The flags word of a possibly-walked header: a relaxed read under
/// `rc-walk` (the collector's byte stores race every plain header
/// access during an epoch), a plain read otherwise. The one
/// build-dispatching read helper — teardown paths and the weak
/// machinery share it rather than owning private copies.
#[inline]
pub(crate) unsafe fn header_flags(header: *const RcHeader) -> u32 {
    #[cfg(not(feature = "rc-walk"))]
    return unsafe { (*header).flags };
    #[cfg(feature = "rc-walk")]
    unsafe {
        mutator_load_header(header).1
    }
}

/// Read the refcount of a **published** header, same dispatch rule —
/// the counter twin of [`header_flags`].
#[inline]
pub(crate) unsafe fn header_refcount(header: *const RcHeader) -> u32 {
    #[cfg(not(feature = "rc-walk"))]
    return unsafe { (*header).refcount };
    #[cfg(feature = "rc-walk")]
    unsafe {
        mutator_load_header(header).0
    }
}

/// Rewrite the flags of a **published** header, same dispatch rule —
/// the write twin of [`header_flags`]. Post-publish flag writes on a
/// walked header must not be plain stores under `rc-walk`.
#[inline]
pub(crate) unsafe fn update_header_flags(header: *mut RcHeader, f: impl FnOnce(u32) -> u32) {
    #[cfg(not(feature = "rc-walk"))]
    unsafe {
        (*header).flags = f((*header).flags)
    };

    #[cfg(feature = "rc-walk")]
    unsafe {
        mutator_update_flags(header, f)
    };
}

/// Mutator-side relaxed header read; pair of the mutator's word store.
/// Under a live epoch every plain header access races the collector's
/// byte stores, which is undefined behaviour — these helpers are the
/// same instructions with the race made defined.
#[cfg(feature = "rc-walk")]
#[inline]
pub(crate) unsafe fn mutator_load_header(header: *const RcHeader) -> (u32, u32) {
    let word = unsafe { header_word_load(header as *mut RcHeader) };
    (word as u32, (word >> 32) as u32)
}

/// Mutator-side flags update as one relaxed whole-word store. May bury
/// a concurrent collector byte store — a lost stamp or verdict, always
/// the conservative direction (`rfc/model/gc/rc-walk.md`).
#[cfg(feature = "rc-walk")]
#[inline]
pub(crate) unsafe fn mutator_update_flags(header: *mut RcHeader, f: impl FnOnce(u32) -> u32) {
    let word = unsafe { header_word_load(header) };
    let flags = f((word >> 32) as u32) as u64;
    unsafe { header_word_store(header, flags << 32 | word as u32 as u64) };
}

/// The teardown guard's `+1` (relaxed whole-word; flags kept).
#[cfg(feature = "rc-walk")]
#[inline]
pub(crate) unsafe fn mutator_guard_retain(header: *mut RcHeader) {
    let word = unsafe { header_word_load(header) };
    let flags_half = word & 0xFFFF_FFFF_0000_0000;
    unsafe { header_word_store(header, flags_half | (word as u32 + 1) as u64) };
}

/// The teardown guard's `-1` (relaxed whole-word; flags kept): returns
/// the new refcount. Since the eager-death amendment a condemnation
/// landing mid-destructor changes nothing here — teardown always
/// finishes, and the component's later drain drops on the corpse.
#[cfg(feature = "rc-walk")]
#[inline]
pub(crate) unsafe fn mutator_unguard_release(header: *mut RcHeader) -> u32 {
    let word = unsafe { header_word_load(header) };
    let refcount = (word as u32) - 1;
    let flags_half = word & 0xFFFF_FFFF_0000_0000;
    unsafe { header_word_store(header, flags_half | refcount as u64) };
    refcount
}

#[cfg(test)]
mod tests;
