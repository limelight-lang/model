//! The string entity: `RcHeader | len | hash | bytes`, one allocation,
//! bytes inline after the fixed fields (`rfc/model/strings.md`).
//!
//! This is the **inline** layout, `COW = 1` — the default form and the
//! only one the crate produces so far. The dynamic layout (`COW = 0`,
//! bytes out of line with spare capacity) shares `len` at +8 and `hash`
//! at +16 so that neither read has to decide which layout it is looking
//! at; only byte access and teardown branch. Nothing promotes between
//! the two at run time: the compiler picks one at allocation, and moving
//! between them would rewrite the body under a header the collector may
//! be reading.
//!
//! `len` is 32 bits, so a string holds at most [`MAX_LEN`] bytes. Every
//! path that creates or grows one passes through [`fits`]: a length
//! truncated to 32 bits is a write past the end of the buffer, which is
//! the worst defect available here, and the check being in one place is
//! also what gives a future wider string a single hook.
//!
//! An **interned name is a string entity like any other** — immortal
//! category, hash already computed — so `intern.rs` builds one through
//! [`init_at`] rather than laying out its own (`dev/ARCHITECTURE.md`,
//! invariant 13).

use crate::memory::buffer::Buffer;
use crate::memory::context::{LLContext, resolve_arena};
use crate::memory::immortal::immortal_alloc;
use crate::refcount::{COW, EntityKind, MemoryCategory, RcHeader, publish_header};

/// The most bytes a string can hold: `len` is a `u32`
/// (`rfc/dev/DECISIONS.md`, 2026-08-04). Language-visible — a program
/// that builds a longer string gets an error, never a truncation.
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
    /// The store is plain, not atomic: a lazily hashed string is written
    /// by the thread that owns it. An immortal string is shared, and that
    /// is exactly why `intern` hashes eagerly — nothing lazily writes to
    /// a string another thread can see.
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
/// # Safety
/// `s` must be a live string entity, and must outlive the slice.
#[inline]
pub unsafe fn string_bytes<'a>(s: *const LLString) -> &'a [u8] {
    if unsafe { (*s).rc.flags } & COW != 0 {
        unsafe { LLString::bytes(s) }
    } else {
        unsafe { LLStringDynamic::bytes(s as *const LLStringDynamic) }
    }
}

/// The value a genuine zero hash is mapped to, so that zero can mean
/// "not computed" without the sentinel ever colliding with a real hash.
/// Part of the frozen definition of the hash rather than the caller's
/// job: the compiler folds the same value the runtime computes.
const ZERO_REPLACEMENT: u64 = 1;

/// The string hash. FNV-1a for now.
///
/// **The function is a build-time choice** — the same axis the GC
/// strategy uses — and the decision (`rfc/model/strings.md`, "The hash
/// function is a build-time choice") names rapidhash v3 as the default.
/// Porting it is its own step: it has to match the reference
/// implementation constant for constant, checked against the author's
/// test vectors, because a one-constant divergence between the
/// compiler's folded hash and the runtime's produces no crash, only
/// wrong misses in a hash table. Until then this is the function that
/// was already here, behind the name everything else calls.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    if h == 0 { ZERO_REPLACEMENT } else { h }
}

/// Lay a string out at `mem`, which must have room for the fixed fields
/// plus `bytes`.
///
/// Same commissioning contract as the object factory and the reference
/// box: body first, header published LAST as one 8-byte store, so an
/// entity-block slot reads refcount 0 until the string is fully formed
/// (`rfc/model/gc/rc-walk.md`, Phase 1).
///
/// `hash` is the precomputed hash, or zero to leave it lazy.
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
    let mem = match category {
        MemoryCategory::RequestArena => unsafe { (*resolve_arena(ctx)).alloc(size) },
        MemoryCategory::GcHeap | MemoryCategory::LongLived => unsafe {
            crate::memory::heap::entity_alloc(size)
        },
        MemoryCategory::Immortal => immortal_alloc(size),
    };
    if mem.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { init_at(mem, category, bytes, hash) }
}

/// Where a separated copy lands — decided by **the holder's** category,
/// not the original's and not the writing context's.
///
/// The copy is a fresh entity nothing has registered anywhere, so the
/// only thing that can go wrong is a holder outliving it. An arena
/// holder can hold anything, since it dies at the reset itself; every
/// other holder needs something that outlives the request, so the copy
/// goes to the GC heap. That also keeps a copy out of the two categories
/// that cannot own a written string at all: immortal is shared
/// process-wide, and `string_die` frees only `GcHeap`, so a long-lived
/// copy could never be reclaimed.
///
/// The original's category does not enter into it. An arena holder
/// writing to an interned string gets an arena copy — a bump and a reset,
/// rather than a heap allocation plus a release-at-reset record for a
/// value that dies at the reset regardless.
#[inline]
fn separation_category(owner_cat: MemoryCategory) -> MemoryCategory {
    match owner_cat {
        MemoryCategory::RequestArena => MemoryCategory::RequestArena,
        _ => MemoryCategory::GcHeap,
    }
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
/// drop_ref(owner_cat, old);                         // old loses this holder
/// ll_release(copy);                                 // copy[1] — the creation
///                                                   //   reference, now spent
/// ```
///
/// Skipping that last release leaves the copy at two for one holder: it
/// never reaches zero, and — worse than the leak — the sharing test reads
/// `2 > 1` on every later write, so the value separates forever and COW
/// is off for the rest of its life. Skipping the `store_ptr` instead, and
/// writing the slot raw to "transfer" the reference, loses the escape
/// registration that store performs.
///
/// **The original is not released here.** That is the holder's
/// `drop_ref` above: the copy's creation reference and the original's
/// displaced reference are two different obligations, and only the first
/// belongs to this function.
///
/// The copy's hash is left unset. Carrying the original's would be
/// correct for the instant the copy exists unwritten, and wrong from the
/// first byte of the write that separation exists to serve; and since a
/// separation copies whatever the original holds, one missed
/// invalidation would propagate into every later copy of that value and
/// never be recomputed ([`LLString::hash`] short-circuits on any non-zero
/// field). Recomputing costs one pass over bytes the caller is about to
/// walk anyway.
///
/// **Null on allocation failure**, propagated from [`new_with_hash`]: the
/// caller must raise rather than store it, since NULL in a non-nullable
/// pointer slot is the uninitialized marker and would turn an
/// out-of-memory into "must not be accessed before initialization" on the
/// next read.
///
/// # Safety
/// `s` must be a live **inline** string; `ctx` per
/// [`crate::memory::context::ll_arena_alloc`].
pub unsafe fn separate(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    s: *mut LLString,
) -> *mut LLString {
    debug_assert_ne!(
        unsafe { (*s).rc.flags } & COW,
        0,
        "a dynamic string is outside the COW rule and writes in place"
    );
    let bytes = unsafe { LLString::bytes(s) };
    unsafe { new_with_hash(ctx, separation_category(owner_cat), bytes, 0) }
}

/// Allocate a **dynamic** string holding `bytes`: the entity in
/// `category`, the payload through the memory manager's buffer
/// machinery, which routes it by size and hands back a block whose kind
/// says who reclaims it (`memory/buffer_arena.rs`).
///
/// `hint` is a growth recommendation for the payload, 0 to let the
/// pressure mode decide (`rfc/model/memory/buffers.md`).
///
/// **Only `GcHeap` and `RequestArena`.** A dynamic string is the freely
/// mutable form, so it cannot be immortal — that would be a
/// process-wide shared string written in place — and it cannot be
/// long-lived, because [`string_die`] can only reclaim the GC heap.
/// `strings.md` permits both layouts in every category in general; this
/// is the exclusion that follows from the non-COW half of it.
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
    // Refused, not redirected. A `debug_assert` here would vanish in
    // release into the catch-all arm below, which would put an
    // immortal-flagged entity in a GC entity block — walked by the
    // census, never released (retain and release no-op on immortal), and
    // pinned for the life of the process.
    let mem = match category {
        MemoryCategory::RequestArena => unsafe {
            (*resolve_arena(ctx)).alloc(size_of::<LLStringDynamic>())
        },
        MemoryCategory::GcHeap => unsafe {
            crate::memory::heap::entity_alloc(size_of::<LLStringDynamic>())
        },
        MemoryCategory::LongLived | MemoryCategory::Immortal => std::ptr::null_mut(),
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
        (&raw mut (*s).hash).write(0);
        (&raw mut (*s).data).write(payload.data);
        publish_header(
            s as *mut RcHeader,
            RcHeader::new(category, EntityKind::String.to_flags()),
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
/// buffer machinery the category calls for: the request arena extends at
/// its bump top when it can, everything else is alloc-new, copy,
/// free-old. False on refusal, with the buffer untouched.
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
    let grown = match category {
        MemoryCategory::RequestArena => unsafe {
            crate::memory::buffer::buffer_ensure(
                &mut *resolve_arena(ctx),
                payload,
                min_capacity,
                hint,
            )
        },
        _ => crate::memory::buffer_arena::buffer_ensure_longlived(payload, min_capacity, hint),
    };
    !grown.is_null()
}

/// Append to a dynamic string, in place — no sharing test, no copy: the
/// non-COW form is the one the compiler allocated because it proved a
/// single owner (`rfc/model/strings.md`).
///
/// The cached hash is cleared, which is this function's job rather than
/// the payload's: after this the bytes are different, and nothing
/// recomputes a hash field that is not zero.
///
/// **False on refusal** — the 4 GiB cap or an allocation that failed —
/// with the string exactly as it was.
///
/// # Safety
/// `s` must be a live dynamic string; `ctx` per
/// [`crate::memory::context::ll_arena_alloc`].
pub unsafe fn ll_string_append(
    ctx: *mut LLContext,
    s: *mut LLStringDynamic,
    extra: &[u8],
) -> bool {
    debug_assert_eq!(
        unsafe { (*s).rc.flags } & COW,
        0,
        "an inline string separates instead of appending"
    );
    if extra.is_empty() {
        return true;
    }
    let old_len = unsafe { (*s).len } as usize;
    let Some(needed) = old_len.checked_add(extra.len()).filter(|n| fits(*n)) else {
        return false;
    };

    let category = unsafe { (*s).rc.memory_category() };
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
    let owner_cat = unsafe { crate::object::header_category(s as *const RcHeader) };
    let inline = unsafe { (*s).rc.flags } & COW != 0;
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
mod tests {
    use super::*;
    use crate::memory::arena::Arena;
    use crate::refcount::{ENTITY_KIND_MASK, ll_release};

    /// The offsets the second layout has to match: a dynamic string
    /// (`rfc/model/strings.md`) puts `len` and `hash` in the same places,
    /// so reading either does not require deciding which layout this is.
    /// Swapping the two fields still compiles and still passes every
    /// other test here, which is why the contract is pinned.
    #[test]
    fn layout_matches_the_string_design() {
        assert_eq!(size_of::<RcHeader>(), 8, "header must stay 8 bytes");
        assert_eq!(std::mem::offset_of!(LLString, rc), 0);
        assert_eq!(std::mem::offset_of!(LLString, len), 8);
        let probe = LLString {
            rc: RcHeader::new(MemoryCategory::Immortal, COW),
            len: 0,
            hash: 0,
        };
        assert_eq!(
            std::mem::size_of_val(&probe.len),
            4,
            "len is 32-bit: the 4 GiB cap"
        );
        assert_eq!(
            std::mem::offset_of!(LLString, hash),
            16,
            "+12 stays free for the dynamic layout's capacity"
        );
        assert_eq!(size_of::<LLString>(), 24, "bytes start right after");
    }

    #[test]
    fn a_heap_string_is_a_cow_kind_1_entity_that_dies_by_refcount() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"hello") };
        assert!(!s.is_null());

        let rc = unsafe { &(*s).rc };
        assert_eq!(rc.refcount, 1);
        assert_eq!(rc.flags & ENTITY_KIND_MASK, EntityKind::String.to_flags());
        assert_ne!(rc.flags & COW, 0, "the inline layout is the COW form");
        assert_eq!(rc.memory_category(), MemoryCategory::GcHeap);
        assert_eq!(unsafe { LLString::bytes(s) }, b"hello");
        assert_eq!(unsafe { (*s).len }, 5);

        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// The bytes live past `size_of::<LLString>()`, so the allocation has
    /// to be sized for them and the copy has to land there. A string one
    /// byte longer than the fixed fields would pass a size-class check
    /// either way; content is what catches a wrong base.
    #[test]
    fn bytes_are_inline_and_survive_a_second_string_landing_beside_them() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let long = vec![b'x'; 100];
        let a = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, &long) };
        let b = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"beside") };
        assert_eq!(unsafe { LLString::bytes(a) }, &long[..]);
        assert_eq!(unsafe { LLString::bytes(b) }, b"beside");
        unsafe {
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_entity_die(a as *mut RcHeader);
            assert!(ll_release(b as *mut RcHeader));
            crate::object::ll_entity_die(b as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    #[test]
    fn the_hash_is_computed_once_on_demand_and_never_zero() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"lazy") };
        assert_eq!(unsafe { (*s).hash }, 0, "not computed at allocation");

        let first = unsafe { LLString::hash(s) };
        assert_ne!(first, 0, "zero is the sentinel, never a value");
        assert_eq!(unsafe { (*s).hash }, first, "cached in the entity");

        // Poison the bytes: a second call that recomputed would notice.
        unsafe { (s.add(1) as *mut u8).write(b'L') };
        assert_eq!(unsafe { LLString::hash(s) }, first, "read from the cache");

        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// `hash_bytes` maps a genuine zero away, so the sentinel stays
    /// unambiguous. FNV-1a reaches zero on inputs nobody will type, so
    /// the mapping is checked directly rather than through a search.
    #[test]
    fn a_genuine_zero_hash_is_mapped_away() {
        assert_eq!(hash_bytes(&[]), 0xcbf2_9ce4_8422_2325);
        assert_ne!(hash_bytes(b"anything"), 0);
        assert_eq!(ZERO_REPLACEMENT, 1);
    }

    /// A string that lives in the request arena is reclaimed by the
    /// reset, not by its own teardown: the same entity, a different
    /// owner of its memory (`rfc/model/memory/arenas.md`).
    #[test]
    fn an_arena_string_is_left_to_the_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::RequestArena, b"scoped") };
        assert_eq!(
            unsafe { (*s).rc.memory_category() },
            MemoryCategory::RequestArena
        );
        assert_eq!(unsafe { LLString::bytes(s) }, b"scoped");
        arena.reset(|_| {});
    }

    /// A heap string lands in an entity block, so the walker meets it.
    /// It must be counted as its own kind and contribute no edges: a
    /// string's payload is bytes, so it is a leaf and cannot close a ring
    /// (`walk::trace_entity`).
    #[test]
    fn the_walker_counts_a_heap_string_as_a_leaf() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let before = unsafe { crate::walk::heap_census() };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"walked") };
        let after = unsafe { crate::walk::heap_census() };

        let k = EntityKind::String as usize;
        assert_eq!(after.by_kind[k], before.by_kind[k] + 1);
        assert_eq!(after.edges, before.edges, "a string has no out-edges");

        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
        let freed = unsafe { crate::walk::heap_census() };
        assert_eq!(freed.by_kind[k], before.by_kind[k], "and it goes away");
        arena.reset(|_| {});
    }

    /// The four branches of the COW rule, in the order
    /// `rfc/model/values.md` fixes them. Each is checked through the
    /// generic barrier rather than through `separate`, because the order
    /// of the tests is the part that matters: a count read before the
    /// category would call an interned string a sole owner.
    #[test]
    fn a_sole_owner_in_the_heap_writes_in_place() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"mine") };
        let after = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, s as *mut RcHeader)
        };
        assert_eq!(after as usize, s as usize, "no copy for a lone holder");
        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// The whole composition a holder performs, with the counts checked
    /// at the end — separation, the store that retains, the drop of what
    /// the slot displaced, and the release of the copy's creation
    /// reference. That last one is the step whose absence leaves the copy
    /// at two for one holder: not merely leaked, but reading as shared on
    /// every later write, so the value would separate forever.
    #[test]
    fn separating_then_storing_leaves_exactly_one_holder_on_each_side() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        // Two holders of one string: a slot, and the local that made it.
        let original = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"shared") };
        let mut slot: *mut RcHeader = std::ptr::null_mut();
        unsafe {
            crate::memory::barrier::store_ptr(
                &raw mut arena,
                MemoryCategory::GcHeap,
                &raw mut slot,
                original as *mut RcHeader,
            )
        };
        assert_eq!(unsafe { (*original).rc.refcount }, 2);

        let copy = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, slot)
        };
        assert_ne!(copy as usize, original as usize, "two holders, so a copy");
        unsafe {
            crate::memory::barrier::store_ptr(
                &raw mut arena,
                MemoryCategory::GcHeap,
                &raw mut slot,
                copy,
            );
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, original as *mut RcHeader);
            assert!(!ll_release(copy), "the creation reference, spent");
        }

        assert_eq!(unsafe { (*copy).refcount }, 1, "the slot alone holds it");
        assert_eq!(
            unsafe { (*original).rc.refcount },
            1,
            "and the local alone holds the original"
        );
        assert_eq!(unsafe { LLString::bytes(copy as *mut LLString) }, b"shared");

        unsafe {
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, slot);
            assert!(ll_release(original as *mut RcHeader));
            crate::object::ll_entity_die(original as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// The copy's hash starts unset even though its bytes are the
    /// original's: the write that separation exists to serve is about to
    /// invalidate it, and a carried hash that someone forgets to clear
    /// would propagate into every later copy of that value — nothing
    /// recomputes a non-zero one.
    #[test]
    fn a_copy_starts_without_a_hash() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"hashed") };
        let hash = unsafe { LLString::hash(s) };
        assert_ne!(hash, 0);
        unsafe { crate::refcount::ll_retain(s as *mut RcHeader) };

        let copy = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, s as *mut RcHeader)
        } as *mut LLString;
        assert_eq!(unsafe { (*copy).hash }, 0, "not carried over");
        assert_eq!(unsafe { LLString::hash(copy) }, hash, "same bytes, so same value");

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(!ll_release(s as *mut RcHeader));
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// The reason the category is read before the count: retain and
    /// release return early on an immortal entity, so its count sits at 1
    /// forever. Read as "sole owner", that would rewrite an interned name
    /// shared by the whole process.
    #[test]
    fn an_immortal_string_separates_although_its_count_says_one() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let name = crate::intern::intern_str("Order") as *mut LLString;
        assert_eq!(unsafe { (*name).rc.refcount }, 1);

        let copy = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, name as *mut RcHeader)
        } as *mut LLString;
        assert_ne!(copy as usize, name as usize);
        assert_eq!(unsafe { LLString::bytes(copy) }, b"Order");
        assert_eq!(
            unsafe { (*copy).rc.memory_category() },
            MemoryCategory::GcHeap,
            "a heap holder gets a heap copy"
        );
        assert_eq!(unsafe { LLString::bytes(name) }, b"Order", "the name is intact");

        // The same interned name, written through an arena holder: the
        // copy is a bump in the arena the reset reclaims, not a heap
        // allocation with a release-at-reset record behind it.
        let local = unsafe {
            crate::object::ll_cow_separate(
                &mut ctx,
                MemoryCategory::RequestArena,
                name as *mut RcHeader,
            )
        } as *mut LLString;
        assert_eq!(
            unsafe { (*local).rc.memory_category() },
            MemoryCategory::RequestArena,
            "the holder's category decides, not the original's"
        );

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// A long-lived string separates although its count is maintained
    /// and may read as one. `values.md` justifies the category test with
    /// "the count is pinned", which is true of immortal and false here —
    /// `ll_retain` takes neither early return for a COW entity. The
    /// reasons that do hold: the count is non-atomic while the entity is
    /// reachable from more than one request, and `string_die` frees only
    /// `GcHeap`, so an in-place write would land somewhere nothing
    /// reclaims.
    #[test]
    fn a_long_lived_string_separates_although_its_count_is_real() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new(&mut ctx, MemoryCategory::LongLived, b"cached") };
        assert_eq!(unsafe { (*s).rc.refcount }, 1);
        unsafe { crate::refcount::ll_retain(s as *mut RcHeader) };
        assert_eq!(
            unsafe { (*s).rc.refcount },
            2,
            "counted, unlike an immortal entity"
        );
        unsafe { assert!(!ll_release(s as *mut RcHeader)) };

        let copy = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, s as *mut RcHeader)
        } as *mut LLString;
        assert_ne!(copy as usize, s as usize, "sole holder and still a copy");
        assert_eq!(unsafe { LLString::bytes(copy) }, b"cached");
        assert_eq!(
            unsafe { (*copy).rc.memory_category() },
            MemoryCategory::GcHeap
        );

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// The two branches no entity in the crate can currently reach, so
    /// they are checked on the predicate rather than on a fabricated
    /// header: flipping `COW` on a live string would break the design's
    /// central invariant (the flag is the layout, set at allocation and
    /// never changed) and, under `rc-walk`, race the collector's byte
    /// stores.
    ///
    /// **`IS_ESCAPEE`**: `barrier::escape_gain` overwrites the count with
    /// the escape hold-count and asserts the entity is not COW, so a COW
    /// escapee cannot be produced today — the arena design routes value-
    /// like data through a deep copy at the barrier instead, and that is
    /// unbuilt. The arm stays because the rule has it and because the
    /// hold-count is genuinely not a reference count.
    ///
    /// **`COW = 0`**: the dynamic layout has no constructor yet.
    #[test]
    fn the_rule_reads_the_flag_before_the_category_and_the_count() {
        use crate::refcount::{IS_ESCAPEE, cow_separation_needed};
        let cow = COW | MemoryCategory::GcHeap as u32;

        assert!(!cow_separation_needed(cow, 1), "sole owner writes in place");
        assert!(cow_separation_needed(cow, 2), "a second holder copies");
        assert!(
            cow_separation_needed(cow | IS_ESCAPEE, 1),
            "an escapee copies: that 1 is a hold-count, not a reference count"
        );
        assert!(
            cow_separation_needed(COW | MemoryCategory::Immortal as u32, 1),
            "immortal copies at any count"
        );
        assert!(
            cow_separation_needed(COW | MemoryCategory::LongLived as u32, 1),
            "long-lived copies at any count"
        );

        // Not COW: outside the rule entirely, whatever else is set.
        for flags in [
            MemoryCategory::GcHeap as u32,
            MemoryCategory::Immortal as u32,
            MemoryCategory::RequestArena as u32 | IS_ESCAPEE,
        ] {
            assert!(
                !cow_separation_needed(flags, 9),
                "a non-COW entity is never copied by the write barrier"
            );
        }
    }

    /// The second layout's offsets, and the half of them it shares with
    /// the first: `len` at +8 and `hash` at +16 in both, which is what
    /// lets either be read without deciding which layout this is.
    #[test]
    fn the_dynamic_layout_shares_the_offsets_that_matter() {
        assert_eq!(std::mem::offset_of!(LLStringDynamic, rc), 0);
        assert_eq!(
            std::mem::offset_of!(LLStringDynamic, len),
            std::mem::offset_of!(LLString, len)
        );
        assert_eq!(std::mem::offset_of!(LLStringDynamic, capacity), 12);
        assert_eq!(
            std::mem::offset_of!(LLStringDynamic, hash),
            std::mem::offset_of!(LLString, hash)
        );
        assert_eq!(std::mem::offset_of!(LLStringDynamic, data), 24);
        assert_eq!(size_of::<LLStringDynamic>(), 32);
    }

    #[test]
    fn a_dynamic_heap_string_holds_its_bytes_out_of_line() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"grows", 0) };
        assert!(!s.is_null());

        let rc = unsafe { &(*s).rc };
        assert_eq!(rc.flags & ENTITY_KIND_MASK, EntityKind::String.to_flags());
        assert_eq!(rc.flags & COW, 0, "the dynamic layout is the non-COW form");
        assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"grows");
        assert_eq!(
            unsafe { string_bytes(s as *const LLString) },
            b"grows",
            "and the layout-agnostic accessor agrees"
        );
        assert!(
            unsafe { (*s).capacity } >= 5,
            "the payload is allocated with its own capacity"
        );
        assert!(
            !unsafe { (*s).data }.is_null()
                && unsafe { (*s).data } as usize != s as usize + 24,
            "out of line, not inline"
        );

        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// A dynamic string is outside the COW rule, so an append writes in
    /// place with no sharing test — even with a second holder, which for
    /// an inline string would force a copy.
    #[test]
    fn an_append_grows_in_place_and_drops_the_cached_hash() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"one", 0) };
        let hash = unsafe { LLString::hash(s as *mut LLString) };
        assert_ne!(hash, 0);
        unsafe { crate::refcount::ll_retain(s as *mut RcHeader) };

        assert!(unsafe { ll_string_append(&mut ctx, s, b"-two") });
        assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"one-two");
        assert_eq!(unsafe { (*s).len }, 7);
        assert_eq!(unsafe { (*s).hash }, 0, "the old hash is not the new bytes'");
        assert_eq!(
            unsafe { LLString::hash(s as *mut LLString) },
            hash_bytes(b"one-two"),
            "and recomputing gives the new content's — asserting merely \
             that it differs would pass on a hash of the payload address"
        );
        assert_ne!(hash_bytes(b"one-two"), hash);

        // Growth past the initial capacity: the payload may move, the
        // entity may not.
        let address = s as usize;
        let long = vec![b'x'; 4096];
        assert!(unsafe { ll_string_append(&mut ctx, s, &long) });
        assert_eq!(unsafe { (*s).len } as usize, 7 + 4096);
        assert_eq!(unsafe { LLStringDynamic::bytes(s) }[..7], *b"one-two");
        assert_eq!(s as usize, address, "the entity never moves");

        unsafe {
            assert!(!ll_release(s as *mut RcHeader));
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// An arena dynamic string takes its payload from the arena, so the
    /// reset reclaims both halves and teardown must not hand the payload
    /// to the long-lived free routine — a block of the wrong kind.
    #[test]
    fn an_arena_dynamic_string_leaves_both_halves_to_the_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s =
            unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::RequestArena, b"scoped", 0) };
        assert_eq!(
            unsafe { (*s).rc.memory_category() },
            MemoryCategory::RequestArena
        );
        assert!(unsafe { ll_string_append(&mut ctx, s, b" and grown") });
        assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"scoped and grown");

        let payload = unsafe { (*s).data };
        unsafe { string_die(s as *mut LLString) };
        // The payload is still the arena's, and still intact. Handing it
        // to the long-lived free routine instead would have written a
        // free-list link — `{ next, size }`, 16 bytes — over the front of
        // it, so reading the content back is what catches that.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(payload, 16) },
            b"scoped and grown",
            "an arena payload belongs to the reset, and teardown left it alone"
        );
        arena.reset(|_| {});
    }

    /// The hash is a function of the content and of nothing else, so the
    /// two layouts holding the same bytes hash the same. Computing it
    /// through the inline accessor on a dynamic string would hash the
    /// `data` field — an address — and this is the assertion that says so.
    #[test]
    fn the_two_layouts_hash_the_same_content_the_same() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let content = b"a string long enough to reach past the fixed fields";

        let inline = unsafe { ll_string_new(&mut ctx, MemoryCategory::GcHeap, content) };
        let dynamic =
            unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, content, 0) };
        assert_eq!(
            unsafe { LLString::hash(inline) },
            unsafe { LLString::hash(dynamic as *mut LLString) },
            "same bytes, same hash, whichever layout holds them"
        );
        assert_eq!(unsafe { LLString::hash(inline) }, hash_bytes(content));

        // Two dynamic strings with equal content agree as well — they
        // would not if the hash were taken over the payload pointer.
        let other =
            unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, content, 0) };
        assert_ne!(unsafe { (*other).data }, unsafe { (*dynamic).data });
        assert_eq!(
            unsafe { LLString::hash(other as *mut LLString) },
            unsafe { LLString::hash(dynamic as *mut LLString) }
        );

        unsafe {
            for p in [
                inline as *mut RcHeader,
                dynamic as *mut RcHeader,
                other as *mut RcHeader,
            ] {
                assert!(ll_release(p));
                crate::object::ll_entity_die(p);
            }
        }
        arena.reset(|_| {});
    }

    /// The accumulator: `$s = ""` and then appends. An empty dynamic
    /// string has no payload at all, so every read of it has to answer
    /// without dereferencing `data` — `slice::from_raw_parts` requires a
    /// non-null pointer even for a zero-length slice.
    #[test]
    fn an_empty_dynamic_string_has_no_payload_and_still_reads() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"", 0) };
        assert!(!s.is_null());
        assert!(unsafe { (*s).data }.is_null(), "nothing was allocated");
        assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"");
        assert_eq!(unsafe { string_bytes(s as *const LLString) }, b"");
        assert_eq!(unsafe { LLString::hash(s as *mut LLString) }, hash_bytes(b""));

        assert!(unsafe { ll_string_append(&mut ctx, s, b"first") });
        assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"first");

        // With a hint, the payload is there from the start — that is what
        // the hint is for, and the empty case is where it matters most.
        let hinted = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"", 4096) };
        assert!(!unsafe { (*hinted).data }.is_null());
        assert!(unsafe { (*hinted).capacity } >= 4096);
        assert_eq!(unsafe { (*hinted).len }, 0);

        unsafe {
            for p in [s as *mut RcHeader, hinted as *mut RcHeader] {
                assert!(ll_release(p));
                crate::object::ll_entity_die(p);
            }
        }
        arena.reset(|_| {});
    }

    /// The two categories a dynamic string may not have are refused, not
    /// redirected. A debug-only check would vanish in release into the
    /// heap arm and put an immortal-flagged entity in a GC entity block:
    /// walked by the census, never released, pinned for the life of the
    /// process.
    #[test]
    fn a_dynamic_string_refuses_the_categories_it_cannot_live_in() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        for category in [MemoryCategory::Immortal, MemoryCategory::LongLived] {
            assert!(
                unsafe { ll_string_new_dynamic(&mut ctx, category, b"no", 0) }.is_null(),
                "the mutable layout is heap or arena only"
            );
        }
        arena.reset(|_| {});
    }

    /// An arena dynamic string may not escape into a longer-lived
    /// holder. Promotion keeps the entity and rewrites its category but
    /// knows nothing about the payload, which stays arena memory — the
    /// block goes home at the reset and is later re-stamped as a buffer
    /// block, so the survivor reads recycled memory and its eventual free
    /// routes into an arena that never granted the chunk. Refused at the
    /// escape, not documented, until promotion carries the payload.
    #[test]
    #[should_panic(expected = "cannot escape its arena")]
    fn an_arena_dynamic_string_may_not_escape() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe {
            ll_string_new_dynamic(&mut ctx, MemoryCategory::RequestArena, b"accumulator", 0)
        };
        let mut heap_slot: *mut RcHeader = std::ptr::null_mut();
        unsafe {
            crate::memory::barrier::store_ptr(
                &raw mut arena,
                MemoryCategory::GcHeap,
                &raw mut heap_slot,
                s as *mut RcHeader,
            )
        };
    }

    /// Teardown returns a heap dynamic string's payload to the arena it
    /// came from, and this is the assertion that says so — deleting the
    /// payload half of `string_die` leaves every other test in this file
    /// green. The proof is the buffer arena's own: in critical mode a
    /// freed chunk goes on the block's free list and a fitting allocation
    /// finds it, so the same address coming back means the chunk was
    /// really returned.
    #[test]
    fn teardown_returns_a_heap_payload_to_the_buffer_arena() {
        use crate::memory::buffer::{PressureMode, set_pressure_mode};
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let content = vec![b'p'; 64];
        let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, &content, 0) };
        let payload = unsafe { (*s).data };
        let capacity = unsafe { (*s).capacity } as usize;
        assert!(!payload.is_null());

        unsafe {
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }

        set_pressure_mode(PressureMode::Critical);
        let (reused, _) =
            crate::memory::buffer_arena::with_buffer_arena(|a| a.alloc(capacity));
        set_pressure_mode(PressureMode::Plenty);
        assert_eq!(
            reused, payload,
            "the payload was not returned: the free list has no chunk of that size"
        );

        crate::memory::buffer_arena::with_buffer_arena(|a| unsafe { a.free(reused, capacity) });
        arena.reset(|_| {});
    }

    #[test]
    fn an_append_past_the_cap_is_refused_with_the_string_untouched() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, b"near", 0) };
        // Claim a length just under the cap without owning the bytes:
        // the gate is arithmetic on `len`, and this is the only way to
        // reach it without four gigabytes.
        unsafe { (*s).len = MAX_LEN as u32 - 1 };
        assert!(
            !unsafe { ll_string_append(&mut ctx, s, b"xx") },
            "4 GiB is a refusal, not a truncation"
        );
        assert_eq!(unsafe { (*s).len }, MAX_LEN as u32 - 1, "untouched");

        unsafe {
            (*s).len = 4;
            assert!(ll_release(s as *mut RcHeader));
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    #[test]
    fn content_past_the_cap_is_refused_rather_than_truncated() {
        assert!(fits(MAX_LEN));
        assert!(!fits(MAX_LEN + 1));
    }
}
