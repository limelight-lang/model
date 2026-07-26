//! The store barrier, as micro-operations (`rfc/model/gc/strategies.md`
//! §1). A reference store is not one hook: it is composed from small
//! operations the compiler picks per site and inlines, specialized to the
//! slot's kind and to a compile-time-constant `owner_cat`. This crate
//! provides the pieces; the *composition* — which ops, in what order, with
//! which checks elided — is lowering's, and the runtime never sees it.
//!
//! - [`store_ptr`] / [`store_box`] — **publish** a reference into an
//!   8-byte pointer slot or a 16-byte `Value` slot: retain, category
//!   barrier, write. No release: an initializing store is `store_*` alone.
//! - [`drop_ref`] — **drop** the entity a slot held (overwrite, clear, or
//!   holder teardown): release, cascade. Independent of the slot's kind.
//!   An overwriting store is `store_*` then `drop_ref`, in that order.
//! - [`ref_store`] — the convenience composition of `store_box` +
//!   `drop_ref` for a Box-slot overwrite, kept for callers holding an
//!   owner header and a whole `Value`.
//!
//! Composition in this build (phase 1): RC operations + the category
//! barrier (`rfc/model/memory/arenas.md`). Strategy hooks (SATB) plug
//! into `drop_ref` later (A5).
//!
//! `owner_cat` is a **parameter, not a load from the owner**: the compiler
//! knows the destination's category, so it is passed. A slot has no header
//! of its own, which is why this works even for a headerless destination
//! (a static block, A6). Which arena's logs to write is answered by the
//! context: one mounted arena per executing context, kept correct by the
//! compiler.
//!
//! Category sources: the stored value carries its own memory category as
//! 2 bits in its `RcHeader` flags, stamped at allocation; the
//! destination's is `owner_cat`, supplied by the caller. Actor arenas are
//! unreachable from outside, so a store creating an escape always runs
//! with the owner's arena mounted.

use crate::memory::arena::Arena;
use crate::memory::context::{LLContext, resolve_arena};
use crate::refcount::{COW, IS_ESCAPEE, MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::value::Value;

/// A longer-lived container took a reference to request-arena object
/// `entity` (the **gain** of the escape rule, `rfc/model/memory/arenas.md`).
/// Bump its escape hold-count; on the 0 → 1 transition it becomes an
/// escapee and joins the arena's list. The count lives in `entity`'s
/// otherwise-idle `refcount` ([`IS_ESCAPEE`]), so reset decides its fate
/// from the count alone and never dereferences a holder slot.
///
/// # Safety
/// `entity` must be a live request-arena entity.
pub(crate) unsafe fn escape_gain(arena: *mut Arena, entity: *mut RcHeader) {
    let e = unsafe { &mut *entity };
    debug_assert!(
        e.flags & COW == 0,
        "COW arena value escape takes the deepCopy path, not the counter (deferred)"
    );
    if e.flags & IS_ESCAPEE == 0 {
        e.flags |= IS_ESCAPEE;
        e.refcount = 1;
        unsafe { (*arena).log_escapee(entity) };
    } else {
        // Same field, same arithmetic, so the same guard as `ll_retain`:
        // a wrapped hold-count would make reset believe every holder let
        // go and drop a still-held escapee.
        #[cfg(feature = "checked-refcount")]
        if e.refcount == u32::MAX {
            return;
        }
        e.refcount += 1;
    }
}

/// A longer-lived slot let go of request-arena escapee `entity`: either
/// overwritten with another value, or its whole holder torn down. Those
/// are the same **lose** event. Drop the hold-count; at zero it is no
/// longer an escapee. No arena handle needed — the list is append-only and
/// reset skips a zero count.
///
/// # Safety
/// `entity` must be a live request-arena entity.
pub(crate) unsafe fn escape_lose(entity: *mut RcHeader) {
    let e = unsafe { &mut *entity };
    if e.flags & IS_ESCAPEE == 0 {
        return; // not tracked (never gained, or already back to zero)
    }
    debug_assert!(e.refcount > 0, "escape hold-count underflow");
    e.refcount -= 1;
    if e.refcount == 0 {
        e.flags &= !IS_ESCAPEE;
    }
}

/// The **category barrier** for a newly-published entity: the cross-arena
/// escape bookkeeping shared by [`store_ptr`] and [`store_box`]. `new` is
/// the entity the slot now references (already retained); `owner_cat` is
/// the destination's memory category.
///
/// Two directions, both keyed only on categories, never on reading a slot:
/// an arena entity into a longer-lived owner counts the escape (`gain`); a
/// heap entity into an arena owner records a release the reset owns.
///
/// # Safety
/// `new` a live entity; `arena` the live mounted arena.
#[inline]
unsafe fn store_category_barrier(
    arena: *mut Arena,
    owner_cat: MemoryCategory,
    new: *mut RcHeader,
) {
    let new_cat = unsafe { (*new).memory_category() };

    // Dangerous direction: an arena reference stored into a longer-lived
    // container would dangle after reset. Count the escape (`gain`); its
    // fate is decided at arena death from the count, never by reading the
    // slot back.
    if new_cat == MemoryCategory::RequestArena && owner_cat != MemoryCategory::RequestArena {
        unsafe { escape_gain(arena, new) };
    }

    // Reverse direction: a heap entity stored into an arena container would
    // leak (reset skips per-object drop). The log owns exactly one release
    // per record.
    if new_cat == MemoryCategory::GcHeap && owner_cat == MemoryCategory::RequestArena {
        unsafe { (*arena).log_release_at_reset(new) };
    }
}

/// The `store_ptr` micro-op (`rfc/model/gc/strategies.md` §1): **publish** a
/// reference into a bare 8-byte pointer slot — retain, category barrier,
/// write. Publish only: an initializing store is `store_ptr` alone, an
/// overwriting one is `store_ptr` then [`drop_ref`] (publish before release,
/// audit C1). `owner_cat` is a parameter, not a load from an owner header,
/// so a headerless destination (a static block, A6) can be a slot. A `NULL`
/// `new` (clearing the slot) just writes, nothing to retain or note.
///
/// # Safety
/// `slot` a live 8-byte pointer slot; `new` null or a live entity; `arena`
/// the live mounted arena; `owner_cat` the slot owner's category.
pub(crate) unsafe fn store_ptr(
    arena: *mut Arena,
    owner_cat: MemoryCategory,
    slot: *mut *mut RcHeader,
    new: *mut RcHeader,
) {
    if !new.is_null() {
        unsafe { ll_retain(new) };
        unsafe { store_category_barrier(arena, owner_cat, new) };
    }
    unsafe { slot.write(new) };
}

/// The `store_box` micro-op: the same publish for a 16-byte `Value` slot.
///
/// **The whole `Value` is written**, not just the payload word — a torn
/// "new pointer, old tag" slot is a crash for a reentrant reader, and one
/// writer for one slot is the right rule. Publish only, like [`store_ptr`];
/// the displaced value is released by [`drop_ref`]. A non-entity `new`
/// (int, bool, null) counts as no entity: nothing retained or noted.
///
/// # Safety
/// `slot` a live `Value` slot; `new`'s entity null or live; `arena` the
/// live mounted arena; `owner_cat` the slot owner's category.
pub(crate) unsafe fn store_box(
    arena: *mut Arena,
    owner_cat: MemoryCategory,
    slot: *mut Value,
    new: Value,
) {
    let new_ptr = if new.is_refcounted() {
        new.entity_ptr()
    } else {
        std::ptr::null_mut()
    };
    if !new_ptr.is_null() {
        unsafe { ll_retain(new_ptr) };
        unsafe { store_category_barrier(arena, owner_cat, new_ptr) };
    }
    unsafe { slot.write(new) };
}

/// The `drop` micro-op: **release** the entity a slot held after an
/// overwriting or clearing store, or when the whole holder is torn down.
/// It takes the displaced *entity*, not the slot, so it is independent of
/// the slot's kind — one `drop` serves both `store_ptr` and `store_box`.
///
/// It mirrors the category barrier in reverse: a longer-lived slot letting
/// go of an arena escapee drops its hold-count (`lose`); a heap value
/// displaced from an arena container is **not** released here (its
/// release-at-reset record owns that release — releasing twice was the
/// double-release bug the design fixes); everything else releases and, if
/// that was the last reference, cascades into teardown. Teardown runs
/// `__destruct`, which reenters the runtime; it resolves its own arena, so
/// `drop_ref` needs none.
///
/// # Safety
/// `old` null or the live entity the slot held; `owner_cat` the slot
/// owner's category.
pub(crate) unsafe fn drop_ref(owner_cat: MemoryCategory, old: *mut RcHeader) {
    if old.is_null() {
        return;
    }
    let old_cat = unsafe { (*old).memory_category() };

    // A longer-lived slot letting go of an arena escapee: drop its
    // hold-count (`lose`). The barrier half of the escape rule; holder
    // teardown is the other half, and it is the same event.
    if old_cat == MemoryCategory::RequestArena && owner_cat != MemoryCategory::RequestArena {
        unsafe { escape_lose(old) };
    }

    // A displaced heap value in an arena container is NOT released here:
    // its own release-at-reset record owns that release.
    let old_is_log_owned =
        owner_cat == MemoryCategory::RequestArena && old_cat == MemoryCategory::GcHeap;

    if !old_is_log_owned && unsafe { ll_release(old) } {
        // Last reference gone: tear it down (destructor, release children,
        // free), dispatched on the entity kind (`ll_entity_die`) — an
        // object through its class's dispose, a reference box by
        // releasing its Value.
        unsafe { crate::object::ll_entity_die(old) };
    }
}

/// The store mechanics for a 16-byte Box slot, as the convenience
/// composition of the micro-ops: [`store_box`] to publish, then
/// [`drop_ref`] of the displaced entity. The compiler emits the micro-ops
/// directly and specialized (`rfc/model/gc/strategies.md` §1); this entry
/// keeps the composed overwriting-store shape for callers holding an owner
/// header and a whole `Value`, reading `owner_cat` from the owner.
///
/// `owner` is the entity containing `slot`; `old` is the slot's current
/// entity (null for a non-entity); `new` is the whole `Value` being
/// stored. Publish precedes drop (audit C1: the slot must not still point
/// at `old` when its teardown runs and may collect).
///
/// `arena` is a raw pointer, not `&mut Arena`: `drop_ref`'s teardown runs
/// `__destruct`, which reenters the runtime and resolves this same arena.
/// Holding an exclusive borrow across that call would alias (audit H5).
///
/// # Safety
/// `owner` a live entity containing `slot`; `old` the entity the slot
/// currently holds (null when a non-entity); `old`/`new`'s entity each
/// null or live; `arena` the live mounted arena.
pub unsafe fn ref_store(
    arena: *mut Arena,
    owner: *mut RcHeader,
    slot: *mut Value,
    old: *mut RcHeader,
    new: Value,
) {
    debug_assert!(!owner.is_null(), "a slot always has an owner");
    debug_assert_eq!(
        {
            let held = unsafe { slot.read() };
            if held.is_refcounted() {
                held.entity_ptr()
            } else {
                std::ptr::null_mut()
            }
        },
        old,
        "old must be the entity the slot holds"
    );

    let owner_cat = unsafe { (*owner).memory_category() };
    unsafe { store_box(arena, owner_cat, slot, new) };
    unsafe { drop_ref(owner_cat, old) };
}

/// The unified store barrier (`rfc/model/gc/strategies.md`): the single
/// hook through which generated code performs a reference store it
/// could not resolve statically.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`]; the rest per
/// [`ref_store`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_ref_store(
    ctx: *mut LLContext,
    owner: *mut RcHeader,
    slot: *mut Value,
    old: *mut RcHeader,
    new: Value,
) {
    unsafe { ref_store(resolve_arena(ctx), owner, slot, old, new) }
}

/// C ABI: the `store_ptr` micro-op. `owner_cat` crosses as a plain `u32`
/// — the 2-bit category, a compile-time constant at the call site — masked
/// to a valid category rather than trusted as an enum bit pattern (as in
/// [`crate::object::ll_object_new_abi`]).
///
/// # Safety
/// As [`store_ptr`]; `owner_cat` a valid `MemoryCategory` code (`0..=3`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_store_ptr(
    ctx: *mut LLContext,
    owner_cat: u32,
    slot: *mut *mut RcHeader,
    new: *mut RcHeader,
) {
    unsafe { store_ptr(resolve_arena(ctx), MemoryCategory::from_flags(owner_cat), slot, new) }
}

/// C ABI: the `store_box` micro-op.
///
/// # Safety
/// As [`store_box`]; `owner_cat` a valid `MemoryCategory` code (`0..=3`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_store_box(
    ctx: *mut LLContext,
    owner_cat: u32,
    slot: *mut Value,
    new: Value,
) {
    unsafe { store_box(resolve_arena(ctx), MemoryCategory::from_flags(owner_cat), slot, new) }
}

/// C ABI: the `drop` micro-op. `ctx` is unused in this composition — a
/// `drop` needs no arena — but is kept in the signature, reserved for the
/// SATB strategy hook that plugs into `drop` (A5).
///
/// # Safety
/// As [`drop_ref`]; `owner_cat` a valid `MemoryCategory` code (`0..=3`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_drop(ctx: *mut LLContext, owner_cat: u32, old: *mut RcHeader) {
    let _ = ctx;
    unsafe { drop_ref(MemoryCategory::from_flags(owner_cat), old) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(cat: MemoryCategory) -> RcHeader {
        RcHeader::new(cat, 0)
    }

    /// A one-slot container: header + slot, like a minimal object.
    struct Holder {
        header: RcHeader,
        slot: Value,
    }

    impl Holder {
        fn new(cat: MemoryCategory) -> Self {
            Holder {
                header: entity(cat),
                slot: Value::null(),
            }
        }

        fn entity_ptr(&self) -> *mut RcHeader {
            if self.slot.is_refcounted() {
                self.slot.entity_ptr()
            } else {
                std::ptr::null_mut()
            }
        }

        unsafe fn store(&mut self, arena: &mut Arena, new: *mut RcHeader) {
            let old = self.entity_ptr();
            let value = if new.is_null() {
                Value::null()
            } else {
                Value::entity(crate::value::Tag::Object, new)
            };
            unsafe { ref_store(arena, &mut self.header, &mut self.slot, old, value) };
        }
    }

    #[test]
    fn heap_to_heap_counts_and_writes_slot() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut owner = Holder::new(MemoryCategory::GcHeap);
        let mut a = entity(MemoryCategory::GcHeap);
        let mut b = entity(MemoryCategory::GcHeap);
        // One pointer per entity, taken once: a second `&mut a` would
        // retag and invalidate the copy the slot is holding.
        let (pa, pb): (*mut RcHeader, *mut RcHeader) = (&mut a, &mut b);

        unsafe { owner.store(&mut arena, pa) };
        assert_eq!(owner.entity_ptr(), pa);
        assert_eq!(unsafe { (*pa).refcount }, 2, "initial + the slot's reference");

        unsafe { owner.store(&mut arena, pb) };
        assert_eq!(
            unsafe { (*pa).refcount },
            1,
            "displaced from a heap slot: released now"
        );
        assert_eq!(unsafe { (*pb).refcount }, 2);
    }

    /// The pointer-slot analog, driven by the micro-ops directly: an
    /// 8-byte `*mut RcHeader` slot published by `store_ptr` (no drop on an
    /// initializing store), then an overwrite as `store_ptr` + `drop_ref`.
    #[test]
    fn store_ptr_publishes_a_pointer_slot_then_drop_releases_the_old() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut a = entity(MemoryCategory::GcHeap);
        let mut b = entity(MemoryCategory::GcHeap);
        let (pa, pb): (*mut RcHeader, *mut RcHeader) = (&mut a, &mut b);
        let mut slot: *mut RcHeader = std::ptr::null_mut();

        // Initializing store: publish only, no old to drop.
        unsafe { store_ptr(&mut arena, MemoryCategory::GcHeap, &mut slot, pa) };
        assert_eq!(slot, pa, "slot published as a bare 8-byte pointer");
        assert_eq!(unsafe { (*pa).refcount }, 2, "initial + the slot's reference");

        // Overwriting store: publish the new pointer, then drop the old.
        let old = slot;
        unsafe { store_ptr(&mut arena, MemoryCategory::GcHeap, &mut slot, pb) };
        unsafe { drop_ref(MemoryCategory::GcHeap, old) };
        assert_eq!(slot, pb);
        assert_eq!(unsafe { (*pa).refcount }, 1, "displaced from a heap slot: released");
        assert_eq!(unsafe { (*pb).refcount }, 2);
    }

    /// `owner_cat` is passed, not read from an owner header — so a
    /// headerless destination (a static block, A6) is a valid store target
    /// and still gets the escape barrier. A long-lived slot taking an arena
    /// reference counts the escape exactly as a heap owner would, with no
    /// owner entity anywhere.
    #[test]
    fn owner_cat_parameter_drives_the_escape_barrier_without_an_owner_header() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let obj = arena.alloc(16) as *mut RcHeader;
        unsafe { obj.write(entity(MemoryCategory::RequestArena)) };
        let mut slot: *mut RcHeader = std::ptr::null_mut();

        unsafe { store_ptr(&mut arena, MemoryCategory::LongLived, &mut slot, obj) };
        assert_ne!(
            unsafe { (*obj).flags } & IS_ESCAPEE,
            0,
            "arena ref escaped into a long-lived slot"
        );
        assert_eq!(unsafe { (*obj).refcount }, 1, "one holder, counted in the escapee");

        let mut escapees = Vec::new();
        arena.reset_with(|_| {}, |e| escapees.push(e));
        assert_eq!(escapees, vec![obj], "the escapee itself, no slot dereferenced");
    }

    #[test]
    fn arena_ref_into_heap_owner_is_recorded_as_an_escapee() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut owner = Holder::new(MemoryCategory::GcHeap);

        // The escapee lives in real arena memory, as it would in life.
        let obj = arena.alloc(16) as *mut RcHeader;
        unsafe { obj.write(entity(MemoryCategory::RequestArena)) };

        unsafe { owner.store(&mut arena, obj) };
        // The escape is counted in the entity itself (the IS_ESCAPEE
        // hold-count), not by remembering the holder's slot.
        assert_eq!(unsafe { (*obj).refcount }, 1, "one heap holder");
        assert_ne!(
            unsafe { (*obj).flags } & crate::refcount::IS_ESCAPEE,
            0,
            "marked as an escapee"
        );

        // Reset sees the escapee entity directly — no slot is dereferenced.
        let mut escapees = Vec::new();
        arena.reset_with(|_| {}, |e| escapees.push(e));
        assert_eq!(escapees, vec![obj], "the escapee itself, not its slot");
    }

    #[test]
    fn heap_ref_into_arena_owner_defers_all_releases_to_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut owner = Holder::new(MemoryCategory::RequestArena);
        let mut a = entity(MemoryCategory::GcHeap);
        let mut b = entity(MemoryCategory::GcHeap);

        unsafe { owner.store(&mut arena, &mut a) };
        assert_eq!(a.refcount, 2);

        // Overwrite: A must NOT be released here — its log record owns
        // the release.
        unsafe { owner.store(&mut arena, &mut b) };
        assert_eq!(a.refcount, 2, "no release on overwrite in an arena slot");
        assert_eq!(b.refcount, 2);

        // Store A again: a second retain and a second log record.
        unsafe { owner.store(&mut arena, &mut a) };
        assert_eq!(a.refcount, 3);
        assert_eq!(b.refcount, 2);

        // Reset releases once per record: A twice, B once. Balanced.
        arena.reset(|_| {});
        assert_eq!(a.refcount, 1);
        assert_eq!(b.refcount, 1);
    }

    #[test]
    fn storing_null_clears_the_slot_without_double_release() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut owner = Holder::new(MemoryCategory::RequestArena);
        let mut a = entity(MemoryCategory::GcHeap);

        unsafe { owner.store(&mut arena, &mut a) };
        unsafe { owner.store(&mut arena, std::ptr::null_mut()) };
        assert!(owner.entity_ptr().is_null());
        assert_eq!(a.refcount, 2, "the log still owns A's release");

        arena.reset(|_| {});
        assert_eq!(a.refcount, 1, "exactly one release, from the log");
    }

    /// Audit C1: the displaced value's `__destruct` is user code and may
    /// collect. If the slot still pointed at the value being torn down,
    /// the collector would walk an edge the refcount has already given up.
    ///
    /// The damage needs the owner to be garbage itself: then nothing
    /// restores the subtracted count, the dying value goes white with it,
    /// and `collect_white` frees it **while its own teardown is running** —
    /// a free of memory the caller is still inside, followed by a second
    /// free when teardown finishes. Publishing the slot first removes the
    /// edge, so only the owner's genuine garbage is collected.
    #[test]
    fn a_collecting_destructor_cannot_see_the_slot_it_is_being_removed_from() {
        use crate::class::ClassBuilder;
        use crate::gc::{ll_gc_collect_cycles, set_test_threshold};
        use crate::memory::context::LLContext;
        use crate::object::{Object, new_constructed};
        use crate::value::{Tag, Value};

        static COLLECTED: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(usize::MAX);

        unsafe extern "C" fn collect_from_destructor(_obj: *mut Object) {
            let n = unsafe { ll_gc_collect_cycles() };
            COLLECTED.store(n, std::sync::atomic::Ordering::Relaxed);
        }

        let _g = crate::memory::block_pool::test_guard();
        let owner_cls = ClassBuilder::new("C1Owner")
            .prop("next", true)
            .prop("mine", true)
            .build();
        let dying_cls = ClassBuilder::new("C1Dying")
            .destructor(collect_from_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        unsafe {
            let owner = new_constructed(&mut ctx, owner_cls, MemoryCategory::GcHeap);
            let old = new_constructed(&mut ctx, dying_cls, MemoryCategory::GcHeap);
            let next = Object::prop_at(owner, 16);
            let mine = Object::prop_at(owner, 32);

            // owner --mine--> owner: a self-cycle, so the owner is garbage
            // held up only by its own edge.
            ref_store(
                &mut arena,
                owner as *mut RcHeader,
                mine,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, owner as *mut RcHeader),
            );
            // owner --next--> old, then drop the creation reference: the
            // slot holds the only one left.
            ref_store(
                &mut arena,
                owner as *mut RcHeader,
                next,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, old as *mut RcHeader),
            );
            assert!(!ll_release(old as *mut RcHeader));

            // Drop the owner's external reference too: now it is a
            // buffered candidate root, and the collection the destructor
            // starts will trace it — through `next`, if the old value is
            // still visible there.
            set_test_threshold(usize::MAX); // arm nothing, fire only from the destructor
            assert!(!ll_release(owner as *mut RcHeader));

            // Displaces `old`: last reference gone, teardown runs, the
            // destructor collects from inside it.
            ref_store(
                &mut arena,
                owner as *mut RcHeader,
                next,
                old as *mut RcHeader,
                Value::null(),
            );

            // The collection reclaimed the owner's self-cycle and nothing
            // else. Two means it also took `old`, whose teardown was on the
            // stack at that moment.
            assert_eq!(
                COLLECTED.load(std::sync::atomic::Ordering::Relaxed),
                1,
                "the dying value must not be visible to a collection inside its own destructor"
            );
            set_test_threshold(crate::gc::CANDIDATE_THRESHOLD);
        }
        arena.reset(|_| {});
    }

    #[test]
    fn immortal_values_touch_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut owner = Holder::new(MemoryCategory::GcHeap);
        let mut s = entity(MemoryCategory::Immortal);

        unsafe { owner.store(&mut arena, &mut s) };
        assert_eq!(s.refcount, 1, "immortals are never counted");
        assert_eq!(owner.entity_ptr(), &mut s as *mut _);

        let mut escapes = 0;
        arena.reset_with(|_| {}, |_| escapes += 1);
        assert_eq!(escapes, 0, "no logs for immortal stores");
    }
}
