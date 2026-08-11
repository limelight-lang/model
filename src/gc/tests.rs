use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::barrier::ref_store;
use crate::memory::context::LLContext;
use crate::object::new_constructed;
use crate::refcount::ll_release;
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS, wide_class};
use crate::value::Tag;

/// Real store through the barrier: retain + whole-value slot write.
unsafe fn link(arena: *mut Arena, from: *mut Object, offset: u32, to: *mut Object) {
    unsafe {
        let slot = Object::prop_at(from, offset);
        assert!(
            ref_store(
                arena,
                from as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, to as *mut RcHeader),
            ),
            "the barrier refused the link this test is built on"
        );
    }
}

fn node_class() -> *const crate::class::Class {
    ClassBuilder::new("CycleNode").prop("next", true).build()
}

/// Trial deletion subtracts the internal edges and reclaims what
/// reaches zero, restoring the counts of anything an external
/// reference holds. It has to trace through every kind carrying
/// counted slots — a reference box included, or the back-edge is
/// invisible and the object reads externally rooted — and through
/// both halves of the large-entity population. Acyclic garbage dies
/// by refcount and never reaches the collector at all.
mod trial_deletion_over_a_ring {
    use super::*;

    #[test]
    fn a_two_node_cycle_is_reclaimed() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            link(&mut arena, a, 16, b); // a→b: b rc=2
            link(&mut arena, b, 16, a); // b→a: a rc=2
            // External references die: counts drop to 1, both buffered.
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));
        }

        let freed = unsafe { collect_cycles() };
        assert_eq!(freed, 2, "the cycle is garbage and must be reclaimed");
        arena.reset(|_| {});
    }

    #[test]
    fn a_self_cycle_is_reclaimed() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            link(&mut arena, a, 16, a); // a→a: rc=2
            assert!(!ll_release(a as *mut RcHeader));
        }

        assert_eq!(unsafe { collect_cycles() }, 1);
        arena.reset(|_| {});
    }

    #[test]
    fn an_externally_referenced_cycle_survives_with_counts_restored() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            link(&mut arena, a, 16, b);
            link(&mut arena, b, 16, a);
            // Only b's external reference dies; a is still held (by us).
            assert!(!ll_release(b as *mut RcHeader));
        }

        assert_eq!(unsafe { collect_cycles() }, 0, "externally reachable");
        unsafe {
            assert_eq!((*a).rc.refcount, 2, "trial deletion fully restored");
            assert_eq!((*b).rc.refcount, 1);
        }

        // Now the external reference dies too: the cycle is garbage.
        unsafe { assert!(!ll_release(a as *mut RcHeader)) };
        assert_eq!(unsafe { collect_cycles() }, 2);
        arena.reset(|_| {});
    }

    /// A ring through a reference box (`$a->next = &$a`): trial deletion
    /// must trace THROUGH the box by kind, or the box's back-edge is
    /// invisible, the object reads externally rooted, and the ring leaks
    /// silently forever.
    #[test]
    fn a_cycle_through_a_reference_box_is_reclaimed() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let r = crate::reference::ll_reference_new();
        unsafe {
            // a.next owns the box's initial ref; the box owns a's second.
            Object::prop_at(a, 16).write(Value::entity(Tag::Reference, r as *mut RcHeader));
            crate::refcount::ll_retain(a as *mut RcHeader);
            (*r).value = Value::entity(Tag::Object, a as *mut RcHeader);
            // The frame's reference dies: a is buffered as a candidate.
            assert!(!ll_release(a as *mut RcHeader));
        }

        let freed = unsafe { collect_cycles() };
        assert_eq!(freed, 2, "object + box are one garbage ring");
        arena.reset(|_| {});
    }

    /// The same ring, in the strategy that finds its roots from
    /// decrements instead of from a walk. It exercises none of the
    /// enumerators — this collector never reads a block header — and
    /// what it pins is the other half: a large entity is an ordinary
    /// candidate, and the teardown that frees the white set routes by
    /// block kind (`rfc/model/memory/large-entities.md`).
    #[test]
    fn a_cycle_through_large_entities_is_collected_by_the_tracing_strategy() {
        let _g = crate::memory::block_pool::test_guard();
        let pooled_cls = wide_class("PooledTraceNode", POOLED_FILLERS, None);
        let run_cls = wide_class("RunTraceNode", RUN_FILLERS, None);

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        unsafe {
            let a = new_constructed(&mut ctx, pooled_cls, MemoryCategory::GcHeap);
            let b = new_constructed(&mut ctx, run_cls, MemoryCategory::GcHeap);
            let kind_of = |o: *mut Object| {
                *(((o as usize) & !crate::memory::block_pool::BLOCK_MASK) as *const u32)
            };

            assert_eq!(
                kind_of(a),
                crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE
            );
            assert_eq!(
                kind_of(b),
                crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN
            );

            let run_block = (b as usize) & !crate::memory::block_pool::BLOCK_MASK;
            link(&mut arena, a, 16, b);
            link(&mut arena, b, 16, a);
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));

            assert_eq!(collect_cycles(), 2, "the ring is garbage here too");
            assert!(
                !crate::memory::large_entity::snapshot().contains(&run_block),
                "and the run's registry entry went with it"
            );
        }

        arena.reset(|_| {});
    }

    #[test]
    fn acyclic_garbage_never_reaches_the_collector() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            // Plain death: refcount to zero, no non-zero decrement ever.
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_object_die(a);
        }

        assert_eq!(
            unsafe { (*candidate_buffer()).len() },
            0,
            "straight-line deaths never buffer"
        );
    }
}

/// A buffered entity that dies has to leave the buffer, or the next
/// collection traces freed memory as a root — a duty the teardown
/// doors owe, and one the drain owes for a nested array it tears
/// down itself. Buffering is deduplicated by a flag, a refused entry
/// leaves the entity unmarked rather than permanently unbufferable,
/// and `swap_remove` moves a candidate's recorded position with it.
mod the_candidate_buffer {
    use super::*;

    #[test]
    fn buffering_is_deduplicated_and_death_forgets_the_candidate() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            crate::refcount::ll_retain(a as *mut RcHeader);
            crate::refcount::ll_retain(a as *mut RcHeader); // rc=3
            assert!(!ll_release(a as *mut RcHeader)); // buffered
            assert!(!ll_release(a as *mut RcHeader)); // deduplicated
        }

        let buffered = unsafe { &*candidate_buffer() }
            .iter()
            .filter(|&&p| p == a as *mut RcHeader)
            .count();

        assert_eq!(buffered, 1, "one buffer entry per object");

        // The last reference dies through plain RC: the candidate must
        // be forgotten, and a later collection must not touch freed
        // memory.
        unsafe {
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_object_die(a);
        }

        assert_eq!(unsafe { collect_cycles() }, 0);
        arena.reset(|_| {});
    }

    /// A buffer that cannot grow refuses the candidate instead of taking
    /// the process down with it. The entity must come out of that
    /// unmarked — a buffered bit with no entry behind it would make the
    /// object permanently unbufferable and, worse, make `forget_candidate`
    /// hunt for something that was never there.
    #[test]
    fn a_refused_candidate_is_left_unmarked_and_arms_a_collection() {
        // `FORCE_BUFFER_REFUSAL` is process-global, so this has to hold
        // the test lock like any other fault injection here.
        let _g = crate::memory::block_pool::test_guard();
        use std::sync::atomic::Ordering;

        let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
        let p = &mut e as *mut RcHeader;
        COLLECT_PENDING.with(|f| f.set(false));

        FORCE_BUFFER_REFUSAL.store(true, Ordering::Relaxed);
        unsafe { buffer_candidate(p) };
        FORCE_BUFFER_REFUSAL.store(false, Ordering::Relaxed);

        assert!(
            unsafe { (*candidate_buffer()).is_empty() },
            "nothing was recorded"
        );
        assert_eq!(
            e.flags & CYCLE_COLLECTOR_BUFFERED,
            0,
            "and nothing was claimed"
        );
        assert!(COLLECT_PENDING.with(|f| f.get()), "a refusal arms instead");

        // Still bufferable once there is room again.
        unsafe { buffer_candidate(p) };
        assert_eq!(unsafe { (*candidate_buffer()).len() }, 1);
        unsafe { forget_candidate(p) };
        COLLECT_PENDING.with(|f| f.set(false));
    }

    /// `swap_remove` moves the tail candidate, so its recorded position
    /// has to move with it. A stale one cannot corrupt the buffer — the
    /// slot is checked before removal and a mismatch falls back to the
    /// scan — so this asserts the position itself, not just the outcome.
    #[test]
    fn forgetting_a_candidate_keeps_the_moved_one_findable() {
        let mut h: Vec<RcHeader> = (0..4)
            .map(|_| RcHeader::new(MemoryCategory::GcHeap, 0))
            .collect();
        let p: Vec<*mut RcHeader> = h.iter_mut().map(|e| e as *mut RcHeader).collect();
        let buffer = || unsafe { (*candidate_buffer()).clone() };

        unsafe {
            for &e in &p {
                buffer_candidate(e);
            }

            assert_eq!(buffer(), p, "buffered in order");

            // Removes index 1 and moves p[3] into it.
            forget_candidate(p[1]);
            assert_eq!(buffer(), vec![p[0], p[3], p[2]]);
            assert_eq!(
                decode_index(p[3]),
                Some(1),
                "the moved candidate knows where it is"
            );
            assert_eq!(
                decode_index(p[1]),
                None,
                "the removed one no longer claims a slot"
            );

            forget_candidate(p[3]);
            assert_eq!(buffer(), vec![p[0], p[2]], "the moved candidate was found");

            forget_candidate(p[0]);
            forget_candidate(p[2]);
            assert!(buffer().is_empty());
            assert!(h.iter().all(|e| e.flags & CYCLE_COLLECTOR_BUFFERED == 0));
        }
    }

    /// An array that dies through plain refcounting leaves the candidate
    /// buffer on the way out. The duty used to live inside
    /// `ll_default_dispose`, which no array ever runs, so a buffered
    /// array would die leaving its pointer behind and the next
    /// collection would trace freed memory as a root.
    ///
    /// Seen failing under Miri on the read through the stale root; under
    /// plain `cargo test` the reused slot answers plausibly and the
    /// assertion below is what catches it.
    #[test]
    fn a_dying_array_forgets_its_candidacy() {
        use crate::array::entity::ll_array_new;
        use crate::refcount::ll_retain;
        let _g = crate::memory::block_pool::test_guard();

        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            ll_retain(a as *mut RcHeader);
            assert!(!ll_release(a as *mut RcHeader));
            assert!(
                (*candidate_buffer()).contains(&(a as *mut RcHeader)),
                "the non-zero decrement buffered it"
            );

            assert!(ll_release(a as *mut RcHeader), "the last reference");
            crate::object::ll_entity_die(a as *mut RcHeader);
        }

        assert!(
            !unsafe { (*candidate_buffer()).contains(&(a as *mut RcHeader)) },
            "the buffer kept a root pointing at freed memory"
        );
        assert_eq!(unsafe { collect_cycles() }, 0);
    }

    /// The same duty, one level down and owed by a different party. A
    /// nested array is torn down by the drain inside
    /// `array::entity::array_die`, never by `ll_entity_die`, so the
    /// door's candidate-forget does not run for it and the drain owes it
    /// instead. Left out, the buffer keeps a root into freed memory —
    /// the state the door's duty was added to prevent.
    ///
    /// Seen failing on the candidacy assertion with the drain's
    /// `leave_the_candidate_buffer` call removed.
    #[test]
    fn a_nested_array_forgets_its_candidacy_when_the_drain_takes_it() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::refcount::ll_retain;
        use crate::value::Tag;
        let _g = crate::memory::block_pool::test_guard();

        let outer = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let inner = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            // A non-zero decrement is what buffers the inner array; the
            // entry that follows takes its creation reference, so the
            // count is one and the entry backs it.
            ll_retain(inner as *mut RcHeader);
            assert!(!ll_release(inner as *mut RcHeader));
            assert!(
                (*candidate_buffer()).contains(&(inner as *mut RcHeader)),
                "the non-zero decrement buffered the inner array"
            );
            (*outer).table.insert(
                crate::array::entity::category_of(outer),
                Key::Int(0),
                Value::entity(Tag::Array, inner as *mut RcHeader),
            );

            assert!(ll_release(outer as *mut RcHeader), "the last reference");
            crate::object::ll_entity_die(outer as *mut RcHeader);
        }

        assert!(
            !unsafe { (*candidate_buffer()).contains(&(inner as *mut RcHeader)) },
            "the buffer kept a root pointing at freed memory"
        );
        assert_eq!(unsafe { collect_cycles() }, 0);
    }
}

/// Crossing the threshold arms a collection and never runs one
/// inline, because the crossing happens inside a release or inside a
/// teardown's child releases, where the dying object sits at
/// refcount zero and is still a buffered root: a collection there
/// computes it garbage and frees it under its own teardown. A fire
/// point reached from inside a destructor therefore collects nothing
/// and leaves the work for the next clean one.
mod where_a_collection_may_fire {
    use super::*;

    /// The candidate buffer crossing its threshold *arms* a collection but
    /// never runs it inline. Here the arming happens inside `ll_object_die`'s
    /// phase 2 (a child release), the worst possible moment: on the old
    /// fire-inline code that collection ran mid-teardown and freed the
    /// dying object a second time. Now it only sets the pending flag, and
    /// the live child survives.
    #[test]
    fn threshold_crossing_during_teardown_only_arms() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let p = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let c = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };

        unsafe {
            // p.next = c  → c held by p's slot (rc 2) and by us (the creator
            // reference, which must keep c alive past p's death).
            link(&mut arena, p, 16, c);
            assert_eq!((*c).rc.refcount, 2);

            // Buffer p as a cycle-root candidate (a non-zero decrement),
            // still under the default threshold so nothing arms yet.
            crate::refcount::ll_retain(p as *mut RcHeader); // rc 2
            assert!(!ll_release(p as *mut RcHeader)); // rc 1, buffered
            assert!(!COLLECT_PENDING.with(|f| f.get()), "not armed yet");

            // From now the next buffered candidate crosses the threshold.
            set_test_threshold(1);

            // p's last reference dies; teardown releases c during phase 2,
            // which buffers c and crosses the threshold *mid-teardown*.
            assert!(ll_release(p as *mut RcHeader)); // rc 0 → death
            crate::object::ll_object_die(p);
            set_test_threshold(CANDIDATE_THRESHOLD);

            // The collection was armed, not fired: nothing ran inside the
            // teardown, so the still-referenced child is untouched and p was
            // freed exactly once (no crash). On the fire-inline code
            // COLLECT_PENDING is instead false here (a collection ran).
            assert!(COLLECT_PENDING.with(|f| f.get()), "armed, not fired");
            assert_eq!((*c).rc.refcount, 1, "the live child must survive");

            // Firing at a clean point reclaims nothing (c is externally held).
            assert_eq!(ll_gc_maybe_collect(), 0);
            assert!(!COLLECT_PENDING.with(|f| f.get()), "pending cleared");

            assert!(ll_release(c as *mut RcHeader));
            crate::object::ll_object_die(c);
        }

        arena.reset(|_| {});
    }

    /// An armed collection is deferred to a clean fire point: crossing the
    /// threshold from inside `ll_release` must not collect there (that is
    /// the mid-mutation hazard), only arm. The cyclic garbage stays live
    /// until `ll_gc_maybe_collect` runs it at a safe point.
    #[test]
    fn armed_cycle_is_deferred_to_maybe_collect() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = node_class();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            link(&mut arena, a, 16, a); // self-cycle: a.rc = 2
            set_test_threshold(1); // the buffering release will cross it

            // External reference dies: a non-zero decrement (a is still held
            // by its own self-edge), so it buffers a and crosses the
            // threshold from *inside* ll_release. Arm-and-defer must not
            // collect here.
            assert!(!ll_release(a as *mut RcHeader)); // a.rc 1, buffered
            set_test_threshold(CANDIDATE_THRESHOLD);

            assert!(COLLECT_PENDING.with(|f| f.get()), "armed");
            assert_eq!(
                (*a).rc.refcount,
                1,
                "cyclic garbage still live, not collected inline"
            );

            // Fire at a clean point: now the cycle is reclaimed.
            assert_eq!(ll_gc_maybe_collect(), 1);
            assert!(
                !COLLECT_PENDING.with(|f| f.get()),
                "pending cleared after fire"
            );
        }

        arena.reset(|_| {});
    }

    /// A fire point reached from inside a destructor collects nothing and
    /// leaves the work for the next clean point. Edmond's ruling of
    /// 2026-08-07 is that `ll_gc_maybe_collect` may stand inside a
    /// destructor body and must return there, so the runtime enforces it
    /// rather than trusting the compiler not to emit one.
    ///
    /// What it prevents: the object under teardown is at refcount zero
    /// and still a buffered root while its `dispose` releases children,
    /// so a collection running there computes it garbage and frees it,
    /// and the teardown that was interrupted frees it again. Seen
    /// failing at the returned count, which was 2 without the guard —
    /// the two objects being freed were the ones already dying.
    #[test]
    fn a_collection_fired_from_a_destructor_does_nothing_and_defers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        /// `usize::MAX` until the destructor has run.
        static FIRED: AtomicUsize = AtomicUsize::new(usize::MAX);

        unsafe extern "C" fn fire_a_collection(_o: *mut Object) {
            FIRED.store(unsafe { collect_cycles() }, Ordering::Relaxed);
        }

        let _g = crate::memory::block_pool::test_guard();
        FIRED.store(usize::MAX, Ordering::Relaxed);
        let node = node_class();
        let firer = ClassBuilder::new("FiringNode")
            .prop("next", true)
            .destructor(fire_a_collection as *const ())
            .build();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };

        unsafe {
            // Garbage waiting for a collection: a two-node cycle whose
            // creation references are gone. It is what the deferred
            // collection must still find afterwards.
            let d = new_constructed(&mut ctx, node, MemoryCategory::GcHeap);
            let e = new_constructed(&mut ctx, node, MemoryCategory::GcHeap);
            link(arena_ptr, d, 16, e);
            link(arena_ptr, e, 16, d);
            assert!(!ll_release(d as *mut RcHeader));
            assert!(!ll_release(e as *mut RcHeader));

            // The object that dies with a fire point inside its teardown:
            // `a` holds `c`, so `a`'s dispose drops `c` and `c`'s
            // destructor collects while `a` is a refcount-zero root.
            let a = new_constructed(&mut ctx, node, MemoryCategory::GcHeap);
            let c = new_constructed(&mut ctx, firer, MemoryCategory::GcHeap);
            link(arena_ptr, a, 16, c);
            assert!(!ll_release(c as *mut RcHeader), "a holds it");
            // A non-zero decrement, so `a` is a candidate root when it
            // dies a moment later.
            crate::refcount::ll_retain(a as *mut RcHeader);
            assert!(!ll_release(a as *mut RcHeader));
            assert!(ll_release(a as *mut RcHeader), "the last reference");
            crate::object::ll_entity_die(a as *mut RcHeader);
        }

        assert_eq!(
            FIRED.load(Ordering::Relaxed),
            0,
            "a collection fired from inside teardown must reclaim nothing"
        );
        assert_eq!(
            unsafe { collect_cycles() },
            2,
            "the refused collection deferred the work rather than losing it"
        );
        arena.reset(|_| {});
    }
}

/// Cyclic garbage runs `__destruct` before it is freed, so user code
/// runs over a half-deleted graph: a destructor nulling its own edge
/// releases a sibling the guard must hold to its own un-guard, and
/// one storing `$this` into a live holder resurrects the object
/// together with the child that survives only through it, without
/// the destructor running a second time.
mod what_a_destructor_does_to_the_white_set {
    use super::*;

    /// Cyclic garbage must run `__destruct` before it is freed — the gap
    /// this closes. A two-node cycle of objects each with a destructor,
    /// unreferenced from outside, is collected; both destructors must fire.
    #[test]
    fn cyclic_garbage_runs_its_destructor() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CYCLE_DTORS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn counting(_o: *mut Object) {
            CYCLE_DTORS.fetch_add(1, Ordering::Relaxed);
        }

        let _g = crate::memory::block_pool::test_guard();
        CYCLE_DTORS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("DtorNode")
            .prop("next", true)
            .destructor(counting as *const ())
            .build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        unsafe {
            let a = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
            let b = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
            link(&mut arena, a, 16, b);
            link(&mut arena, b, 16, a);
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));

            assert_eq!(collect_cycles(), 2, "the cycle is garbage");
            assert_eq!(
                CYCLE_DTORS.load(Ordering::Relaxed),
                2,
                "both cyclic objects ran __destruct before being freed"
            );
        }

        arena.reset(|_| {});
    }

    /// A destructor that nulls its own edge (`$this->next = null`) releases
    /// a sibling mid-teardown. The guard must hold that sibling to its own
    /// un-guard; nothing may be freed twice (Miri is the real check).
    #[test]
    fn a_destructor_unsetting_its_own_edge_does_not_double_free() {
        use crate::memory::context::{resolve_arena, set_current_context};
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DTORS: AtomicUsize = AtomicUsize::new(0);

        // An `assert!` inside a destructor aborts rather than failing the
        // test: this is `extern "C"` and a panic may not unwind out of it.
        // Legitimate here because `ref_store` can only refuse a COW entity
        // leaving the arena, and both stores below write null or a heap
        // object — but the idiom does not travel to a path where a refusal
        // is reachable.
        unsafe extern "C" fn unset_next(obj: *mut Object) {
            DTORS.fetch_add(1, Ordering::Relaxed);
            unsafe {
                let arena = resolve_arena(std::ptr::null_mut());
                let slot = Object::prop_at(obj, 16);
                let v = slot.read();
                let old = if v.is_refcounted() {
                    v.entity_ptr()
                } else {
                    std::ptr::null_mut()
                };

                assert!(
                    ref_store(arena, obj as *mut RcHeader, slot, old, Value::null()),
                    "the barrier refused the unset this destructor performs"
                );
            }
        }

        let _g = crate::memory::block_pool::test_guard();
        DTORS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("Unsetter")
            .prop("next", true)
            .destructor(unset_next as *const ())
            .build();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        unsafe {
            let a = new_constructed(ctx_ptr, cls, MemoryCategory::GcHeap);
            let b = new_constructed(ctx_ptr, cls, MemoryCategory::GcHeap);
            link(arena_ptr, a, 16, b);
            link(arena_ptr, b, 16, a);
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));
            collect_cycles();
            assert_eq!(
                DTORS.load(Ordering::Relaxed),
                2,
                "both ran once, no double free"
            );
        }

        set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// A destructor stores `$this` into a live holder, resurrecting the
    /// cycle. The re-trace must keep the resurrected object *and* its child
    /// (which gained no direct external reference — it survives only because
    /// its parent does), and `__destruct` must not run a second time.
    #[test]
    fn a_destructor_resurrecting_the_cycle_keeps_it_and_its_child() {
        use crate::memory::context::{resolve_arena, set_current_context};
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DTORS: AtomicUsize = AtomicUsize::new(0);
        static LIVE: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn resurrect(obj: *mut Object) {
            DTORS.fetch_add(1, Ordering::Relaxed);
            unsafe {
                let arena = resolve_arena(std::ptr::null_mut());
                let l = LIVE.load(Ordering::Relaxed) as *mut Object;
                let slot = Object::prop_at(l, 16);
                assert!(
                    ref_store(
                        arena,
                        l as *mut RcHeader,
                        slot,
                        std::ptr::null_mut(),
                        Value::entity(Tag::Object, obj as *mut RcHeader),
                    ),
                    "the barrier refused the resurrection this destructor stages"
                );
            }
        }

        let _g = crate::memory::block_pool::test_guard();
        DTORS.store(0, Ordering::Relaxed);
        let a_cls = ClassBuilder::new("Resur")
            .prop("next", true)
            .destructor(resurrect as *const ())
            .build();
        let l_cls = ClassBuilder::new("LiveHolder").prop("keep", true).build();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let ctx_ptr: *mut LLContext = &mut ctx;
        set_current_context(ctx_ptr);

        unsafe {
            let l = new_constructed(ctx_ptr, l_cls, MemoryCategory::GcHeap);
            let a = new_constructed(ctx_ptr, a_cls, MemoryCategory::GcHeap);
            let b = new_constructed(ctx_ptr, node_class(), MemoryCategory::GcHeap);
            LIVE.store(l as usize, Ordering::Relaxed);
            link(arena_ptr, a, 16, b);
            link(arena_ptr, b, 16, a);
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));

            let freed = collect_cycles();
            assert_eq!(DTORS.load(Ordering::Relaxed), 1);
            assert_eq!(freed, 0, "resurrected: nothing freed");
            assert_eq!(
                Object::prop_at(l, 16).read().entity_ptr(),
                a as *mut RcHeader,
                "L keeps A"
            );
            assert_eq!(
                Object::prop_at(a, 16).read().entity_ptr(),
                b as *mut RcHeader,
                "A->B intact"
            );
            assert_eq!((*a).rc.refcount, 2, "A: B->A + L->A");
            assert_eq!((*b).rc.refcount, 1, "B: A->B");

            // Drop the holder; the cycle is garbage again but __destruct
            // already ran, so it must not fire twice.
            assert!(
                ref_store(
                    arena_ptr,
                    l as *mut RcHeader,
                    Object::prop_at(l, 16),
                    a as *mut RcHeader,
                    Value::null(),
                ),
                "the barrier refused the drop of the holder's slot"
            );
            assert!(ll_release(l as *mut RcHeader));
            crate::object::ll_object_die(l);
            assert_eq!(collect_cycles(), 2, "the un-held cycle is reclaimed");
            assert_eq!(DTORS.load(Ordering::Relaxed), 1, "no second __destruct");
        }

        set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }
}

/// A white-set member is freed carrying whatever count trial
/// deletion left, so the free writes the final zero itself: the
/// enumerators read that word as occupancy, and a slot they read as
/// live has a free-list link where the class pointer was. Everything
/// an entity holds outside its own slot goes back with it — an
/// array's table storage, a dynamic string's payload — and an escape
/// hold-count on an arena object is dropped, or the reset promotes
/// an escapee nobody holds.
mod what_the_free_of_the_white_set_owes {
    use super::*;

    /// A freed slot's header word is the enumerators' occupancy test:
    /// `heap::for_each_entity_slot` and the epoch snapshot both read it and
    /// treat a non-zero refcount as a live entity. An ordinary death drives
    /// the count to zero on its way out, but a white-set member is freed
    /// while its count is whatever trial deletion left, so the free has to
    /// write the final zero itself.
    ///
    /// The consequence is worse than an over-count. A freed object slot has
    /// the free-list link at bytes 8-15, where the class pointer was, so a
    /// walk that reads the slot as live follows a free-list link as a
    /// `*const Class`.
    #[test]
    fn a_collected_member_leaves_a_slot_the_walk_reads_as_free() {
        let _g = crate::memory::block_pool::test_guard();

        let cls = ClassBuilder::new("WhiteRing")
            .prop("self", true)
            .prop("text", true)
            .build();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let s = unsafe { crate::string::ll_string_new(&mut ctx, MemoryCategory::GcHeap, b"white") };
        assert!(!s.is_null());

        unsafe {
            assert!(ref_store(
                arena_ptr,
                obj as *mut RcHeader,
                Object::prop_at(obj, 16),
                std::ptr::null_mut(),
                Value::entity(Tag::Object, obj as *mut RcHeader),
            ));
            assert!(ref_store(
                arena_ptr,
                obj as *mut RcHeader,
                Object::prop_at(obj, 32),
                std::ptr::null_mut(),
                Value::entity(Tag::String, s as *mut RcHeader),
            ));
            assert!(!ll_release(s as *mut RcHeader));
            assert!(!ll_release(obj as *mut RcHeader));
        }

        let (obj_addr, string_addr) = (obj as usize, s as usize);
        let reclaimed = unsafe { collect_cycles() };
        assert!(reclaimed >= 2, "the self-ring was not collected");

        // Nothing allocates from this block between the collection and the
        // read: blocks are owner-allocated and this thread is the owner, so
        // the two slots are still on its free list.
        let refcount_at = |addr: usize| unsafe { *(addr as *const u32) };
        assert_eq!(
            refcount_at(obj_addr),
            0,
            "the collected object's slot still reads as a live entity"
        );
        assert_eq!(
            refcount_at(string_addr),
            0,
            "the collected string's slot still reads as a live entity"
        );

        arena.reset(|_| {});
    }

    /// The white set is freed by reclaiming each entity's own slot, and
    /// an array's table storage is not in that slot. Without the arm the
    /// storage is lost with no pointer left anywhere to it — a buffer
    /// chunk holding its block's live count above zero for the life of
    /// the process. `$obj->arr = [$obj]` reaches it with no new mechanism:
    /// the object buffers as a candidate on kind 0 and `trace_entity`
    /// pulls the array into the white set behind it.
    ///
    /// Seen failing on the storage never coming back.
    #[test]
    fn a_collected_array_gives_its_table_storage_back() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::memory::buffer::{PressureMode, set_pressure_mode};
        use crate::memory::buffer_arena::with_buffer_arena;
        use crate::refcount::ll_retain;
        let _g = crate::memory::block_pool::test_guard();

        let cls = ClassBuilder::new("ArrayHolder").prop("arr", true).build();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };

        let (storage, capacity) = unsafe {
            // Retained before the entry is published, per `Table::insert`.
            ll_retain(obj as *mut RcHeader);
            (*a).table.insert(
                crate::array::entity::category_of(a),
                Key::Int(0),
                Value::entity(Tag::Object, obj as *mut RcHeader),
            );
            (*a).table.storage_and_capacity()
        };

        assert!(!storage.is_null(), "the insert allocated storage to lose");

        unsafe {
            let slot = Object::prop_at(obj, 16);
            assert!(ref_store(
                arena_ptr,
                obj as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, a as *mut RcHeader),
            ));
            // Both creation references go; the ring is all that holds
            // either of them now.
            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(obj as *mut RcHeader));
        }

        let reclaimed = unsafe { collect_cycles() };
        assert!(reclaimed >= 2, "the ring was not collected");

        // In critical mode an allocation searches the block's free list,
        // so the same address coming back is the storage having been
        // returned rather than merely forgotten.
        set_pressure_mode(PressureMode::Critical);
        let (reused, granted) = with_buffer_arena(|arena| arena.alloc(capacity));
        set_pressure_mode(PressureMode::Plenty);
        assert_eq!(reused, storage, "the array's table storage was never freed");
        with_buffer_arena(|arena| unsafe { arena.free(reused, granted) });

        arena.reset(|_| {});
    }

    /// The same hole, one kind over. A dynamic string's payload is a
    /// separate allocation too, and it has been reachable from cyclic
    /// garbage since the layout landed — longer than the array has
    /// existed. The critic pass that found the array half named only the
    /// array; this is the rest of it.
    ///
    /// A self-ring is enough: the object holds itself, so it is garbage,
    /// and its string property is white behind it.
    #[test]
    fn a_collected_dynamic_string_gives_its_payload_back() {
        use crate::memory::buffer::{PressureMode, set_pressure_mode};
        use crate::memory::buffer_arena::with_buffer_arena;
        use crate::string::ll_string_new_dynamic;
        let _g = crate::memory::block_pool::test_guard();

        let cls = ClassBuilder::new("StringHolder")
            .prop("self", true)
            .prop("text", true)
            .build();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = LLContext { arena: arena_ptr };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let s = unsafe {
            ll_string_new_dynamic(
                std::ptr::null_mut(),
                MemoryCategory::GcHeap,
                b"a payload",
                0,
            )
        };

        assert!(!s.is_null());
        let (payload, capacity) = unsafe { ((*s).data, (*s).capacity as usize) };
        assert!(!payload.is_null(), "the string has an out-of-line payload");

        unsafe {
            assert!(ref_store(
                arena_ptr,
                obj as *mut RcHeader,
                Object::prop_at(obj, 16),
                std::ptr::null_mut(),
                Value::entity(Tag::Object, obj as *mut RcHeader),
            ));
            assert!(ref_store(
                arena_ptr,
                obj as *mut RcHeader,
                Object::prop_at(obj, 32),
                std::ptr::null_mut(),
                Value::entity(Tag::String, s as *mut RcHeader),
            ));
            assert!(!ll_release(s as *mut RcHeader));
            assert!(!ll_release(obj as *mut RcHeader));
        }

        let reclaimed = unsafe { collect_cycles() };
        assert!(reclaimed >= 2, "the self-ring was not collected");

        set_pressure_mode(PressureMode::Critical);
        let (reused, granted) = with_buffer_arena(|arena| arena.alloc(capacity));
        set_pressure_mode(PressureMode::Plenty);
        assert_eq!(reused, payload, "the string's payload was never freed");
        with_buffer_arena(|arena| unsafe { arena.free(reused, granted) });

        arena.reset(|_| {});
    }

    /// A cyclic garbage holder that referenced an arena object still
    /// owes it a `lose`. The trace never sees arena entities — only the
    /// heap is traced — so freeing the white set has to drop those
    /// hold-counts itself. Left standing, the count makes arena reset
    /// believe a dead holder still holds the escapee, and reset promotes
    /// it: a leak for the life of the process, and a live-looking object
    /// nobody can reach.
    #[test]
    fn collecting_a_holder_drops_its_escape_hold_counts() {
        use crate::refcount::IS_ESCAPEE;

        let _g = crate::memory::block_pool::test_guard();
        let holder_cls = ClassBuilder::new("EscHolder")
            .prop("peer", true)
            .prop("esc", true)
            .build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        unsafe {
            let a = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
            let b = new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap);
            let escapee = new_constructed(&mut ctx, node_class(), MemoryCategory::RequestArena);

            link(&mut arena, a, 16, b); // a <-> b: a heap cycle
            link(&mut arena, b, 16, a);
            link(&mut arena, a, 32, escapee); // and a holds an arena object
            assert_ne!(
                (*escapee).rc.flags & IS_ESCAPEE,
                0,
                "the store made it an escapee"
            );

            assert!(!ll_release(a as *mut RcHeader));
            assert!(!ll_release(b as *mut RcHeader));
            assert_eq!(ll_gc_collect_cycles(), 2, "the cycle is garbage");

            assert_eq!(
                (*escapee).rc.flags & IS_ESCAPEE,
                0,
                "the dead holder let go of its escapee"
            );
        }

        arena.reset(|_| {});
    }
}

/// The candidate gate decides what this strategy can ever see, and
/// both shapes reach it through a kind that is not an object: two
/// arrays holding each other, and `$a[0] = &$a`, where the last
/// external release lands on the box. While the gate was a mask over
/// the kind codes neither produced a candidate, so the configuration
/// whose whole purpose is cycles was green with a systematic leak.
mod a_ring_with_no_object_in_it {
    use super::*;

    /// A ring with no object anywhere in it: two arrays holding each
    /// other. The last external release of either is a non-zero
    /// decrement and bought nothing while the gate masked all three kind
    /// bits — neither array ever became a candidate, the collector never
    /// got a root, and the ring leaked in the configuration whose whole
    /// purpose is cycles. Both configurations are required legs of the
    /// gate, so rc-trace was green with a systematic leak in it; the
    /// rc-walk twin is `walk::tests::a_ring_of_two_arrays_and_no_object_
    /// is_collected`.
    ///
    /// Seen failing on the candidacy assertion below.
    #[test]
    fn a_ring_of_two_arrays_and_no_object_is_collected() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::refcount::ll_retain;
        let _g = crate::memory::block_pool::test_guard();

        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let b = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            // The reference is taken before the entry is published, which
            // is `Table::insert`'s contract: an entry a walker can reach
            // must already be backed by a count.
            ll_retain(b as *mut RcHeader);
            (*a).table.insert(
                crate::array::entity::category_of(a),
                Key::Int(0),
                Value::entity(Tag::Array, b as *mut RcHeader),
            );
            ll_retain(a as *mut RcHeader);
            (*b).table.insert(
                crate::array::entity::category_of(b),
                Key::Int(0),
                Value::entity(Tag::Array, a as *mut RcHeader),
            );
            // Drop the creation references: each array is held by the
            // other and by nothing else, which is the ring.
            assert!(!ll_release(a as *mut RcHeader), "a is still held by b");
            assert!(!ll_release(b as *mut RcHeader), "b is still held by a");
        }

        assert!(
            unsafe { (*candidate_buffer()).contains(&(a as *mut RcHeader)) },
            "an array that took a non-zero decrement is a candidate root"
        );
        // At least two, not exactly two: the buffer is this thread's and
        // an earlier test on it may have left roots of its own, so an
        // exact count would be a claim about them rather than about this
        // ring.
        assert!(
            unsafe { collect_cycles() } >= 2,
            "the ring was judged and then not freed"
        );
    }

    /// A ring whose last external release lands on the **ReferenceBox**:
    /// `$a[0] = &$a`, where `&$a` makes the variable a box, the box
    /// holds the array and the array's element holds the box. An integer
    /// key, because the key's own kind is not what this measures — a
    /// string key would add one counted child and no edge through the
    /// box. Nothing
    /// outside ever decrements the array, so the only entity that can
    /// become a candidate is the box — and unless the gate admits its
    /// kind, the ring produces no candidate at all, so no collection ever
    /// judges it and it lives to process exit.
    ///
    /// The rc-walk twin is
    /// `walk::tests::a_ring_through_a_reference_box_and_an_array_is_collected`,
    /// which needs no candidate at all.
    #[test]
    fn a_ring_whose_last_release_lands_on_a_reference_box_is_collected() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::refcount::ll_retain;
        let _g = crate::memory::block_pool::test_guard();

        let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let boxed = crate::reference::ll_reference_new();
        unsafe {
            // `&$a` moves the variable's hold onto the box rather than
            // adding one, so the box takes the array's creation reference.
            (*boxed).value = Value::entity(Tag::Array, array as *mut RcHeader);
            // Retained before the entry is published, per `Table::insert`.
            ll_retain(boxed as *mut RcHeader);
            (*array).table.insert(
                crate::array::entity::category_of(array),
                Key::Int(0),
                Value::entity(Tag::Reference, boxed as *mut RcHeader),
            );
            // The frame's reference dies. It is the ring's only external
            // hold, and it lands on the box rather than on the array.
            assert!(
                !ll_release(boxed as *mut RcHeader),
                "the box is still held by the array's element"
            );
        }

        assert!(
            unsafe { (*candidate_buffer()).contains(&(boxed as *mut RcHeader)) },
            "a reference box that took a non-zero decrement is a candidate root"
        );
        assert!(
            unsafe { collect_cycles() } >= 2,
            "the ring was judged and then not freed"
        );
    }
}
