use super::*;
use crate::class::ClassBuilder;
use crate::epoch::checkpoint;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::memory::heap::for_each_entity_slot;
use crate::object::{Object, new_constructed};
use crate::refcount::{ll_release, ll_retain};
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS};
use crate::value::{Tag, Value};
use std::sync::atomic::AtomicUsize;

fn walked_addresses() -> Vec<usize> {
    let mut seen = Vec::new();
    unsafe { for_each_entity_slot(|e| seen.push(e as usize)) };
    seen
}

unsafe fn tie(a: *mut Object, offset: u32, b: *mut Object) {
    unsafe {
        Object::prop_at(a, offset).write(Value::entity(Tag::Object, b as *mut RcHeader));
    }
}

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// Step one epoch to completion on this thread, playing both actors:
/// the collector steps, with a mutator checkpoint wherever the
/// protocol needs an ack or a drain.
fn stepped_epoch() -> EpochStats {
    let mut e = Epoch::open();
    checkpoint(); // publish the activity bit
    e.snapshot();
    e.walk();
    e.judge();
    if e.stats.candidates > 0 {
        e.condemn();
        checkpoint(); // the Phase 3 ack
        e.recheck_and_post();
        checkpoint(); // the Phase 4 drain
    }

    let stats = e.close();
    checkpoint(); // the post-epoch flush of parked memory
    stats
}

/// [`crate::test_support::wide_class`] with the counting destructor
/// every ring test here uses.
fn wide_class(name: &str, fillers: usize) -> *const crate::class::Class {
    crate::test_support::wide_class(name, fillers, Some(counting_destructor as *const ()))
}

/// The collector's reader has to find every population and every
/// kind's children: a template's values, counted by its shape rather
/// than by its class, so a class-driven walk finds none of them;
/// both halves of the large-entity population, the pooled block the
/// region scan reaches and the run only the registry names; and a
/// retained former-arena block, whose occupants come from the
/// reset's own index because nothing there can be strided. A run
/// freed mid-epoch stays addressable until the flush, its memory
/// being unmapped at the real free.
mod what_the_snapshot_reaches {
    use super::*;

    /// The concurrent collector reads cells relaxed-atomically and so
    /// keeps its own copy of the slot stride — which means a template's
    /// values, counted by its shape rather than by its class, have to be
    /// found here too. A class-driven walk finds nothing on a template
    /// (the class has no runs), and an under-counted in-degree makes the
    /// target look rooted: a ring through a template would never be
    /// collected.
    #[test]
    fn the_concurrent_tracer_sees_a_templates_values() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        // The shape lives in immortal memory, where a compiler-emitted
        // one lives: this walk recovers the shape word from a relaxed
        // integer load, so the address it casts back has to be one that
        // was exposed as an integer to begin with — Miri says so, and it
        // is right (`dev/WORKFLOW.md`, provenance).
        let parts = [crate::intern::intern_str(""), crate::intern::intern_str("")];
        let shape =
            crate::memory::immortal::immortal_alloc(size_of::<crate::template::TemplateShape>())
                as *mut crate::template::TemplateShape;
        unsafe {
            shape.write(crate::template::TemplateShape {
                value_count: 1,
                parts: parts.as_ptr(),
            })
        };

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let held = unsafe {
            crate::string::ll_string_new(&mut ctx, crate::refcount::MemoryCategory::GcHeap, b"v")
        };

        let values = [Value::entity(Tag::String, held as *mut RcHeader)];
        let t = unsafe {
            crate::template::ll_template_new(
                &mut ctx,
                cls,
                shape,
                &values,
                crate::refcount::MemoryCategory::GcHeap,
            )
        };

        let mut seen = Vec::new();
        unsafe {
            crate::walk::trace_cells::<crate::walk::RelaxedCells>(
                t as *mut RcHeader,
                EntityKind::Object as u32,
                |cell| seen.push(cell.child),
            )
        };

        assert_eq!(
            seen,
            vec![held as *mut RcHeader],
            "the template's value is invisible to the epoch's walk"
        );

        unsafe {
            if ll_release(t as *mut RcHeader) {
                crate::object::ll_entity_die(t as *mut RcHeader);
            }

            if ll_release(held as *mut RcHeader) {
                crate::object::ll_entity_die(held as *mut RcHeader);
            }
        }

        arena.reset(|_| {});
    }

    /// The epoch's own snapshot reaches a large entity, which is a
    /// different question from the synchronous walk reaching one: this is
    /// the arm that runs on the collector thread, and the rows it builds
    /// are what `census_row` divides. Both halves of the population are
    /// here — a pooled block found by the region scan, an OS-direct run
    /// found only in the registry — and each contributes **one** slot.
    /// A stride would fabricate rows out of the objects' own cells, and
    /// fabricated edges can balance a live component into collection.
    #[test]
    fn the_epoch_snapshot_reaches_both_halves_of_a_large_entity_ring() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe {
            new_constructed(
                &mut ctx,
                wide_class("EpochPooled", POOLED_FILLERS),
                MemoryCategory::GcHeap,
            )
        };

        let b = unsafe {
            new_constructed(
                &mut ctx,
                wide_class("EpochRun", RUN_FILLERS),
                MemoryCategory::GcHeap,
            )
        };

        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }

        // The snapshot the epoch will take, read directly: one row per
        // object, at the object's own address, one slot wide.
        let rows = crate::memory::heap::snapshot_entity_blocks();
        for &entity in &[a as usize, b as usize] {
            let row = rows
                .iter()
                .find(|r| r.payload == entity)
                .expect("a large entity is missing from the epoch's snapshot");
            assert_eq!(row.slots, 1, "one occupant, whatever its size");
            assert!(
                row.index.is_none(),
                "and it is found by address, not by index"
            );
        }

        let first = stepped_epoch();
        assert!(first.stamped_new >= 2, "creation epoch: allocate-black");
        assert_eq!(first.confirmed, 0);

        let second = stepped_epoch();
        assert_eq!(
            second.confirmed, 1,
            "the ring is one confirmed component, across both halves"
        );
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// A run freed while an epoch is in flight stays addressable until
    /// the flush, which is the whole reason both large-entity kinds park:
    /// its memory is **unmapped** at the real free, and the snapshot
    /// dereferences every registered address. The corpse reads refcount 0
    /// and takes no row — what is being tested is that reading it is
    /// sound at all.
    #[test]
    fn a_run_freed_mid_epoch_is_still_addressable_when_the_snapshot_reads_it() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe {
            new_constructed(
                &mut ctx,
                wide_class("EpochDying", RUN_FILLERS),
                MemoryCategory::GcHeap,
            )
        };

        let block = (obj as usize) & !crate::memory::block_pool::BLOCK_MASK;
        assert!(crate::memory::large_entity::snapshot().contains(&block));

        let mut e = Epoch::open();
        checkpoint();
        unsafe {
            assert!(ll_release(obj as *mut RcHeader));
            crate::object::ll_entity_die(obj as *mut RcHeader);
        }

        assert!(
            crate::memory::large_entity::snapshot().contains(&block),
            "the free parked, so the run is still registered and still mapped"
        );

        e.snapshot();
        e.walk();
        e.judge();
        assert_eq!(e.stats.candidates, 0, "a corpse is no candidate");
        let _ = e.close();
        checkpoint();

        assert!(
            !crate::memory::large_entity::snapshot().contains(&block),
            "and the flush gives the run back"
        );
        arena.reset(|_| {});
    }

    /// The epoch reaches a retained former-arena block the same way it
    /// reaches an entity block, though neither the walk nor the census
    /// can stride there: the reset's object index supplies the slot
    /// addresses, and the census resolves a child inside one by
    /// searching that index instead of dividing
    /// (`rfc/model/gc/retained-block-walk.md`). The ring is built and
    /// promoted before any epoch, so it matures in the first and dies
    /// in the second, exactly as a heap-born ring does.
    #[test]
    fn a_ring_promoted_into_a_retained_block_is_collected() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let node = ClassBuilder::new("EpochPromotedRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();
        let holder_cls = ClassBuilder::new("EpochPromotedHolder")
            .prop("head", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
        let b = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
            assert!(crate::memory::barrier::ref_store(
                &mut arena,
                holder as *mut RcHeader,
                Object::prop_at(holder, 16),
                std::ptr::null_mut(),
                Value::entity(Tag::Object, a as *mut RcHeader),
            ));
        }

        unsafe { crate::promote::arena_reset_full(&mut arena) };
        assert_eq!(
            unsafe {
                (*crate::memory::block_pool::BlockHeader::of_ptr(a as *const u8))
                    .kind
                    .load(Ordering::Relaxed)
            },
            crate::memory::block_pool::BLOCK_KIND_RETAINED
        );
        unsafe {
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
        }

        let first = stepped_epoch();
        assert!(
            first.stamped_new >= 2,
            "the promoted pair is new to the collector"
        );
        let second = stepped_epoch();
        assert!(
            second.walked >= 2,
            "a retained block's occupants are walkable"
        );
        assert_eq!(
            second.confirmed, 1,
            "the promoted ring is one confirmed component"
        );
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        assert!(!walked_addresses().contains(&(a as usize)));
    }
}

/// A frame-held ring is a computed root in every epoch, and a ring
/// born under one is stamped new there, so it matures in that epoch
/// and dies in the next. Phase 3 acquits a component the mutator
/// touched between condemn and re-check, while a borrow taken and
/// returned restores the snapshot exactly and confirms — correctly,
/// the Phase 4 exact test proving every reference is in-component at
/// drain time.
mod the_three_way_judgement {
    use super::*;

    /// F3 made concrete: a ring created before its first epoch is
    /// stamped new there (allocate-black) and collected only by the
    /// second epoch — destructors, memory and all.
    #[test]
    fn a_garbage_ring_matures_one_epoch_and_dies_the_next() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }

        let first = stepped_epoch();
        assert!(first.stamped_new >= 2, "creation epoch: allocate-black");
        assert_eq!(first.confirmed, 0);
        let seen = walked_addresses();
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));

        let second = stepped_epoch();
        assert!(second.walked >= 2, "stamped last epoch: mature now");
        assert_eq!(second.confirmed, 1, "the ring is one confirmed component");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// A frame-held ring is a computed root in every epoch — never a
    /// candidate, never condemned (the central identity; scenario 2).
    #[test]
    fn a_live_ring_is_never_condemned() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorLiveRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
            ll_retain(a as *mut RcHeader); // the frame
        }

        stepped_epoch(); // matures them
        let stats = stepped_epoch();
        assert_eq!(stats.candidates, 0, "RC − IN > 0 on a: rooted, ring marked");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0);

        // Genuine garbage once the frame lets go.
        assert!(!unsafe { ll_release(a as *mut RcHeader) });
        let stats = stepped_epoch();
        assert_eq!(stats.confirmed, 1);
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        arena.reset(|_| {});
    }

    /// The Phase 3 filter: a mutator touch between condemn and re-check
    /// (here a borrow, still held) acquits the whole component by
    /// snapshot difference. The acquittal is collector-private — no
    /// message, no duties — and the untouched ring is collected by the
    /// next epoch.
    #[test]
    fn a_touch_between_condemn_and_recheck_acquits() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorTouched")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }

        stepped_epoch(); // mature

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk();
        e.judge();
        assert_eq!(e.stats.candidates, 1);
        e.condemn();
        // The mutator borrows a member and still holds it at re-check
        // time: the count difference against the snapshot acquits.
        // (A touch-and-restore acquits nothing — that case is the
        // sibling test below.)
        unsafe { ll_retain(a as *mut RcHeader) };
        checkpoint(); // ack
        e.recheck_and_post();
        assert_eq!(
            e.stats.acquitted, 1,
            "the held borrow acquits by difference"
        );
        assert_eq!(e.stats.confirmed, 0);
        assert_eq!(
            crate::epoch::outstanding_verdicts(),
            0,
            "an acquittal posts nothing — it is dropped in private"
        );
        let _ = e.close();
        checkpoint(); // flush
        assert!(!unsafe { ll_release(a as *mut RcHeader) }); // borrow ends

        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            0,
            "acquitted: nothing died"
        );
        let seen = walked_addresses();
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));

        let stats = stepped_epoch();
        assert_eq!(stats.confirmed, 1, "untouched next epoch: collected");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        arena.reset(|_| {});
    }

    /// The ABA case: a borrow taken and returned between condemn and
    /// re-check restores the snapshot exactly and the component
    /// **confirms** — correctly: the Phase 4 exact test proves every
    /// reference is in-component at drain time, so the transiently
    /// borrowed ring is garbage and is freed one epoch earlier than
    /// the old clear-on-touch filter allowed.
    #[test]
    fn a_touch_and_restore_between_condemn_and_recheck_confirms_and_frees() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorAba")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }

        stepped_epoch(); // mature

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk();
        e.judge();
        assert_eq!(e.stats.candidates, 1);
        e.condemn();
        unsafe {
            ll_retain(a as *mut RcHeader);
            assert!(!ll_release(a as *mut RcHeader)); // restored: ABA
        }

        checkpoint(); // ack
        e.recheck_and_post();
        assert_eq!(e.stats.confirmed, 1, "the restored count confirms");
        checkpoint(); // drain: exact test passes, the ring dies here
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2, "freed this epoch");
        let _ = e.close();
        checkpoint(); // flush
        arena.reset(|_| {});
    }

    /// A ring that runs through an array is garbage like any other, and
    /// the epoch could not see it until the array's entries could be read
    /// coherently: the edge *into* the array was counted and the edge
    /// *out* was not, so the holder read `RC` above `IN` and was a
    /// computed root every epoch, forever (`PLAN.md`, item 12).
    #[test]
    fn a_mature_ring_through_an_array_is_collected() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorArrayRing")
            .prop("table", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let table = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        assert!(!table.is_null());
        unsafe {
            // The holder's property takes the array, and the array's only
            // element takes the holder back: one ring, one edge of it
            // inside a table.
            Object::prop_at(holder, 16).write(Value::entity(Tag::Array, table as *mut RcHeader));
            // Retain before the entry is published, which is
            // `Table::insert`'s contract: an entry a walker can reach must
            // already be backed by a count.
            crate::refcount::ll_retain(holder as *mut RcHeader);
            (*table).storage.as_table_mut().insert(
                crate::array::entity::category_of(table),
                Key::Int(0),
                Value::entity(Tag::Object, holder as *mut RcHeader),
            );
        }

        assert!(!unsafe { ll_release(holder as *mut RcHeader) });

        stepped_epoch(); // mature quietly first
        let stats = stepped_epoch();

        assert_eq!(stats.confirmed, 1, "the ring through the array survived");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1);
        let seen = walked_addresses();
        assert!(
            !seen.contains(&(holder as usize)) && !seen.contains(&(table as usize)),
            "a member outlived the drain"
        );
        arena.reset(|_| {});
    }
}

/// A word the walk reads can point anywhere, so an edge into a slot
/// recycled mid-epoch maps to no walked row and one into a slot's
/// interior fails the census division: both are dropped rather than
/// snapped to a row, a fabricated edge being able to balance a live
/// component into collection. A newcomer created after the snapshot
/// is never judged and its store pins its target as a root, and
/// DC1's stale count masked by self-loops is caught twice — by the
/// Phase 3 re-read and, independently, by the Phase 4 exact test.
mod an_edge_the_walk_may_not_record {
    use super::*;

    /// The A8 clause embodied: an edge into a slot reused mid-epoch maps
    /// to no walked row and is dropped — the newcomer in the recycled
    /// slot is never dragged into a component as a phantom non-root.
    #[test]
    fn an_edge_into_a_recycled_slot_is_dropped_not_recorded() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("CollectorRecycled")
            .prop("child", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        // A victim freed BEFORE the epoch: its slot goes to the free
        // list, which can hand it out again mid-epoch — inside the
        // range the snapshot covers.
        let victim = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let victim_addr = victim as usize;
        stepped_epoch(); // holder matures
        unsafe {
            assert!(ll_release(victim as *mut RcHeader));
            crate::object::ll_object_die(victim);
        }

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        // Mid-epoch: the free list hands the victim's slot to a newcomer,
        // and the mature holder points at it.
        let newcomer = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        assert_eq!(
            newcomer as usize, victim_addr,
            "LIFO free list: slot reused"
        );
        unsafe { tie(holder, 16, newcomer) };
        e.walk();
        e.judge();
        assert!(e.stats.dropped_edges >= 1, "the immature edge was dropped");
        assert_eq!(e.stats.candidates, 0);
        let _ = e.close();
        checkpoint();

        let seen = walked_addresses();
        assert!(seen.contains(&(holder as usize)) && seen.contains(&(newcomer as usize)));
        unsafe {
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
        }

        arena.reset(|_| {});
    }

    /// A garbage word can point anywhere; one aimed at the interior of
    /// a live slot must be dropped, not snapped to that slot's row —
    /// the census division validates slot alignment, exactly as the
    /// address map it replaced did by exact-key match.
    #[test]
    fn an_edge_into_a_slot_interior_is_dropped() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("CollectorInterior")
            .prop("child", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let target = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        stepped_epoch(); // both mature
        // A raw store of an interior address wearing an object tag — the
        // torn-ValueBox shape the walker must absorb by validation.
        unsafe {
            Object::prop_at(holder, 16).write(Value::entity(
                Tag::Object,
                (target as usize + 8) as *mut RcHeader,
            ));
        }

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk();
        e.judge();
        assert!(e.stats.dropped_edges >= 1, "the interior edge was dropped");
        assert_eq!(e.stats.candidates, 0);
        let _ = e.close();
        checkpoint();

        unsafe {
            Object::prop_at(holder, 16).write(Value::null());
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
            assert!(ll_release(target as *mut RcHeader));
            crate::object::ll_object_die(target);
        }

        arena.reset(|_| {});
    }

    /// Allocate-black under a live epoch: a newcomer created after the
    /// snapshot is stamped, never judged, and its store pins the mature
    /// target as a root (scenario 4).
    #[test]
    fn a_mid_epoch_newcomer_is_skipped_and_pins_its_target() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("CollectorNewcomer")
            .prop("child", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let target = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        stepped_epoch(); // target matures; its creation ref is the frame's

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        // Mid-epoch: allocate C and hand target's frame reference to it.
        let c = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe { tie(c, 16, target) }; // slot owns the ref the frame held
        e.walk();
        e.judge();
        // The newcomer is either in a block the snapshot never saw
        // (never visited) or in a snapshotted slot (stamped and
        // skipped) — both are allocate-black; either way it contributes
        // no row and no edge.
        assert_eq!(
            e.stats.candidates, 0,
            "target: RC 1 from an unwalked source, IN 0 — a computed root"
        );
        let _ = e.close();
        checkpoint();

        let seen = walked_addresses();
        assert!(seen.contains(&(target as usize)) && seen.contains(&(c as usize)));
        unsafe {
            assert!(ll_release(c as *mut RcHeader));
            crate::object::ll_object_die(c);
        }

        arena.reset(|_| {});
    }

    /// DC1 forced end-to-end (`rfc/model/gc/rc-walk-danger-cases.md`) —
    /// the machine-found trace that defeats a byte-only filter: the walk
    /// reads s2's count, the mutator then inflates `IN` with self-loops
    /// stored **between the count pass and the field pass**, and the
    /// diff reads `crc 2 − in 2 = 0` — the frame reference is exactly
    /// the masked term. The sound design must catch it twice,
    /// independently: the Phase 3 count re-read, and — driven past the
    /// filter, as a broken byte-only confirm would — the Phase 4 exact
    /// test. (The kill itself, freeing s2 under the live frame, is the
    /// TLC battery's job: `MC_dc1.cfg`, 16 states.)
    #[test]
    fn dc1_a_stale_count_masked_by_self_loops_is_caught_twice() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("Dc1Mask")
            .prop("child", true)
            .prop("link", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let mk = |ctx: &mut LLContext| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        let (s1, s2, s3) = (mk(&mut ctx), mk(&mut ctx), mk(&mut ctx));
        unsafe {
            tie(s1, 16, s2); // the ring, f1
            tie(s2, 16, s1);
            tie(s1, 32, s3); // s1.f2 = s3
            ll_retain(s1 as *mut RcHeader); // fr1
        }

        stepped_epoch(); // everything matures

        unsafe {
            // m1: fr2 borrows s2.
            ll_retain(s2 as *mut RcHeader);
            // m2: drop fr1 — the ring is now garbage-shaped.
            assert!(!ll_release(s1 as *mut RcHeader));
            // m3: store(s2.f1, fr2) — first self-loop. Publish first,
            // then drop the displaced s1: it dies, cascading s3.
            ll_retain(s2 as *mut RcHeader);
            tie(s2, 16, s2);
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, s1 as *mut RcHeader);
        }

        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "s1 and s3 died ordinarily"
        );
        assert_eq!(unsafe { (*s2).rc.refcount }, 2, "fr2 + self-loop");

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk_rows(); // crc[s2] = 2, read here and now stale forever
        unsafe {
            // m5: the second self-loop lands between the passes.
            ll_retain(s2 as *mut RcHeader);
            tie(s2, 32, s2);
        }

        e.walk_edges(); // records BOTH self-edges: in[s2] = 2
        e.judge();
        assert_eq!(
            e.stats.candidates, 1,
            "the mask worked: {{s2}} is a candidate"
        );
        e.condemn();
        checkpoint();
        e.recheck_and_post();
        assert_eq!(e.stats.acquitted, 1, "gate 1: the count re-read sees 3 ≠ 2");
        assert_eq!(e.stats.confirmed, 0);
        let _ = e.close();
        checkpoint();
        assert!(walked_addresses().contains(&(s2 as usize)), "s2 lives");
        assert_eq!(
            unsafe { (*s2).rc.refcount },
            3,
            "fr2 + two self-loops, intact"
        );

        // Gate 2, independently: drive the same verdict PAST the filter,
        // exactly what a filterless confirm would post.
        crate::epoch::post_confirmation(vec![s2 as *mut RcHeader]);
        checkpoint();
        assert!(
            walked_addresses().contains(&(s2 as usize)),
            "exact test: 3 ≠ indeg 2"
        );
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "no destructor ran on s2"
        );

        // fr2 lets go: rc 2 = in 2, genuine garbage now.
        assert!(!unsafe { ll_release(s2 as *mut RcHeader) });
        let stats = stepped_epoch();
        assert_eq!(stats.confirmed, 1);
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 3);
        assert!(!walked_addresses().contains(&(s2 as usize)));
        arena.reset(|_| {});
    }
}

/// An epoch abandoned in flight strands nothing: condemnation is
/// private state that dies with it, and `Drop` releases the deferral
/// window that a stuck activity bit would leave parking every free.
/// The other two run the real thing — a collector thread against a
/// mutator that only reaches checkpoints, and a free-running mutator
/// churning garbage through whole epochs — where the races are the
/// design's accepted ones. `measure_epoch_cost` is a probe rather
/// than a test and is ignored.
mod the_epoch_as_a_whole {
    use super::*;

    /// Since the eager-death amendment a collector that condemns and
    /// dies before posting strands nothing: condemnation is private
    /// state and dies with the epoch — no stranded bytes, no owed
    /// acquittals. `Drop` releases the deferral window (a stuck
    /// activity bit would park every free forever) and the ring is
    /// collected cleanly next epoch.
    #[test]
    fn an_abandoned_epoch_strands_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorAbandoned")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }

        stepped_epoch(); // mature

        let mut e = Epoch::open();
        checkpoint();
        e.snapshot();
        e.walk();
        e.judge();
        assert_eq!(e.stats.candidates, 1);
        e.condemn();
        checkpoint(); // ack
        drop(e); // the collector dies before recheck_and_post

        assert_eq!(
            crate::epoch::outstanding_verdicts(),
            0,
            "nothing was posted, nothing is owed"
        );
        assert!(
            !crate::memory::deferred_free::active(),
            "the deferral window was released by the unwind"
        );
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "the ring lives");

        let stats = stepped_epoch();
        assert_eq!(stats.confirmed, 1, "and is collected cleanly next epoch");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        arena.reset(|_| {});
    }

    /// The production shape: a real collector thread runs a full epoch
    /// while the mutator thread does nothing but reach checkpoints.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "spin-waits against a live thread; the stepped tests cover the logic under Miri"
    )]
    fn a_threaded_epoch_collects_a_mature_ring() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorThreaded")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }

        stepped_epoch(); // mature quietly first

        let collector = std::thread::spawn(run_epoch);
        // The mutator: checkpoints until the collector is done. The
        // entities belong to this thread; the drain runs here.
        let stats = loop {
            checkpoint();
            if collector.is_finished() {
                break collector.join().unwrap();
            }

            std::thread::yield_now();
        };

        checkpoint(); // flush parked memory

        assert_eq!(stats.confirmed, 1);
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// Free-running concurrency: the collector loops whole epochs while
    /// the mutator churns — allocating garbage rings, dropping them,
    /// checkpointing only as a side effect of its own allocations. The
    /// races this executes (collector byte stores against mutator word
    /// stores and field stores) are the design's accepted ones, now all
    /// through relaxed atomics; the assertions are the invariants that
    /// must hold whatever the interleaving: the live ring survives,
    /// every garbage destructor runs exactly once, nothing crashes.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "free-running mixed-size atomic races are the design's accepted model gap; Miri rejects them and cannot pace live threads — the stepped tests carry Miri coverage"
    )]
    fn a_free_running_mutator_survives_concurrent_epochs() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("CollectorStress")
            .prop("child", true)
            .prop("link", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let mk = |ctx: &mut LLContext| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };

        // The canary: a frame-held ring that must survive every epoch.
        let (k1, k2) = (mk(&mut ctx), mk(&mut ctx));
        unsafe {
            tie(k1, 16, k2);
            tie(k2, 16, k1);
            ll_retain(k1 as *mut RcHeader);
        }

        let collector = std::thread::spawn(|| {
            let mut stats = Vec::new();
            for _ in 0..4 {
                stats.push(run_epoch());
            }

            stats
        });

        // The mutator: garbage rings and short-lived holders. Bounded —
        // an unthrottled loop out-produces the epochs and the deferral
        // window by orders of magnitude (measured: gigabytes of parked
        // garbage) — and after the bound it just keeps checkpointing.
        // Its own entity allocations are checkpoints too.
        let mut rings = 0usize;
        while !collector.is_finished() {
            if rings < 10_000 {
                let (a, b) = (mk(&mut ctx), mk(&mut ctx));
                unsafe {
                    tie(a, 16, b);
                    tie(b, 16, a);
                    tie(a, 32, k1); // an edge into the live ring, retained
                    ll_retain(k1 as *mut RcHeader);
                    // Both frame references gone: the ring floats, cyclic.
                }

                rings += 1;
                let holder = mk(&mut ctx);
                unsafe {
                    // A fresh holder is never condemned (allocate-black),
                    // so its release always reports the death.
                    assert!(ll_release(holder as *mut RcHeader));
                    crate::object::ll_object_die(holder);
                }
            }

            checkpoint();
            std::hint::spin_loop();
        }

        let epoch_stats = collector.join().unwrap();
        assert_eq!(epoch_stats.len(), 4);

        // Quiesce and sweep what the concurrent epochs did not reach:
        // rings born after the last snapshot need one epoch to mature
        // and one to die.
        checkpoint();
        let seen = walked_addresses();
        assert!(
            seen.contains(&(k1 as usize)) && seen.contains(&(k2 as usize)),
            "the live ring survived every interleaving"
        );
        stepped_epoch();
        stepped_epoch();
        stepped_epoch();
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed) as usize,
            2 * rings + rings, // two ring members and one holder per round
            "every garbage entity destructed exactly once"
        );
        let seen = walked_addresses();
        assert!(seen.contains(&(k1 as usize)) && seen.contains(&(k2 as usize)));

        // The canary dies only when the frame lets go — with the ring
        // references the garbage rings piled on it all dropped.
        assert_eq!(
            unsafe { (*k1).rc.refcount },
            2, // frame + k2's slot: every ring's retain was dropped
            "each collected ring released its edge into the live ring"
        );
        assert!(!unsafe { ll_release(k1 as *mut RcHeader) });
        stepped_epoch();
        stepped_epoch();
        let seen = walked_addresses();
        assert!(!seen.contains(&(k1 as usize)) && !seen.contains(&(k2 as usize)));
        arena.reset(|_| {});
    }

    /// Measurement probe, not a correctness test: collector-side cost
    /// of one epoch by live-set size and shape (`dev/BENCHMARKS.md`,
    /// 2026-07-27 session). Ignored so ordinary runs stay fast; run
    /// explicitly, release mode:
    ///
    /// ```
    /// cargo test --release --lib -- --ignored measure_epoch_cost --nocapture
    /// ```
    ///
    /// Two shapes per size: `singletons` (rows only, no edges) and
    /// `chain` (one traced edge per entity, every node externally
    /// retained so nothing is condemned). Four epochs each; the first
    /// is warm-up — read the later ones.
    #[test]
    #[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
    fn measure_epoch_cost() {
        use std::time::Instant;
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("EpochCost").prop("child", true).build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };

        for &n in &[1_000usize, 10_000, 100_000] {
            for chained in [false, true] {
                let mut objects: Vec<*mut Object> = Vec::with_capacity(n);
                for i in 0..n {
                    let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
                    if chained {
                        // Slot ref + our handle: rc 2, externally live,
                        // never condemned; the walk still chases the edge.
                        unsafe { ll_retain(obj as *mut RcHeader) };
                        if i > 0 {
                            unsafe { tie(objects[i - 1], 16, obj) };
                        }
                    }

                    objects.push(obj);
                }

                for round in 0..4 {
                    let t0 = Instant::now();
                    let mut e = Epoch::open();
                    checkpoint();
                    let t1 = Instant::now();
                    e.snapshot();
                    let t2 = Instant::now();
                    e.walk();
                    let t3 = Instant::now();
                    e.judge();
                    let t4 = Instant::now();
                    assert_eq!(e.stats.candidates, 0, "probe set must stay live");
                    let _ = e.close();
                    checkpoint();
                    let t5 = Instant::now();
                    println!(
                        "epoch_cost n={n} shape={} round={round}: handshake={:?} \
                         snapshot={:?} walk={:?} judge={:?} close+flush={:?} total={:?}",
                        if chained { "chain" } else { "singletons" },
                        t1 - t0,
                        t2 - t1,
                        t3 - t2,
                        t4 - t3,
                        t5 - t4,
                        t5 - t0,
                    );
                }

                // Teardown without a 100k-deep drop cascade: null every
                // slot first (raw writes — the counts the slots held are
                // settled by hand below), then every node dies childless
                // with a uniform two releases: {constructed-or-slot ref,
                // the probe's retain} for chained, one for singletons.
                if chained {
                    for &obj in &objects {
                        unsafe {
                            Object::prop_at(obj, 16).write(Value::null());
                        }
                    }
                }

                for &obj in objects.iter().rev() {
                    if chained {
                        unsafe { ll_release(obj as *mut RcHeader) };
                    }

                    if unsafe { ll_release(obj as *mut RcHeader) } {
                        unsafe { crate::object::ll_object_die(obj) };
                    }
                }
            }
        }
    }
}
