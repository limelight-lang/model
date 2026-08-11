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

/// A store publishes the new value and reports whether it did.
/// Giving back what it displaced is the caller's second call,
/// `drop_ref`, and only on a report of `true`; a `Value` slot and a
/// bare pointer slot compose the same way. A null store clears the
/// slot, and with an arena owner the displaced heap value's release
/// belongs to the reset log rather than to the store — exactly one
/// release either way.
mod the_ordinary_store {
    use super::*;

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
}

/// The owner's category is a parameter rather than a header read, so
/// a headerless static block is a valid destination and still gets
/// the escape barrier. An arena reference entering a longer-lived
/// slot is recorded as an escapee, a COW value is copied instead
/// — it is value-like, and a copy holds no arena memory — a heap
/// reference entering an arena slot waits for the reset, and an
/// immortal value costs nothing at all.
mod what_crossing_a_category_boundary_costs {
    use super::*;

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

/// The slot holds the new value before the displaced one's
/// `__destruct` runs, so user code that collects from inside it
/// cannot walk an edge the refcount has already given up.
///
/// rc-trace only, for the second half of the scenario below: an
/// `rc-walk` build has no candidate buffer for the destructor's fire
/// point to reclaim from. The order itself is strategy-independent.
#[cfg(not(feature = "rc-walk"))]
mod publication_before_teardown {
    use super::*;

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
            assert!(
                ref_store(
                    &mut arena,
                    owner as *mut RcHeader,
                    next,
                    old as *mut RcHeader,
                    Value::null(),
                ),
                "the barrier refused the displacement this test is built on"
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
}
