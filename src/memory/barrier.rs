//! The unified store barrier slot: `ll_ref_store`.
//!
//! Every reference store the compiler could not resolve statically goes
//! through this one door (`rfc/model/gc/strategies.md`). The runtime
//! only executes mechanics; *whether* to call the barrier — or skip it
//! because the categories were proven at compile time — is entirely the
//! compiler's decision.
//!
//! Composition in this build (phase 1): RC operations + the category
//! barrier (`rfc/model/memory/arenas.md`). Strategy hooks (SATB) plug
//! in here later.
//!
//! Category sources: both the stored value and the destination carry
//! their memory category as 2 bits in their own `RcHeader` flags,
//! stamped at allocation. A *slot* has no header — its category is its
//! owner's, which is why the owner is an argument. Which arena's logs
//! to write is answered by the context: one mounted arena per executing
//! context, kept correct by the compiler (actor arenas are unreachable
//! from outside, so a store creating an escape always runs with the
//! owner's arena mounted).

use crate::memory::arena::Arena;
use crate::memory::context::{LLContext, resolve_arena};
use crate::refcount::{COW, IS_ESCAPEE, MemoryCategory, RcHeader, ll_release, ll_retain};

/// A longer-lived container took a reference to request-arena object
/// `entity` (the **gain** of the escape rule, `rfc/model/memory/arenas.md`).
/// Bump its escape hold-count; on the 0 → 1 transition it becomes an
/// escapee and joins the arena's list. The count lives in `entity`'s
/// otherwise-idle `refcount` ([`IS_ESCAPEE`]), so reset decides its fate
/// from the count alone and never dereferences a holder slot.
///
/// # Safety
/// `entity` must be a live request-arena entity.
pub(crate) unsafe fn escape_gain(arena: &mut Arena, entity: *mut RcHeader) {
    let e = unsafe { &mut *entity };
    debug_assert!(
        e.flags & COW == 0,
        "COW arena value escape takes the deepCopy path, not the counter (deferred)"
    );
    if e.flags & IS_ESCAPEE == 0 {
        e.flags |= IS_ESCAPEE;
        e.refcount = 1;
        arena.log_escapee(entity);
    } else {
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

/// The store mechanics. `owner` is the entity containing `slot`;
/// `old` is the slot's current value (the caller has it loaded);
/// `new` is the value being stored. Writes the slot.
///
/// # Safety
/// `owner` must be a live entity containing `slot`; `old` must equal
/// `*slot`; `old`/`new` must each be null or point to a live entity.
pub unsafe fn ref_store(
    arena: &mut Arena,
    owner: *mut RcHeader,
    slot: *mut *mut RcHeader,
    old: *mut RcHeader,
    new: *mut RcHeader,
) {
    debug_assert!(!owner.is_null(), "a slot always has an owner");
    debug_assert_eq!(unsafe { *slot }, old, "old must be the slot's value");

    if !new.is_null() {
        unsafe { ll_retain(new) };
    }

    let owner_cat = unsafe { (*owner).memory_category() };

    if !new.is_null() {
        let new_cat = unsafe { (*new).memory_category() };

        // Dangerous direction: an arena reference stored into a
        // longer-lived container would dangle after reset. Count the
        // escape (`gain`); its fate is decided at arena death from the
        // count, never by reading this slot back.
        if new_cat == MemoryCategory::RequestArena && owner_cat != MemoryCategory::RequestArena {
            unsafe { escape_gain(arena, new) };
        }

        // Reverse direction: a heap entity stored into an arena
        // container would leak (reset skips per-object drop). The log
        // owns exactly one release per record.
        if new_cat == MemoryCategory::GcHeap && owner_cat == MemoryCategory::RequestArena {
            arena.log_release_at_reset(new);
        }
    }

    if !old.is_null() {
        let old_cat = unsafe { (*old).memory_category() };

        // A longer-lived slot letting go of an arena escapee: drop its
        // hold-count (`lose`). The barrier half of the escape rule; holder
        // teardown is the other half, and it is the same event.
        if old_cat == MemoryCategory::RequestArena && owner_cat != MemoryCategory::RequestArena {
            unsafe { escape_lose(old) };
        }

        // A displaced heap value in an arena container is NOT released
        // here: its own release-at-reset record owns that release
        // (releasing twice was the double-release bug the design
        // fixes). Everything else releases normally.
        let old_is_log_owned =
            owner_cat == MemoryCategory::RequestArena && old_cat == MemoryCategory::GcHeap;

        if !old_is_log_owned && unsafe { ll_release(old) } {
            // TODO(object-lifecycle): teardown of the dying entity.
        }
    }

    unsafe { *slot = new };
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
    slot: *mut *mut RcHeader,
    old: *mut RcHeader,
    new: *mut RcHeader,
) {
    unsafe { ref_store(resolve_arena(ctx), owner, slot, old, new) }
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
        slot: *mut RcHeader,
    }

    impl Holder {
        fn new(cat: MemoryCategory) -> Self {
            Holder {
                header: entity(cat),
                slot: std::ptr::null_mut(),
            }
        }

        unsafe fn store(&mut self, arena: &mut Arena, new: *mut RcHeader) {
            let old = self.slot;
            unsafe { ref_store(arena, &mut self.header, &mut self.slot, old, new) };
        }
    }

    #[test]
    fn heap_to_heap_counts_and_writes_slot() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut owner = Holder::new(MemoryCategory::GcHeap);
        let mut a = entity(MemoryCategory::GcHeap);
        let mut b = entity(MemoryCategory::GcHeap);

        unsafe { owner.store(&mut arena, &mut a) };
        assert_eq!(owner.slot, &mut a as *mut _);
        assert_eq!(a.refcount, 2, "initial + the slot's reference");

        unsafe { owner.store(&mut arena, &mut b) };
        assert_eq!(a.refcount, 1, "displaced from a heap slot: released now");
        assert_eq!(b.refcount, 2);
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
        assert!(owner.slot.is_null());
        assert_eq!(a.refcount, 2, "the log still owns A's release");

        arena.reset(|_| {});
        assert_eq!(a.refcount, 1, "exactly one release, from the log");
    }

    #[test]
    fn immortal_values_touch_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let mut owner = Holder::new(MemoryCategory::GcHeap);
        let mut s = entity(MemoryCategory::Immortal);

        unsafe { owner.store(&mut arena, &mut s) };
        assert_eq!(s.refcount, 1, "immortals are never counted");
        assert_eq!(owner.slot, &mut s as *mut _);

        let mut escapes = 0;
        arena.reset_with(|_| {}, |_| escapes += 1);
        assert_eq!(escapes, 0, "no logs for immortal stores");
    }
}
