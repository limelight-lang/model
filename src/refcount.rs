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
//! That reader does not exist between S30 and S38 (`PLAN.md`): the
//! annotation is kept across the gap rather than taken out and put
//! back. `rc-cycle` collects in-line on the owning thread and adds the
//! collector thread as an accelerator over the same headers
//! (`rfc/model/gc/rc-cycle.md`, "What it is" and "Concurrency").

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
    /// enrols on the release path (`rfc/model/gc/rc-cycle.md`), which
    /// this category never reaches. No
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

/// Transient mark set on an arena entity while arena reset traces its
/// escaped subgraph (`rfc/model/memory/arena-reset.md`). Bit 2, which the
/// deleted CAS handoff of `heap-design.md` used to own together with bit
/// 3; arena entities never ran a strategy, so reset borrowed it, and with
/// the handoff gone it is the bit's only meaning. Cleared when a survivor
/// is promoted to the heap.
///
/// Bits 3 through 6 are free, and `rc-cycle` re-lays the whole word at
/// S31 — the layout there is `rfc/model/gc/cycle/questions.md` Y7.
pub const ARENA_RESET_MARK: u32 = 1 << 2;

/// Entity has weak references (side table exists).
pub const HAS_WEAK_REFERENCES: u32 = 1 << 7;
/// This instance owes a `__destruct`: set only when the user constructor
/// has returned successfully, and only for a class that has a destructor.
/// What every teardown path dispatches on (`rfc/runtime/object-lifecycle.md`).
pub const DESTRUCTOR_PENDING: u32 = 1 << 8;
/// `__destruct` has already run (exactly-once guard),
/// `rfc/runtime/object-lifecycle.md`.
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

/// Entity kind (bits 12-14): what makes a bare heap pointer
/// self-describing for freeing and for a `mixed` conversion. `0` object is
/// the zero default, so an entity built with no kind bits is an object;
/// strings, arrays and the other kinds set theirs explicitly. Authoritative
/// table: `rfc/model/classes.md`, "Flags layout".
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
/// `cells::trace_cells` strides it like an object. String, Box and WeakRef
/// own nothing a ring can pass through and stay out by that same
/// criterion.
///
/// Why a set rather than a mask over the kind bits, why it is built from
/// [`EntityKind`] rather than written as a literal, and why `Lazy` is
/// admitted before any factory stamps it: `dev/DECISIONS.md`, "the
/// candidate gate is a set of kinds, not a mask over their codes".
/// Membership costs a shift and a bit test on a word the release path
/// already holds ([`kind_may_close_a_cycle`]).
///
/// **No consumer since 2026-08-26**, when the candidate buffer this
/// gated was deleted with `rc-trace`. The set outlives the buffer: it is
/// the kind half of the gate `rc-cycle` enrols through, and S31.1
/// replaces the bitset with a range test over renumbered kind codes
/// (`PLAN.md`).
pub const CANDIDATE_KINDS: u32 = (1 << EntityKind::Object as u32)
    | (1 << EntityKind::Array as u32)
    | (1 << EntityKind::Reference as u32)
    | (1 << EntityKind::Lazy as u32);

/// True when the kind in this flags word is in [`CANDIDATE_KINDS`] — the
/// kinds whose non-zero decrement can leave a garbage ring behind. For
/// the reserved code 7 the answer is false, as for every kind outside
/// the set. Takes the flags word rather than the entity, because every
/// caller holds it in a register already.
///
/// **No caller since 2026-08-26**: the release path stopped enrolling
/// when the two collectors went. S31.3 gives it back as one mask over
/// five conditions, of which the kind is the second (`PLAN.md`).
#[inline]
pub fn kind_may_close_a_cycle(flags: u32) -> bool {
    (CANDIDATE_KINDS >> ((flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT)) & 1 != 0
}

/// True when the entity kind field is `Object` (the zero default). The
/// dispatch every teardown and trace path makes on a bare header, and a
/// flags-word predicate rather than a header one because most call sites
/// hold a raw `*mut RcHeader` and have the flags in a register already.
#[inline]
pub fn is_object(flags: u32) -> bool {
    flags & ENTITY_KIND_MASK == 0
}

/// **Dead since 2026-08-26, and unclaimed until S31.** These named the
/// entity's position in `rc-trace`'s candidate buffer, stored as
/// `index + 1` so that zero meant "position unknown"; the buffer and
/// every reader of the field went with the strategy. They stay declared
/// only because the flags word above bit 14 is re-laid whole at S31,
/// which gives 16-17 to the epoch, 18-19 to the maturation age and
/// 20-23 to the collector's reserve (`PLAN.md`). The old design is on
/// `archive/pre-rc-cycle`.
pub const CANDIDATE_INDEX_SHIFT: u32 = 15;
pub const CANDIDATE_INDEX_MASK: u32 = 0x0001_FFFF << CANDIDATE_INDEX_SHIFT;

/// **String entities only:** the bytes live out of line, through the
/// `data` pointer of `string::LLStringDynamic`, rather than inline after
/// the fixed fields. Set once by the factory and never flipped — nothing
/// promotes between the two layouts at run time.
///
/// It sits at bit 15, which the candidate index above claimed as its
/// lowest until that field died; a string never entered the buffer, so
/// the two never met. Nothing claims the bit now, and it stays
/// kind-scoped the way [`ARENA_RESET_MARK`] is kind-scoped in the
/// GC-state field. [`COW`] carries
/// copy-on-write and nothing else: one bit could not say both for an
/// oversize string, which is out of line by size and copy-on-write by
/// semantics (`dev/DECISIONS.md`, "a string's layout is its own header
/// bit"; `rfc/model/memory/large-entities.md`).
pub const STRING_OUT_OF_LINE: u32 = 1 << 15;
/// Largest buffer position the field can hold. Beyond it the index is
/// stored as zero: 131 070 candidates is many full thresholds without a
/// single collection point, and the fallback costs only speed.
pub const CANDIDATE_INDEX_MAX: usize = 0x0001_FFFF - 1;

/// The collector's **epoch byte** — header byte 6, flags bits 16-23,
/// and the collector's only claim on the header. Nothing writes it
/// today: it carried `rc-walk`'s maturity stamp, written by the
/// collector thread in the allocate-black branch and nowhere else, and
/// it went with that collector.
///
/// `rc-cycle` stamps the same region for the same purpose — an entity
/// is traced only once it has stayed a candidate across `k` collections
/// (`rfc/model/gc/rc-cycle.md`, "What it is") — but in two bits rather
/// than eight: S31 splits the byte into epoch at 16-17, maturation age
/// at 18-19 and the collector's reserve at 20-23 (`PLAN.md`).
///
/// It is a byte of its own because the mutator and the collector write
/// the header at different widths from different threads. Which widths
/// each side may use is S31.4's to settle; the Critic round of
/// 2026-08-26 found today's rule guarding writes while the defect it
/// has carried since day one is a read.
pub const EPOCH_BYTE_SHIFT: u32 = 16;
pub const EPOCH_BYTE_MASK: u32 = 0xFF << EPOCH_BYTE_SHIFT;

#[cfg(not(target_endian = "little"))]
compile_error!(
    "the header is one 8-byte word with the refcount in the low half, and \
     byte offset 6 of the collector's byte assumes a little-endian target"
);

/// The 8-byte header at offset 0 of every heap entity.
///
/// Aligned to 8: the factory publishes it as one 8-byte store, and the
/// wide header helpers are relaxed atomics on the whole word — both need
/// the address 8-aligned. Every real entity already was (the smallest
/// heap slot is 16 bytes); the attribute makes stack-built headers in
/// tests honest too.
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
/// refcount and flags separately: a torn pair would expose garbage kind
/// bits behind a live count. Until this store the slot reads refcount 0
/// — block commissioning zeroed it, or the previous occupant's death
/// left it — so a trace crossing the block classifies the slot as free
/// rather than reading a half-built entity. The store is a relaxed
/// atomic because that trace may run on a collector thread
/// (`rfc/model/gc/rc-cycle.md`, "Concurrency"), and without the
/// annotation the race is undefined behaviour.
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

/// Relaxed-atomic load of the whole header word: refcount in the low
/// half, flags in the high (little-endian, enforced above). Same
/// instruction as a plain load on x86-64 and AArch64; the annotation is
/// what makes the cross-thread race with the collector's byte stores
/// defined ([`EPOCH_BYTE_MASK`] is the byte in question).
#[inline]
unsafe fn header_word_load(header: *mut RcHeader) -> u64 {
    unsafe {
        (*(header as *const core::sync::atomic::AtomicU64))
            .load(core::sync::atomic::Ordering::Relaxed)
    }
}

/// Relaxed-atomic store of the whole header word; pair of
/// [`header_word_load`].
#[inline]
unsafe fn header_word_store(header: *mut RcHeader, word: u64) {
    unsafe {
        (*(header as *const core::sync::atomic::AtomicU64))
            .store(word, core::sync::atomic::Ordering::Relaxed)
    }
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
    // A non-zero decrement is where `rc-cycle` enrols a candidate
    // (`rfc/model/gc/rc-cycle.md`); the queue that receives it is S34,
    // so nothing is enrolled yet and a garbage ring is retained until
    // it lands. The epoch handshake this branch used to acknowledge died
    // with the collector that owned it.
    unsafe { release_word(entity) }
}

/// The decrement itself: the shared core of [`ll_release`] and
/// [`ll_release_batch`]. Returns the ABI verdict — the caller must run
/// teardown. There is no condemned test and no deferral, so the death
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
    refcount == 0 && MemoryCategory::from_flags(flags) == MemoryCategory::GcHeap
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

/// The flags word of a published header, read as a relaxed atomic: a
/// collector's byte stores race every plain access to a header it may
/// be tracing. The one read helper — teardown paths and the weak
/// machinery share it rather than owning private copies.
///
/// **Four bytes, not the word.** The store barrier reaches here right after
/// `ll_retain` has written the counter half, and an 8-byte load spanning
/// that fresh 4-byte store waits for the store buffer. Bytes 4-7 do not
/// overlap the counter at all, so nothing has to be forwarded — which is
/// the other half of [`refcount_load`]'s discipline rather than the same
/// case: there the load covers exactly the bytes the store wrote and
/// forwarding succeeds (`dev/BENCHMARKS.md`, "the barrier's header reads
/// go narrow").
#[inline]
pub(crate) unsafe fn header_flags(header: *const RcHeader) -> u32 {
    unsafe { flags_load(header) }
}

/// Read the refcount of a **published** header, same dispatch rule and
/// the same width rule — the counter twin of [`header_flags`].
#[inline]
pub(crate) unsafe fn header_refcount(header: *const RcHeader) -> u32 {
    unsafe { refcount_load(header) }
}

/// The count and the flags in **one** load, for a caller that wants both —
/// [`cow_separation_needed`] is the predicate over the pair. One wide load
/// beats two narrow ones where nothing narrow precedes it, which is the
/// case here and is not the case in the store path.
///
/// It buys no coherence the two readers above lack: the only concurrent
/// writer of a published header is the collector, and its one claim is the
/// epoch byte ([`EPOCH_BYTE_MASK`]), which no caller of this reads.
#[inline]
pub(crate) unsafe fn header_pair(header: *const RcHeader) -> (u32, u32) {
    unsafe { mutator_load_header(header) }
}

/// Rewrite the flags of a **published** header — the write twin of
/// [`header_flags`]. A post-publish flag write must not be a plain
/// store: the header may be under a concurrent trace.
#[inline]
pub(crate) unsafe fn update_header_flags(header: *mut RcHeader, f: impl FnOnce(u32) -> u32) {
    unsafe { mutator_update_flags(header, f) };
}

/// Mutator-side relaxed header read; pair of the mutator's word store.
/// While a collection is in flight every plain header access races the
/// collector's byte stores, which is undefined behaviour — these
/// helpers are the same instructions with the race made defined.
#[inline]
pub(crate) unsafe fn mutator_load_header(header: *const RcHeader) -> (u32, u32) {
    let word = unsafe { header_word_load(header as *mut RcHeader) };
    (word as u32, (word >> 32) as u32)
}

/// Mutator-side flags update as one relaxed whole-word store, which
/// spans the collector's byte at +6 and can bury a store into it.
/// S31.4 deletes this helper for that reason: the mutator's flags
/// writes are to stop below byte 2 (`PLAN.md`).
#[inline]
pub(crate) unsafe fn mutator_update_flags(header: *mut RcHeader, f: impl FnOnce(u32) -> u32) {
    let word = unsafe { header_word_load(header) };
    let flags = f((word >> 32) as u32) as u64;
    unsafe { header_word_store(header, flags << 32 | word as u32 as u64) };
}

/// The teardown guard's `+1` (relaxed whole-word; flags kept).
#[inline]
pub(crate) unsafe fn mutator_guard_retain(header: *mut RcHeader) {
    let word = unsafe { header_word_load(header) };
    let flags_half = word & 0xFFFF_FFFF_0000_0000;
    unsafe { header_word_store(header, flags_half | (word as u32 + 1) as u64) };
}

/// The teardown guard's `-1` (relaxed whole-word; flags kept): returns
/// the new refcount. A collection landing mid-destructor changes
/// nothing here — teardown always finishes, and the corpse rule drops a
/// component holding a member already at zero
/// (`rfc/model/gc/rc-cycle.md`, "Cycle teardown", step 1).
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
