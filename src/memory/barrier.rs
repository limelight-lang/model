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
use crate::refcount::{COW, IS_ESCAPEE, MemoryCategory, RcHeader, is_object, ll_release, ll_retain};
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

/// The store mechanics. `owner` is the entity containing `slot`;
/// `old` is the slot's current entity (the caller has it loaded, null
/// for a non-entity); `new` is the whole `Value` being stored.
///
/// **The barrier writes the entire 16-byte `Value`, not just its
/// payload word.** It used to write the payload and leave every call
/// site to stamp the tag afterwards, which left the slot torn — new
/// pointer, old tag — for the length of the call. Nothing noticed while
/// the slot was published last, because the tear ended before anything
/// could look; publishing before teardown makes a reentrant collector
/// able to read it, and "tag says object, pointer is null" is a crash.
/// One writer for one slot is also simply the right rule.
///
/// `arena` is a raw pointer, not `&mut Arena`: the displaced-value
/// teardown below runs `__destruct`, which reenters the runtime and
/// resolves this same arena. Holding an exclusive borrow across that
/// call would alias (audit H5). Every arena touch here happens before
/// that teardown, each through its own short-lived borrow.
///
/// # Safety
/// `owner` must be a live entity containing `slot`; `old` must be the
/// entity the slot currently holds (null when it holds a non-entity);
/// `old` and `new`'s entity must each be null or point to a live
/// entity; `arena` must point to the live mounted arena.
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

    // A non-entity value (int, bool, null) counts as a null entity here:
    // the categories and the counts are about entities only.
    let new_ptr = if new.is_refcounted() {
        new.entity_ptr()
    } else {
        std::ptr::null_mut()
    };

    if !new_ptr.is_null() {
        unsafe { ll_retain(new_ptr) };
    }

    let owner_cat = unsafe { (*owner).memory_category() };

    if !new_ptr.is_null() {
        let new = new_ptr;
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
            unsafe { (*arena).log_release_at_reset(new) };
        }
    }

    // The slot is published **before** anything releases `old`, and as a
    // whole value. Teardown of a displaced value runs `__destruct`, which
    // is user code and may collect. A collection that starts while the
    // slot still holds `old` walks an edge the refcount has already given
    // up and subtracts it a second time — the audit's C1. Everything
    // below reads `old` from the argument, not from the slot.
    unsafe { slot.write(new) };

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
            // The displaced value's last reference is gone: tear it down
            // (destructor, then release its children, then free). Dispatch
            // on the entity kind — only objects have teardown today;
            // strings/arrays claim their own kind bits later.
            if is_object(unsafe { (*old).flags }) {
                unsafe { crate::object::ll_object_die(old as *mut crate::object::Object) };
            }
        }
    }
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
