//! The string entity: `RcHeader | len | hash | bytes`, one allocation,
//! bytes inline after the fixed fields (`rfc/model/strings.md`).
//!
//! This is the **inline** layout, the default form. The dynamic layout
//! (bytes out of line with spare capacity) shares `len` at +8 and `hash`
//! at +16 so that neither read has to decide which layout it is looking
//! at; only byte access and teardown branch. Which layout a string has is
//! the header flag [`STRING_OUT_OF_LINE`] (`rfc/model/strings.md`, "Two
//! Layouts Behind `StringInterface`").
//!
//! **Layout and copy-on-write are two facts, and the header keeps two
//! bits.** [`COW`] says the value separates when a second holder writes,
//! which is also what makes an arena entity counted and what makes it
//! copied rather than held when it escapes.
//!
//! `len` is 32 bits, so a string holds at most [`MAX_LEN`] bytes
//! (`rfc/model/strings.md`, "A string holds at most 4 GiB").
//!
//! An **interned name is a string entity like any other** — immortal
//! category, hash already computed — so `intern.rs` builds one through
//! [`init_at`] rather than laying out its own (`dev/ARCHITECTURE.md`,
//! invariant 13).

use crate::hash::hash_bytes;
use crate::journal::kinds::journal_event;
use crate::memory::buffer::Buffer;
use crate::memory::context::LLContext;
use crate::refcount::{
    COW, EntityKind, MemoryCategory, RcHeader, STRING_OUT_OF_LINE, publish_header,
};

/// The most bytes a string can hold: `len` is a `u32`
/// (`rfc/dev/DECISIONS.md`, "a string is capped at 4 GiB, and the length
/// gives up half its width to pay for capacity"). Language-visible — a
/// program that builds a longer string gets an error, never a truncation.
pub const MAX_LEN: usize = u32::MAX as usize;

/// The single length gate. Everything that creates or grows a string
/// asks this first; nothing casts a `usize` length to `u32` without
/// having passed through here.
#[inline]
pub fn fits(len: usize) -> bool {
    len <= MAX_LEN
}

/// String entity layout (`rfc/model/strings.md`): bytes inline after the
/// fixed fields, one allocation, no second hop.
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
    /// (audit `class.rs:115`).
    ///
    /// # Safety
    /// `s` must point to a live inline string entity allocated with its
    /// bytes, and must outlive the returned slice.
    #[inline]
    pub unsafe fn bytes<'a>(s: *const LLString) -> &'a [u8] {
        unsafe {
            let base = (s as *const u8).add(size_of::<LLString>());
            std::slice::from_raw_parts(base, (*s).len as usize)
        }
    }

    /// The cached hash, computed on first use.
    ///
    /// **Zero means "not computed"** (`rfc/model/strings.md`), which is
    /// unambiguous because [`hash_bytes`] never returns zero. An interned
    /// name arrives with the field already filled and never reaches the
    /// computing branch.
    ///
    /// The store is plain, not atomic, which is why a string in a shared
    /// category never reaches the computing branch: [`init_at`] hashes an
    /// immortal or long-lived string at creation, so the only strings
    /// left lazy are the ones a single thread owns. `intern` supplies the
    /// hash it already has; every other caller gets the same guarantee
    /// without knowing it exists.
    ///
    /// **Reads the bytes through [`string_bytes`], which branches on the
    /// layout.** The cached read does not have to — that is what the
    /// shared offsets buy — but computing does, and taking the inline
    /// accessor on a dynamic string would hash the `data` field and the
    /// bytes after it: a hash of an address, so equal content would hash
    /// differently and a payload that moved would rehash.
    ///
    /// # Safety
    /// `s` must point to a live string entity of either layout.
    #[inline]
    pub unsafe fn hash(s: *mut LLString) -> u64 {
        let cached = unsafe { (*s).hash };
        if cached != 0 {
            return cached;
        }

        let computed = hash_bytes(unsafe { string_bytes(s) });
        unsafe { (*s).hash = computed };
        computed
    }
}

/// The dynamic layout, `COW = 0`: the same first three words as
/// [`LLString`] — that is the point of the shared offsets — with the
/// padding at +12 spent on `capacity` and the bytes reached through
/// `data` (`rfc/model/strings.md`). 32 bytes.
///
/// Writes go in place and never separate: this is the non-COW form, and
/// the compiler allocates one only where it has proved a single owner.
/// Nothing promotes between the layouts at run time.
#[repr(C)]
pub struct LLStringDynamic {
    pub rc: RcHeader,
    pub len: u32,
    pub capacity: u32,
    pub hash: u64,
    pub data: *mut u8,
}

impl LLStringDynamic {
    /// The bytes, out of line.
    ///
    /// An empty dynamic string has no payload at all — the accumulator
    /// case, `$s = ""` before the first append — so `data` is null there,
    /// and `slice::from_raw_parts` requires a non-null pointer **even for
    /// a zero-length slice**. The empty case returns an empty slice
    /// without touching `data`.
    ///
    /// # Safety
    /// `s` must be a live dynamic string, and must outlive the slice.
    /// **An append may move the payload**, which invalidates any slice
    /// taken before it — the inline layout has no such hazard, since its
    /// bytes never move.
    #[inline]
    pub unsafe fn bytes<'a>(s: *const LLStringDynamic) -> &'a [u8] {
        let len = unsafe { (*s).len } as usize;
        if len == 0 {
            return &[];
        }

        unsafe { std::slice::from_raw_parts((*s).data, len) }
    }
}

/// The bytes of a string in either layout, branching on the one bit that
/// says which. Code that has proved the layout — the compiler's, mostly —
/// calls the layout's own accessor and skips the branch.
///
/// The flags come from [`crate::refcount::header_flags`] rather than a
/// plain field read: a collector byte-stores into the same word while it
/// traces, and a plain load races it. The helper is
/// the same instruction with the race made defined, so this costs
/// nothing; the reason it matters here is the licence a plain load gives
/// the optimiser to keep the header in a register across a loop of byte
/// accesses.
///
/// # Safety
/// `s` must be a live string entity, and must outlive the slice.
#[inline]
pub unsafe fn string_bytes<'a>(s: *const LLString) -> &'a [u8] {
    if unsafe { crate::refcount::header_flags(s as *const RcHeader) } & STRING_OUT_OF_LINE == 0 {
        unsafe { LLString::bytes(s) }
    } else {
        unsafe { LLStringDynamic::bytes(s as *const LLStringDynamic) }
    }
}

/// Lay a string out at `mem`, which must have room for the fixed fields
/// plus `bytes`.
///
/// Same commissioning contract as the object factory and the reference
/// box: body first, header published LAST as one 8-byte store, so an
/// entity-block slot reads refcount 0 until the string is fully formed
/// (`refcount::publish_header`).
///
/// `hash` is the precomputed hash, or zero to leave it lazy — except in
/// the two categories a second thread can reach, where a zero is computed
/// here instead. The lazy store in [`LLString::hash`] is plain, so
/// leaving a shared string unhashed makes two readers race on the same
/// field; hashing at creation costs one pass over bytes that were being
/// copied anyway.
///
/// # Safety
/// `mem` must be 8-aligned and own `size_of::<LLString>() + bytes.len()`
/// writable bytes; `bytes.len()` must have passed [`fits`].
pub(crate) unsafe fn init_at(
    mem: *mut u8,
    category: MemoryCategory,
    bytes: &[u8],
    hash: u64,
) -> *mut LLString {
    debug_assert!(fits(bytes.len()));
    // A supplied hash is taken on trust and never recomputed, so a caller
    // that hands over one this build does not produce writes a string that
    // will never be found by content. The only such caller is `intern`,
    // which passes `hash_bytes` of the same bytes; this is where both
    // operands exist at once, so this is where the trust is checked.
    debug_assert!(
        hash == 0 || hash == hash_bytes(bytes),
        "init_at was given a hash this build does not compute"
    );
    let shared = matches!(
        category,
        MemoryCategory::Immortal | MemoryCategory::LongLived
    );
    let hash = if hash == 0 && shared {
        hash_bytes(bytes)
    } else {
        hash
    };

    let s = mem as *mut LLString;
    unsafe {
        (&raw mut (*s).len).write(bytes.len() as u32);
        (&raw mut (*s).hash).write(hash);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), s.add(1) as *mut u8, bytes.len());
        publish_header(
            s as *mut RcHeader,
            RcHeader::new(category, COW | EntityKind::String.to_flags()),
        );
    }

    s
}

/// Where a fresh string of `size` bytes goes in `category`.
///
/// **One answer for both factories.** The copying one and the
/// assemble-in-place one made this decision separately, and they
/// disagreed within a day: `entity_alloc_in` used to refuse a
/// long-lived oversize entity, so one of them relied on that refusal
/// while the other stated it, and the refusal then moved.
enum Placement {
    /// Bytes after the fixed fields, one allocation.
    Inline,
    /// A 32-byte slot and a payload through the buffer machinery.
    OutOfLine,
    /// No layout serves this: null, for the caller to raise on.
    Refused,
}

/// The rule. Past what the category packs in one slot the inline layout
/// has nowhere to go, so the choice is out-of-line or nothing:
///
/// - the GC heap and the request arena have the payload machinery and
///   take the out-of-line form;
/// - the long-lived heap does not, because `string_die` reclaims the GC
///   heap alone, so nothing would ever free the payload
///   (`rfc/model/strings.md`); refused here rather than left to the
///   allocator, which serves a slot that size instead of refusing one;
/// - the immortal region keeps the inline layout whole, in the
///   block-aligned run `immortal_alloc` serves for a request over one
///   block payload — an immortal string is never freed, so the payload
///   machinery would buy it nothing.
fn placement(category: MemoryCategory, size: usize) -> Placement {
    if size <= crate::memory::routing::slot_limit(category) {
        return Placement::Inline;
    }

    match category {
        MemoryCategory::GcHeap | MemoryCategory::RequestArena => Placement::OutOfLine,
        MemoryCategory::LongLived => Placement::Refused,
        MemoryCategory::Immortal => Placement::Inline,
    }
}

/// Reserve a string of exactly `len` bytes and hand back **where to
/// write it**, for a caller that assembles the content in place instead
/// of copying a finished buffer in. Not an entity yet: the header is
/// unpublished, so nothing can reach it.
///
/// The layout is chosen here by the same rule the copying factory uses,
/// so the destination is [`Reserved::bytes`] rather than a fixed offset.
/// Write all `len` bytes there, then call [`publish_uninit`] to make it a
/// string. Between the two calls the memory belongs to the caller alone.
///
/// **Refused** when the allocation fails, when `len` is over [`MAX_LEN`],
/// and when the category has no layout for that size ([`placement`]);
/// [`Reserved::is_null`] reports it. A payload that fails after the
/// entity slot was taken leaves that slot where it is — in the arena it
/// dies with the reset, in the heap it stays out of circulation until the
/// thread ends, the same shape every other factory has on refusal.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`].
pub(crate) unsafe fn new_uninit(
    ctx: *mut LLContext,
    category: MemoryCategory,
    len: usize,
) -> Reserved {
    if !fits(len) {
        return Reserved::refused();
    }

    let size = size_of::<LLString>() + len;
    // The same size choice the copying factory makes, for the same
    // reason: past one slot the inline layout has nowhere to go. The
    // caller sees only where to write, which is what [`Reserved::bytes`]
    // answers for either layout.
    match placement(category, size) {
        Placement::Refused => return Reserved::refused(),
        Placement::OutOfLine => {
            let mem = unsafe {
                crate::memory::routing::entity_alloc_in(ctx, category, size_of::<LLStringDynamic>())
            };

            if mem.is_null() {
                return Reserved::refused();
            }

            let mut payload = Buffer::new();
            if !unsafe { grow_payload(ctx, category, &mut payload, len, 0) } {
                return Reserved::refused();
            }

            let s = mem as *mut LLStringDynamic;
            unsafe {
                (&raw mut (*s).len).write(len as u32);
                (&raw mut (*s).capacity).write(stored_capacity(payload.capacity));
                (&raw mut (*s).data).write(payload.data);
            }

            return Reserved {
                entity: mem,
                bytes: payload.data,
                out_of_line: true,
            };
        }
        Placement::Inline => {}
    }

    let mem = unsafe { crate::memory::routing::entity_alloc_in(ctx, category, size) };
    if mem.is_null() {
        return Reserved::refused();
    }

    unsafe { (&raw mut (*(mem as *mut LLString)).len).write(len as u32) };
    Reserved {
        entity: mem,
        bytes: unsafe { mem.add(size_of::<LLString>()) },
        out_of_line: false,
    }
}

/// A string reserved by [`new_uninit`] and not yet an entity: its header
/// is unpublished, so nothing can reach it, and the bytes are the
/// caller's to fill exactly once.
pub(crate) struct Reserved {
    entity: *mut u8,
    bytes: *mut u8,
    out_of_line: bool,
}

impl Reserved {
    fn refused() -> Self {
        Reserved {
            entity: std::ptr::null_mut(),
            bytes: std::ptr::null_mut(),
            out_of_line: false,
        }
    }

    /// True when the reservation failed and nothing was allocated.
    pub(crate) fn is_null(&self) -> bool {
        self.entity.is_null()
    }

    /// Where the `len` bytes go: after the fixed fields for the inline
    /// layout, in the payload for the out-of-line one.
    pub(crate) fn bytes(&self) -> *mut u8 {
        self.bytes
    }
}

/// Turn filled [`new_uninit`] memory into a live string: hash it when the
/// category is one another thread can reach — where the lazy hash is not
/// available, the rule [`init_at`] applies for the same reason — and
/// publish the header last, so no walker ever sees a header over bytes
/// that are not yet written.
///
/// # Safety
/// `r` came from [`new_uninit`] on this thread with the same `category`,
/// was not refused, and every one of its bytes has been written exactly
/// once since.
pub(crate) unsafe fn publish_uninit(r: Reserved, category: MemoryCategory) -> *mut LLString {
    let s = r.entity as *mut LLString;
    let shared = matches!(
        category,
        MemoryCategory::Immortal | MemoryCategory::LongLived
    );
    let layout = if r.out_of_line { STRING_OUT_OF_LINE } else { 0 };
    unsafe {
        let len = (*s).len as usize;
        // From the fill pointer rather than from the entity: the two
        // coincide only in the inline layout, and a shared category is
        // never out of line — reading past the fixed fields there would
        // hash the `data` pointer.
        let hash = if shared {
            hash_bytes(std::slice::from_raw_parts(r.bytes as *const u8, len))
        } else {
            0
        };

        (&raw mut (*s).hash).write(hash);
        publish_header(
            s as *mut RcHeader,
            RcHeader::new(category, COW | EntityKind::String.to_flags() | layout),
        );
    }

    s
}

/// Allocate an inline string holding `bytes` in `category`, hash left
/// lazy.
///
/// **Null** when the allocation fails, and null when `bytes` is longer
/// than [`MAX_LEN`] — the caller raises rather than truncating.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`].
pub unsafe fn ll_string_new(
    ctx: *mut LLContext,
    category: MemoryCategory,
    bytes: &[u8],
) -> *mut LLString {
    unsafe { new_with_hash(ctx, category, bytes, 0) }
}

/// [`ll_string_new`] with the hash already known — the interning path and
/// separation, where the bytes were hashed a moment ago or copied from an
/// entity that had hashed them. Zero leaves it lazy.
///
/// # Safety
/// As [`ll_string_new`].
pub(crate) unsafe fn new_with_hash(
    ctx: *mut LLContext,
    category: MemoryCategory,
    bytes: &[u8],
    hash: u64,
) -> *mut LLString {
    if !fits(bytes.len()) {
        return std::ptr::null_mut();
    }

    let size = size_of::<LLString>() + bytes.len();
    // Past what the category's allocator packs in one slot the inline
    // layout has nowhere to go: it would be refused, and before the
    // refusal existed it took a whole block of its own or left the walk.
    // The dynamic layout is a fixed 32-byte slot and a right-sized
    // payload, so the same content fits — and it keeps `COW`, because
    // only the layout changed and the value semantics did not.
    match placement(category, size) {
        Placement::OutOfLine => {
            return unsafe { new_out_of_line(ctx, category, bytes, 0, hash, COW) } as *mut LLString;
        }
        Placement::Refused => return std::ptr::null_mut(),
        Placement::Inline => {}
    }

    let mem = unsafe { crate::memory::routing::entity_alloc_in(ctx, category, size) };
    if mem.is_null() {
        return std::ptr::null_mut();
    }

    unsafe { init_at(mem, category, bytes, hash) }
}

/// Copy an inline string into a fresh entity — the string half of the
/// COW write barrier (`rfc/model/values.md`, "Copy-on-Write Protocol").
///
/// **Returns a +1 reference the caller owns**, like every other factory
/// in the crate, and that matters here because the caller is a store
/// site that retains again. The whole composition, counts in brackets:
///
/// ```text
/// let copy = ll_cow_separate(ctx, owner_cat, old);  // copy[1]
/// store_ptr(arena, owner_cat, slot, copy);          // copy[2], slot registered
/// ll_release(copy);                                 // copy[1] — the creation
///                                                   //   reference, now spent
/// drop_ref(owner_cat, old);                         // old loses this holder
/// ```
///
/// No line of it is optional. Skipping the release leaves the
/// copy at two for one holder, so the sharing test reads `2 > 1` on every
/// later write and COW is off for the rest of that value's life; writing
/// the slot raw instead of through `store_ptr` loses the escape
/// registration; and the release precedes the drop because `drop_ref`
/// runs `__destruct` bodies that can displace the copy from the slot just
/// written (`dev/DECISIONS.md`, "the creation reference is spent before
/// the displaced original is dropped").
/// **The original is not released here**: that is the holder's `drop_ref`
/// above, a different obligation from the copy's creation reference.
///
/// The copy's hash is left unset, because carrying the original's would
/// be wrong from the first byte of the write separation exists to serve,
/// and one missed invalidation propagates into every later copy of that
/// value ([`LLString::hash`] short-circuits on any non-zero field).
///
/// **Null on allocation failure**, propagated from [`new_with_hash`]: the
/// caller must raise rather than store it, since NULL in a non-nullable
/// pointer slot is the uninitialized marker and would turn an
/// out-of-memory into "must not be accessed before initialization" on the
/// next read.
///
/// # Safety
/// `s` must be a live **copy-on-write** string of either layout; `ctx`
/// per [`crate::memory::context::ll_arena_alloc`].
pub unsafe fn separate(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    s: *mut LLString,
) -> *mut LLString {
    debug_assert_ne!(
        unsafe { crate::refcount::header_flags(s as *const RcHeader) } & COW,
        0,
        "a non-COW string is outside the COW rule and writes in place"
    );
    // Through the layout-agnostic accessor, because a string is out of
    // line by size as well as by proof: taking the inline accessor on one
    // would copy the `data` pointer and the bytes after it.
    let bytes = unsafe { string_bytes(s) };
    unsafe {
        new_with_hash(
            ctx,
            crate::refcount::separation_category(owner_cat),
            bytes,
            0,
        )
    }
}

/// Allocate a **dynamic** string holding `bytes`: the entity in
/// `category`, the payload through the memory manager's buffer
/// machinery, which routes it by size and hands back a block whose kind
/// says who reclaims it (`memory/buffer_arena.rs`).
///
/// `hint` is a growth recommendation for the payload, 0 to let the
/// pressure mode decide (`rfc/model/memory/buffers.md`).
///
/// **Only `GcHeap` and `RequestArena`** (`rfc/model/strings.md`, "Two
/// Layouts Behind `StringInterface`").
///
/// **Null** when either allocation fails or the content is past
/// [`MAX_LEN`], leaving nothing half-built.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`].
pub unsafe fn ll_string_new_dynamic(
    ctx: *mut LLContext,
    category: MemoryCategory,
    bytes: &[u8],
    hint: usize,
) -> *mut LLStringDynamic {
    if !fits(bytes.len()) {
        return std::ptr::null_mut();
    }

    // Refused, not redirected, and refused *here* rather than left to
    // the routing below, which serves every category: an
    // immortal-flagged dynamic string would land in a GC entity block —
    // walked by the census, never released (retain and release no-op on
    // immortal), and pinned for the life of the process. A
    // `debug_assert` would say the same thing in debug and nothing at
    // all in release, which is where the damage would happen.
    if matches!(
        category,
        MemoryCategory::LongLived | MemoryCategory::Immortal
    ) {
        return std::ptr::null_mut();
    }

    unsafe { new_out_of_line(ctx, category, bytes, hint, 0, 0) }
}

/// The out-of-line form, whichever reason it has: `extra_flags` is `COW`
/// for a string that is out of line because its content did not fit one
/// slot, and empty for the proved-single-owner form [`ll_string_new_dynamic`]
/// builds. `hash` is stored as given; zero leaves it lazy.
///
/// **Null** on either allocation failing, leaving nothing half-built.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`]; `bytes.len()`
/// must have passed [`fits`], and `category` must be one the payload
/// machinery serves — `GcHeap` or `RequestArena`.
unsafe fn new_out_of_line(
    ctx: *mut LLContext,
    category: MemoryCategory,
    bytes: &[u8],
    hint: usize,
    hash: u64,
    extra_flags: u32,
) -> *mut LLStringDynamic {
    debug_assert!(fits(bytes.len()));
    debug_assert!(matches!(
        category,
        MemoryCategory::GcHeap | MemoryCategory::RequestArena
    ));
    // The same trust check [`init_at`] makes, for the same reason: a hash
    // this build does not produce writes a string nothing finds by
    // content, and this is the other place both operands exist at once.
    debug_assert!(
        hash == 0 || hash == hash_bytes(bytes),
        "new_out_of_line was given a hash this build does not compute"
    );
    let mem = unsafe {
        crate::memory::routing::entity_alloc_in(ctx, category, size_of::<LLStringDynamic>())
    };

    if mem.is_null() {
        return std::ptr::null_mut();
    }

    let s = mem as *mut LLStringDynamic;

    // A payload is allocated when there is content or a hint to honour.
    // An empty string with no hint keeps a null `data`, which the byte
    // accessor answers without dereferencing.
    let mut payload = Buffer::new();
    let want = bytes.len().max(hint.min(MAX_LEN));
    if want > 0 && !unsafe { grow_payload(ctx, category, &mut payload, want, hint.min(MAX_LEN)) } {
        // The entity's memory is left where it is: in the arena it dies
        // with the reset, in the heap the slot came off the free list and
        // stays out of circulation until the thread ends — a leak, not a
        // corruption, and the same shape every other factory has on OOM.
        return std::ptr::null_mut();
    }

    if !bytes.is_empty() {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), payload.data, bytes.len()) };
    }

    unsafe {
        (&raw mut (*s).len).write(bytes.len() as u32);
        (&raw mut (*s).capacity).write(stored_capacity(payload.capacity));
        (&raw mut (*s).hash).write(hash);
        (&raw mut (*s).data).write(payload.data);
        publish_header(
            s as *mut RcHeader,
            RcHeader::new(
                category,
                EntityKind::String.to_flags() | STRING_OUT_OF_LINE | extra_flags,
            ),
        );
    }

    s
}

/// What goes in the 32-bit `capacity` field when the buffer machinery
/// granted more than the field can hold.
///
/// The growth policy doubles and honours the caller's hint, and neither
/// is bounded by [`MAX_LEN`], so a string near the cap can be granted a
/// payload above 4 GiB. Truncating the number would make `capacity < len`
/// representable, which nothing else in the crate expects and which the
/// next reader of these two fields — the spare-capacity path, or the
/// array storage that reuses the same pair at the same offsets — would
/// take as gospel. Recording the cap instead under-reports the payload,
/// which costs one extra growth at worst and can never over-report.
#[inline]
fn stored_capacity(granted: usize) -> u32 {
    granted.min(MAX_LEN) as u32
}

/// Grow `payload` to hold `min_capacity`, through whichever half of the
/// buffer machinery the category calls for. Both halves extend at their
/// own bump top when the payload is still the last chunk taken from it,
/// and fall back to alloc-new, copy, free-old. False on refusal, with the
/// buffer untouched.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`].
unsafe fn grow_payload(
    ctx: *mut LLContext,
    category: MemoryCategory,
    payload: &mut Buffer,
    min_capacity: usize,
    hint: usize,
) -> bool {
    unsafe { crate::memory::routing::body_ensure(ctx, category, payload, min_capacity, hint) }
}

/// Append to an out-of-line string, in place. No sharing test is made
/// here, and the two callers earn that differently: the non-COW form is
/// the one the compiler allocated because it proved a single owner
/// (`rfc/model/strings.md`), and a copy-on-write string reaches this
/// only through `ll_cow_separate`, which has already given the writer a
/// sole-owner entity.
///
/// The cached hash is cleared, which is this function's job rather than
/// the payload's: after this the bytes are different, and nothing
/// recomputes a hash field that is not zero.
///
/// **False on refusal** — the 4 GiB cap or an allocation that failed —
/// with the string exactly as it was.
///
/// # Safety
/// `s` must be a live out-of-line string that this writer owns alone;
/// `ctx` per [`crate::memory::context::ll_arena_alloc`].
pub unsafe fn ll_string_append(ctx: *mut LLContext, s: *mut LLStringDynamic, extra: &[u8]) -> bool {
    let flags = unsafe { crate::refcount::header_flags(s as *const RcHeader) };
    debug_assert_ne!(
        flags & STRING_OUT_OF_LINE,
        0,
        "an inline string has no spare capacity to append into"
    );
    // Out of line no longer implies sole ownership: a string is also out
    // of line because its content did not fit one slot, and that one is
    // copy-on-write. Writing into a shared one in place is the language
    // break the barrier exists to prevent, so the ownership half of the
    // contract is asserted rather than assumed — through the barrier's
    // own predicate, because "count is 1" is not the test: retain and
    // release no-op on an immortal entity, which therefore reads 1
    // forever while being the most shared thing in the process.
    debug_assert!(
        !crate::refcount::cow_separation_needed(flags, unsafe {
            crate::refcount::header_refcount(s as *mut RcHeader)
        }),
        "a shared copy-on-write string separates before it is appended to"
    );
    if extra.is_empty() {
        return true;
    }

    let old_len = unsafe { (*s).len } as usize;
    let Some(needed) = old_len.checked_add(extra.len()).filter(|n| fits(*n)) else {
        return false;
    };

    let category = unsafe { crate::object::header_category(s as *const RcHeader) };
    let mut payload = Buffer {
        data: unsafe { (*s).data },
        len: old_len,
        capacity: unsafe { (*s).capacity } as usize,
    };

    if !unsafe { grow_payload(ctx, category, &mut payload, needed, 0) } {
        return false;
    }

    unsafe {
        std::ptr::copy_nonoverlapping(extra.as_ptr(), payload.data.add(old_len), extra.len());
        (*s).data = payload.data;
        (*s).capacity = stored_capacity(payload.capacity);
        (*s).len = needed as u32;
        (*s).hash = 0;
    }

    true
}

/// Carry a surviving dynamic string's payload out of the arena that is
/// about to reset (`rfc/model/strings.md`, "An arena string that survives
/// the reset takes its payload with it").
///
/// The entity's header stays where it is — the block holding it is
/// retained — but the payload is arena memory and would go back to the
/// pool at the reset, so the survivor would read recycled bytes. A
/// payload above a block payload is OS-direct: ownership transfers, the
/// pointer does not move and nothing can be refused. A smaller one is
/// copied into a fresh heap payload (`dev/DECISIONS.md`, "promotion
/// carries a survivor's payload, and the two routes are not the same
/// route").
///
/// **False when the copy was refused.** The caller keeps the payload's
/// arena block out of circulation instead; the string then reads its old
/// bytes for the rest of its life, and [`string_die`] finds a retained
/// block and leaves it alone.
///
/// # Safety
/// `s` must be a live dynamic string in `arena`, mid-reset.
pub(crate) unsafe fn carry_payload_out_of(
    arena: *mut crate::memory::arena::Arena,
    s: *mut LLStringDynamic,
) -> bool {
    let (data, capacity) = unsafe { ((*s).data, (*s).capacity as usize) };
    if data.is_null() {
        return true; // an empty string has no payload to carry
    }

    if capacity > crate::memory::block_pool::BLOCK_PAYLOAD {
        // True whatever the log says, because the two outcomes of
        // `forget_large` are both carries. It found the record: the run
        // transfers, which is the whole point of the OS-direct branch. It
        // did not: nothing will free the run, so the payload keeps its
        // address and leaks — the safe direction, and the only other one
        // available. Returning the miss as a refusal would send the
        // caller into the copy's fallback, which stamps
        // `BLOCK_KIND_RETAINED` over the run's `LargeHeader` at offset 0
        // and leaves `ll_free` reading a kind the run does not have.
        let forgotten = unsafe { (*arena).forget_large(data) };
        debug_assert!(forgotten, "an OS-direct payload the arena never logged");
        return true;
    }

    let mut carried = Buffer::new();
    if crate::memory::buffer_arena::buffer_ensure_longlived(&mut carried, capacity, 0).is_null() {
        return false;
    }

    unsafe {
        std::ptr::copy_nonoverlapping(data, carried.data, (*s).len as usize);
        (*s).data = carried.data;
        (*s).capacity = stored_capacity(carried.capacity);
    }

    true
}

/// Teardown for a string whose count reached zero (or that a collector
/// owns): the payload first if the layout has one, then the entity's own
/// memory by category. A string holds no children, so there is nothing
/// to release first and no destructor to run.
///
/// The payload is freed only outside the request arena. An arena payload
/// is arena garbage — in-block it dies with the reset, OS-direct it is
/// tracked by the arena — and handing it to the long-lived free routine
/// would offer a block of the wrong kind.
///
/// # Safety
/// `s` must be a live string entity.
pub(crate) unsafe fn string_die(s: *mut LLString) {
    // A death a reset in flight must not walk past (`memory::reset_window`).
    crate::memory::reset_window::record_death(s as *mut crate::refcount::RcHeader);
    journal_event!(
        crate::journal::kinds::KIND_ENTITY_DEATH,
        s as u64,
        EntityKind::String as u64,
        0
    );
    let owner_cat = unsafe { crate::object::header_category(s as *const RcHeader) };
    let inline =
        unsafe { crate::refcount::header_flags(s as *const RcHeader) } & STRING_OUT_OF_LINE == 0;
    if !inline && owner_cat != MemoryCategory::RequestArena {
        let d = s as *mut LLStringDynamic;
        let (data, capacity) = unsafe { ((*d).data, (*d).capacity as usize) };
        if !data.is_null() {
            unsafe { crate::memory::buffer_arena::buffer_free_longlived_payload(data, capacity) };
        }
    }

    if owner_cat == MemoryCategory::GcHeap {
        unsafe { crate::memory::stdapi::ll_free(s as *mut u8) };
    }
}

#[cfg(test)]
mod tests;
