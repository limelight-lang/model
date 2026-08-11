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
//!   An overwriting store is `store_*` and then, **only if it returned
//!   true**, `drop_ref` — see the failure paragraph below: a refused
//!   store leaves the slot holding the old entity, so dropping it anyway
//!   dangles the slot.
//! - [`publish_child`] — the publish alone, for a holder whose slot this
//!   module does not write: an array's entry is written by
//!   `Table::insert` and its string key is no slot at all.
//! - [`ref_store`] — the convenience composition of `store_box` +
//!   `drop_ref` for a Box-slot overwrite, kept for callers holding an
//!   owner header and a whole `Value`.
//!
//! Composition in this build (phase 1): RC operations + the category
//! barrier (`rfc/model/memory/arenas.md`). Strategy hooks (SATB) plug
//! into `drop_ref` later (A5).
//!
//! **A publish can fail, and says so** (2026-08-04). `store_ptr`,
//! `store_box` and `ref_store` return whether the store happened, and
//! the one thing that can refuse is the deep copy a COW value takes when
//! it leaves the arena: the copy is an allocation. A refusal leaves the
//! slot and every count exactly as they were — which is exactly why the
//! `drop_ref` that would have followed must not run: the slot still holds
//! the old entity and still owns its reference. The caller's two duties
//! are therefore to skip the drop and to raise memory-exhausted, which
//! generated code will do through the exceptions runtime
//! (`rfc/runtime/exceptions.md`) once it exists. `drop_ref` itself cannot
//! fail and returns nothing.
//!
//! This is not the reserve's shape and could not be: the log reserve
//! funds the barrier's *own* allocation because a log record is
//! fixed-size, and a copy is the size of the value.
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
        "a COW value is copied out of the arena, never counted into it"
    );
    // The assert is an invariant now rather than a wish: the caller
    // ([`store_category_barrier`]) copies a COW entity instead of calling
    // this, so the hold-count and the exact COW count never claim the
    // same four bytes. A dynamic string reaches here and should: it is
    // the non-COW form, it has real identity, and promotion carries its
    // payload out of the arena (`promote::carry_external_memory`).
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
/// Returns **the entity the slot must hold**, which is `new` itself in
/// every case but one: a COW entity leaving the arena is copied, and the
/// copy is what the holder gets. Null means the copy could not be made
/// and the store must not happen.
///
/// The returned entity carries a reference for the slot either way — the
/// caller retains `new` before calling, and a copy arrives at `+1` from
/// its factory, so the caller releases `new` when they differ.
///
/// **Crate-internal, and publishing without writing a slot is exactly
/// what it is**: the caller stores what it returns. Four callers, and
/// two of them are not the store micro-ops — [`publish_child`], which is
/// this operation plus the reference it assumes, and
/// `array::element::box_element`, which already holds the box's factory
/// count and so reaches this directly. The array publishes every child
/// it takes through the two of them rather than by a bare `ll_retain`,
/// because a copy that records no escape gain spends a hold-count
/// belonging to a real holder when `drop_ref` gives it back.
///
/// # Safety
/// `new` a live entity; `arena` the live mounted arena.
#[inline]
pub(crate) unsafe fn store_category_barrier(
    arena: *mut Arena,
    owner_cat: MemoryCategory,
    new: *mut RcHeader,
) -> *mut RcHeader {
    let new_cat = unsafe { crate::object::header_category(new) };
    let mut stored = new;

    // Dangerous direction: an arena reference stored into a longer-lived
    // container would dangle after reset.
    if new_cat == MemoryCategory::RequestArena && owner_cat != MemoryCategory::RequestArena {
        if unsafe { crate::refcount::header_flags(new) } & COW != 0 {
            // A COW entity is value-like: its identity is not observable,
            // so the longer-lived holder takes a **copy** rather than a
            // hold on arena memory (`rfc/model/memory/arenas.md`, the deep
            // copy). That is also what keeps `IS_ESCAPEE` and the exact
            // COW count from claiming the same four bytes — a COW entity
            // never becomes an escapee at all.
            stored = unsafe { crate::object::escape_copy(arena, owner_cat, new) };
            if stored.is_null() {
                return std::ptr::null_mut();
            }

            // The copy is a fresh heap entity, so the reverse direction
            // below cannot apply to it: its owner is longer-lived by
            // construction.
            return stored;
        }

        // Count the escape (`gain`); its fate is decided at arena death
        // from the count, never by reading the slot back.
        unsafe { escape_gain(arena, new) };
    }

    // Reverse direction: a heap entity stored into an arena container would
    // leak (reset skips per-object drop). The log owns exactly one release
    // per record.
    if new_cat == MemoryCategory::GcHeap && owner_cat == MemoryCategory::RequestArena {
        unsafe { (*arena).log_release_at_reset(new) };
    }

    stored
}

/// **Publish** a value into a holder whose slot this operation does not
/// write: take the reference the holder will own, cross the category
/// barrier, and give back the value the holder must name. That is the
/// value passed in, except where an arena COW entity crosses into a
/// longer-lived holder — then it is the copy, under the original's tag. A
/// non-entity value (int, bool, null) comes back unchanged with nothing
/// retained.
///
/// `None` is the refused copy, the one failure the barrier has: nothing
/// was spent, `new` keeps the count it arrived with, and the caller must
/// store nothing.
///
/// The callers are the array's, where the slot is written by
/// `Table::insert` and a string key is no slot at all.
/// [`store_box`] is this operation followed by a slot write and
/// [`store_ptr`] is its pointer form, but both keep their own copy of it:
/// they are hot paths (`dev/INDEX.md`), so folding them in owes a
/// measurement this crate does not have.
///
/// # Safety
/// `new`'s entity live if it has one; `arena` the live mounted arena;
/// `owner_cat` the holder's category.
#[inline]
pub(crate) unsafe fn publish_child(
    arena: *mut Arena,
    owner_cat: MemoryCategory,
    new: Value,
) -> Option<Value> {
    if !new.is_refcounted() {
        return Some(new);
    }

    let child = new.entity_ptr();
    unsafe { ll_retain(child) };
    let stored = unsafe { store_category_barrier(arena, owner_cat, child) };
    if stored.is_null() {
        unsafe { ll_release(child) };
        return None;
    }

    if stored == child {
        return Some(new);
    }

    // The copy arrives at +1 from its factory and is what the holder
    // names, so the reference retained above goes back.
    unsafe { ll_release(child) };
    Some(Value::entity(new.tag(), stored))
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
#[must_use]
pub(crate) unsafe fn store_ptr(
    arena: *mut Arena,
    owner_cat: MemoryCategory,
    slot: *mut *mut RcHeader,
    new: *mut RcHeader,
) -> bool {
    let mut stored = new;
    if !new.is_null() {
        unsafe { ll_retain(new) };
        stored = unsafe { store_category_barrier(arena, owner_cat, new) };
        if stored.is_null() {
            // Only the copy path reports, and it reports out of memory.
            // The slot is untouched and `new` keeps the count it had.
            unsafe { ll_release(new) };
            return false;
        }

        if stored != new {
            // The slot took the copy, so the reference retained above is
            // the caller's to give back.
            unsafe { ll_release(new) };
        }
    }

    unsafe { write_ptr_slot(slot, stored) };
    true
}

/// Write an 8-byte pointer slot of a (possibly) walked object. Under
/// `rc-walk` the store is a relaxed atomic: the collector reads fields
/// concurrently, a racing plain store is undefined behaviour, and the
/// relaxed store is the same instruction. Same story for
/// [`write_value_slot`], whose two words the walker may see torn — the
/// design absorbs the tear (a phantom or missed edge, repaired by
/// Phases 3-4), the atomics make it defined.
#[inline]
pub(crate) unsafe fn write_ptr_slot(slot: *mut *mut RcHeader, new: *mut RcHeader) {
    #[cfg(not(feature = "rc-walk"))]
    unsafe {
        slot.write(new)
    };

    #[cfg(feature = "rc-walk")]
    unsafe {
        (*(slot as *const std::sync::atomic::AtomicPtr<RcHeader>))
            .store(new, std::sync::atomic::Ordering::Relaxed)
    };
}

/// Write a 16-byte `Value` slot; see [`write_ptr_slot`].
#[inline]
pub(crate) unsafe fn write_value_slot(slot: *mut Value, new: Value) {
    #[cfg(not(feature = "rc-walk"))]
    unsafe {
        slot.write(new)
    };

    #[cfg(feature = "rc-walk")]
    unsafe {
        use std::sync::atomic::{AtomicU64, Ordering};
        let words = core::mem::transmute::<Value, [u64; 2]>(new);
        (*(slot as *const AtomicU64)).store(words[0], Ordering::Relaxed);
        (*((slot as *const u8).add(8) as *const AtomicU64)).store(words[1], Ordering::Relaxed);
    };
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
#[must_use]
pub(crate) unsafe fn store_box(
    arena: *mut Arena,
    owner_cat: MemoryCategory,
    slot: *mut Value,
    new: Value,
) -> bool {
    let new_ptr = if new.is_refcounted() {
        new.entity_ptr()
    } else {
        std::ptr::null_mut()
    };

    let mut written = new;
    if !new_ptr.is_null() {
        unsafe { ll_retain(new_ptr) };
        let stored = unsafe { store_category_barrier(arena, owner_cat, new_ptr) };
        if stored.is_null() {
            unsafe { ll_release(new_ptr) };
            return false;
        }

        if stored != new_ptr {
            // Copied out of the arena: the tag is the value's, the
            // payload is the copy's.
            written = Value::entity(new.tag(), stored);
            unsafe { ll_release(new_ptr) };
        }
    }

    unsafe { write_value_slot(slot, written) };
    true
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
    let dead = unsafe { drop_ref_deferred(owner_cat, old) };
    if !dead.is_null() {
        // Last reference gone: tear it down (destructor, release children,
        // free), dispatched on the entity kind (`ll_entity_die`) — an
        // object through its class's dispose, a reference box by
        // releasing its Value.
        unsafe { crate::object::ll_entity_die(old) };
    }
}

/// [`drop_ref`] up to the teardown, which is handed back rather than run:
/// null when nothing died, otherwise the entity whose count just reached
/// zero and whose teardown the caller now owes.
///
/// It exists for the array's teardown drain, which must not call
/// `ll_entity_die` on a nested array — that call is the recursion the
/// drain replaces, one frame set per nesting level of caller-chosen depth
/// (`crate::array::entity::array_die`). Every other caller wants
/// [`drop_ref`], which is this function plus the teardown it defers.
///
/// # Safety
/// `old` null or the live entity the slot held; `owner_cat` the slot
/// owner's category.
#[inline]
pub(crate) unsafe fn drop_ref_deferred(
    owner_cat: MemoryCategory,
    old: *mut RcHeader,
) -> *mut RcHeader {
    if old.is_null() {
        return std::ptr::null_mut();
    }

    let old_cat = unsafe { crate::object::header_category(old) };

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
        old
    } else {
        std::ptr::null_mut()
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
#[must_use]
pub unsafe fn ref_store(
    arena: *mut Arena,
    owner: *mut RcHeader,
    slot: *mut Value,
    old: *mut RcHeader,
    new: Value,
) -> bool {
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

    let owner_cat = unsafe { crate::object::header_category(owner) };
    // Publish first, and only drop what the slot held if the publish
    // happened: a refused store leaves the slot exactly as it was, which
    // includes the reference it still holds.
    if !unsafe { store_box(arena, owner_cat, slot, new) } {
        return false;
    }

    unsafe { drop_ref(owner_cat, old) };
    true
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
) -> bool {
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
) -> bool {
    unsafe {
        store_ptr(
            resolve_arena(ctx),
            MemoryCategory::from_flags(owner_cat),
            slot,
            new,
        )
    }
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
) -> bool {
    unsafe {
        store_box(
            resolve_arena(ctx),
            MemoryCategory::from_flags(owner_cat),
            slot,
            new,
        )
    }
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

            assert!(unsafe { ref_store(arena, &mut self.header, &mut self.slot, old, value) });
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
        assert_eq!(
            unsafe { (*pa).refcount },
            2,
            "initial + the slot's reference"
        );

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
        assert!(unsafe { store_ptr(&mut arena, MemoryCategory::GcHeap, &mut slot, pa) });
        assert_eq!(slot, pa, "slot published as a bare 8-byte pointer");
        assert_eq!(
            unsafe { (*pa).refcount },
            2,
            "initial + the slot's reference"
        );

        // Overwriting store: publish the new pointer, then drop the old.
        let old = slot;
        assert!(unsafe { store_ptr(&mut arena, MemoryCategory::GcHeap, &mut slot, pb) });
        unsafe { drop_ref(MemoryCategory::GcHeap, old) };
        assert_eq!(slot, pb);
        assert_eq!(
            unsafe { (*pa).refcount },
            1,
            "displaced from a heap slot: released"
        );
        assert_eq!(unsafe { (*pb).refcount }, 2);
    }

    /// The most ordinary string store in the language: `$o->name = $s`,
    /// a heap object taking an arena string. A COW entity is value-like,
    /// so the holder takes a **copy** in the heap rather than a hold on
    /// arena memory. Before this, the store went down the escape counter
    /// and overwrote a live holder count with a hold-count of one —
    /// caught by a `debug_assert` in debug and silently wrong in release
    /// (`PLAN.md` task 15).
    #[test]
    fn a_cow_value_leaving_the_arena_is_copied_rather_than_counted() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut ctx = LLContext {
            arena: &raw mut arena,
        };

        let s = unsafe {
            crate::string::ll_string_new(&raw mut ctx, MemoryCategory::RequestArena, b"name")
        } as *mut RcHeader;
        let mut slot: *mut RcHeader = std::ptr::null_mut();

        assert!(unsafe { store_ptr(&raw mut arena, MemoryCategory::GcHeap, &mut slot, s) });

        assert_ne!(slot, s, "the heap slot must not hold arena memory");
        assert_eq!(
            unsafe { crate::object::header_category(slot) },
            MemoryCategory::GcHeap,
            "the copy lands where its holder lives"
        );
        assert_eq!(
            unsafe { crate::string::LLString::bytes(slot as *const crate::string::LLString) },
            b"name",
            "and it is the same value"
        );
        assert_eq!(
            unsafe { (*slot).refcount },
            1,
            "the slot is its only holder"
        );
        unsafe {
            assert_eq!((*s).flags & IS_ESCAPEE, 0, "a COW entity never escapes");
            assert_eq!((*s).refcount, 1, "the original keeps the count it had");
        }

        let mut escapees = Vec::new();
        arena.reset_with(|_| {}, |e| escapees.push(e));
        assert!(escapees.is_empty(), "nothing was logged as an escapee");

        unsafe { drop_ref(MemoryCategory::GcHeap, slot) };
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

        assert!(unsafe { store_ptr(&mut arena, MemoryCategory::LongLived, &mut slot, obj) });
        assert_ne!(
            unsafe { (*obj).flags } & IS_ESCAPEE,
            0,
            "arena ref escaped into a long-lived slot"
        );
        assert_eq!(
            unsafe { (*obj).refcount },
            1,
            "one holder, counted in the escapee"
        );

        let mut escapees = Vec::new();
        arena.reset_with(|_| {}, |e| escapees.push(e));
        assert_eq!(
            escapees,
            vec![obj],
            "the escapee itself, no slot dereferenced"
        );
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
    /// edge, so there is nothing to walk.
    ///
    /// **The slot is read from inside the destructor**, rather than
    /// inferred from what a collection there reclaims. That inference was
    /// the original instrument and it stopped measuring on 2026-08-07,
    /// when a fire point inside a teardown became a no-op
    /// (`dev/DECISIONS.md`): the count is zero now whatever the slot
    /// holds. Reading the slot states the property directly and needs no
    /// collection to expose it.
    ///
    /// rc-trace only, for the second half of the scenario: the
    /// destructor's fire point must reclaim nothing, and an `rc-walk`
    /// build has no candidate buffer to fire from. The
    /// publish-before-teardown order itself is strategy-independent.
    #[cfg(not(feature = "rc-walk"))]
    #[test]
    fn a_collecting_destructor_cannot_see_the_slot_it_is_being_removed_from() {
        use crate::class::ClassBuilder;
        use crate::gc::{ll_gc_collect_cycles, set_test_threshold};
        use crate::memory::context::LLContext;
        use crate::object::{Object, new_constructed};
        use crate::value::{Tag, Value};

        /// The owner's `next` slot, read from inside the destructor of
        /// the value being removed from it. Null until the destructor
        /// runs — `Value::null()` is not a legal reading of an
        /// unvisited slot, so the assertion below cannot pass by
        /// accident of the destructor never firing.
        static SEEN: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(usize::MAX);
        /// The owner, so the destructor can find the slot it is being
        /// removed from.
        static OWNER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        unsafe extern "C" fn read_the_slot(_obj: *mut Object) {
            let owner = OWNER.load(std::sync::atomic::Ordering::Relaxed) as *mut Object;
            let held = unsafe { Object::prop_at(owner, 16).read() };
            let entity = if held.is_refcounted() {
                held.entity_ptr() as usize
            } else {
                0
            };

            SEEN.store(entity, std::sync::atomic::Ordering::Relaxed);
            // The fire point a destructor may carry, which since
            // 2026-08-07 collects nothing from inside a teardown
            // (`dev/DECISIONS.md`). Kept here because this test exists
            // for what such a collection would have walked.
            assert_eq!(
                unsafe { ll_gc_collect_cycles() },
                0,
                "a collection fired from a destructor reclaims nothing"
            );
        }

        let _g = crate::memory::block_pool::test_guard();
        let owner_cls = ClassBuilder::new("C1Owner")
            .prop("next", true)
            .prop("mine", true)
            .build();
        let dying_cls = ClassBuilder::new("C1Dying")
            .destructor(read_the_slot as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        unsafe {
            let owner = new_constructed(&mut ctx, owner_cls, MemoryCategory::GcHeap);
            let old = new_constructed(&mut ctx, dying_cls, MemoryCategory::GcHeap);
            let next = Object::prop_at(owner, 16);
            let mine = Object::prop_at(owner, 32);
            OWNER.store(owner as usize, std::sync::atomic::Ordering::Relaxed);

            // owner --mine--> owner: a self-cycle, so the owner is garbage
            // held up only by its own edge.
            assert!(ref_store(
                &mut arena,
                owner as *mut RcHeader,
                mine,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, owner as *mut RcHeader),
            ));
            // owner --next--> old, then drop the creation reference: the
            // slot holds the only one left.
            assert!(ref_store(
                &mut arena,
                owner as *mut RcHeader,
                next,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, old as *mut RcHeader),
            ));
            assert!(!ll_release(old as *mut RcHeader));

            // Drop the owner's external reference too: now it is a
            // buffered candidate root, so the owner is exactly the shape
            // a collection would walk — through `next`, if the old value
            // were still visible there.
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

            // The store barrier publishes before it drops, so by the time
            // `old`'s teardown runs the slot holds the new value. A
            // reading of `old` here is the edge still standing into an
            // object at refcount zero, which anything walking the owner —
            // a collection, another destructor — would follow.
            assert_eq!(
                SEEN.load(std::sync::atomic::Ordering::Relaxed),
                0,
                "the slot must be published before the displaced value's teardown"
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
