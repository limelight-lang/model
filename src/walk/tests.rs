use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::new_constructed;
use crate::refcount::{MemoryCategory, ll_release};
use crate::test_support::{POOLED_FILLERS, RUN_FILLERS};
use crate::value::{Tag, Value};

/// Collect the addresses the walk currently yields. Tests assert
/// membership, never totals: the registry is process-global, and
/// other tests' leftovers (abandoned blocks with live objects) are
/// legitimately visible here.
fn walked_addresses() -> Vec<usize> {
    let mut seen = Vec::new();
    unsafe { for_each_entity_slot(|e| seen.push(e as usize)) };
    seen
}

// --- collect_cycles (build step 2) -------------------------------

use std::sync::atomic::{AtomicUsize, Ordering};

static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
static RESURRECTED: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
}

/// `$GLOBALS['keep'] = $this;` inside `__destruct`: an ordinary
/// counted store, so the component must be acquitted.
unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
    DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    unsafe { crate::refcount::ll_retain(obj as *mut RcHeader) };
    RESURRECTED.store(obj as usize, Ordering::Relaxed);
}

/// Tie `a.child = b` the way generated code leaves it after
/// `$a->child = $b; unset($b);` — the slot owns one reference.
unsafe fn tie(a: *mut Object, offset: u32, b: *mut Object) {
    unsafe {
        Object::prop_at(a, offset).write(Value::entity(Tag::Object, b as *mut RcHeader));
    }
}

/// [`crate::test_support::wide_class`] with the counting destructor,
/// so the only edge is the one the ring ties.
fn wide_ring_class(name: &str, fillers: usize) -> *const crate::class::Class {
    crate::test_support::wide_class(name, fillers, Some(counting_destructor as *const ()))
}

/// One walk that yields both the aggregate and the addresses behind
/// it, so a census that disagrees with its predecessor can name the
/// entities that came and went instead of only the count.
///
/// # Safety
/// As [`heap_census`]: a quiescent mutator.
unsafe fn census_with_addresses() -> (Census, Vec<(usize, u64)>) {
    let mut census = Census::default();
    let mut addresses = Vec::new();
    unsafe {
        for_each_entity_slot(|entity| {
            census.entities += 1;
            census.by_kind[entity_kind(entity) as usize] += 1;
            addresses.push((entity as usize, *(entity as *const u64)));
            trace_entity(entity, |_child| census.edges += 1);
        });
    }

    (census, addresses)
}

/// Print what left the census and what joined it, each address with
/// the state of the block the enumerator gates on.
///
/// The census flake under investigation (`PLAN.md`) is entities
/// *leaving*: the count fails to grow because a live entity stopped
/// being enumerated, which is the direction that matters — a missed
/// entity contributes none of its out-edges, so its children read as
/// less rooted than they are.
fn report_census_drift(before: &[(usize, u64)], after: &[(usize, u64)]) {
    let before_set: HashSet<usize> = before.iter().map(|&(a, _)| a).collect();
    let after_set: HashSet<usize> = after.iter().map(|&(a, _)| a).collect();
    eprintln!(
        "census drift: {} before, {} after, expected +2",
        before.len(),
        after.len()
    );
    for addr in before_set.difference(&after_set) {
        eprintln!("  LEFT  {}", crate::memory::heap::describe_slot(*addr));
    }

    for addr in after_set.difference(&before_set) {
        eprintln!("  ADDED {}", crate::memory::heap::describe_slot(*addr));
    }
}

/// Occupancy is the refcount word, so an entity is visible from the
/// moment a factory publishes a header and invisible from the
/// instant teardown frees it, and a reserved cell reads exactly as a
/// free slot does. Raw buffers are a separate population the walk
/// never enters. The census aggregates what the walk yields, in
/// deltas rather than totals, the walk being process-global.
mod what_the_walk_enumerates {
    use super::*;

    /// Reserved cells are invisible to the walker until construction
    /// publishes a header (`rfc/model/memory/bulk-operations.md`): a
    /// cell's slot still reads its final `rc 0` (or virgin zero), the
    /// same occupancy answer as a free slot.
    #[test]
    fn a_reserved_cell_is_walker_invisible_until_constructed() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("CellReserved")
            .prop("child", true)
            .build();
        let size = unsafe { (*cls).object_size } as usize;

        let mut cells = [std::ptr::null_mut::<u8>(); 4];
        let mut contiguous = 0usize;
        let n = unsafe {
            crate::memory::heap::ll_entity_reserve(size, 4, cells.as_mut_ptr(), &mut contiguous)
        };

        assert!(n >= 2, "the probe needs at least two cells; got {n}");

        let seen = walked_addresses();
        for &c in &cells[..n] {
            assert!(
                !seen.contains(&(c as usize)),
                "an unconstructed cell was walked"
            );
        }

        let obj = unsafe { crate::object::ll_object_new_in(cells[0], cls) };
        assert!(
            walked_addresses().contains(&(obj as usize)),
            "constructed: walked"
        );

        unsafe { crate::memory::heap::ll_entity_cells_return(cells.as_ptr().add(1), n - 1) };
        assert!(unsafe { ll_release(obj as *mut RcHeader) });
        unsafe { crate::object::ll_object_die(obj) };
    }

    #[test]
    fn walk_sees_gc_objects_and_not_raw_buffers() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Walked").prop("child", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let parent = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let child = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        // Raw C-ABI buffer of a comparable size: must never be walked.
        let buffer = unsafe { crate::memory::stdapi::ll_malloc(40) };

        let seen = walked_addresses();
        assert!(seen.contains(&(parent as usize)), "GcHeap object is walked");
        assert!(seen.contains(&(child as usize)));
        assert!(
            !seen.contains(&(buffer as usize)),
            "a raw buffer must live outside the entity population"
        );

        // The edge is visible to the kind-dispatched tracer.
        unsafe {
            Object::prop_at(parent, 16).write(Value::entity(Tag::Object, child as *mut RcHeader));
            let mut children = Vec::new();
            trace_entity(parent as *mut RcHeader, |c| children.push(c as usize));
            assert_eq!(children, vec![child as usize]);
        }

        // Tear down in dependency order; the child's count is owned by
        // the parent's slot.
        unsafe {
            assert!(ll_release(parent as *mut RcHeader));
            crate::object::ll_object_die(parent);
            crate::memory::stdapi::ll_free(buffer);
        }

        arena.reset(|_| {});
    }

    /// Occupancy is the refcount word: an entity is invisible to the walk
    /// from the instant teardown frees it — no teardown stamp exists to
    /// forget (`rfc/model/gc/rc-walk.md`, the retired FREE stamp).
    #[test]
    fn a_freed_entity_disappears_from_the_walk() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Ephemeral").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let addr = obj as usize;
        assert!(walked_addresses().contains(&addr));

        unsafe {
            assert!(ll_release(obj as *mut RcHeader));
            crate::object::ll_object_die(obj);
        }

        assert!(
            !walked_addresses().contains(&addr),
            "refcount 0 is the occupancy test; the freed slot must read free"
        );
        arena.reset(|_| {});
    }

    /// The census aggregates what the walk yields. The assertions are
    /// deltas, never totals: the walk is process-global.
    #[test]
    fn census_counts_objects_and_their_edges() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Counted").prop("child", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let (before, before_addrs) = unsafe { census_with_addresses() };
        let parent = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let child = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            Object::prop_at(parent, 16).write(Value::entity(Tag::Object, child as *mut RcHeader));
        }

        let (after, after_addrs) = unsafe { census_with_addresses() };
        if after.entities != before.entities + 2 {
            report_census_drift(&before_addrs, &after_addrs);
            for (name, e) in [("parent", parent), ("child", child)] {
                let addr = e as usize;
                let was = before_addrs
                    .iter()
                    .find(|&&(a, _)| a == addr)
                    .map(|&(_, h)| h);
                let now = after_addrs
                    .iter()
                    .find(|&&(a, _)| a == addr)
                    .map(|&(_, h)| h);
                eprintln!(
                    "  {name} header_at_before {was:#x?} header_at_after {now:#x?} {}",
                    crate::memory::heap::describe_slot(addr)
                );
            }
        }

        assert_eq!(after.entities, before.entities + 2);
        assert_eq!(
            after.by_kind[EntityKind::Object as usize],
            before.by_kind[EntityKind::Object as usize] + 2
        );
        assert_eq!(after.edges, before.edges + 1, "the parent→child edge");

        unsafe {
            assert!(ll_release(parent as *mut RcHeader));
            crate::object::ll_object_die(parent);
        }

        arena.reset(|_| {});
    }
}

/// One stride serves every walker, so the two readers must return
/// the same child set on a quiescent heap — the pin the crate lacked
/// while the stride was written out once per walk, which is how the
/// template's moved value count reached two walkers of three and the
/// third was found by review. An array's out-edges are its elements
/// and its string keys alike, a reference box passes an edge
/// through, and a large entity is found in whichever half of the
/// population it landed in.
mod the_children_a_kind_has {
    use super::*;

    /// The `$a->r = &$a` ring: object → reference box → object. The
    /// first non-object kind inside a collected component — the
    /// tracer's reference arm, the drain's reference sever and the kind
    /// switch at death all fire.
    #[test]
    fn a_cycle_through_a_reference_box_is_collected() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("RefRingHolder")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let r = crate::reference::ll_reference_new();
        unsafe {
            // a.child owns the box's initial ref; the box owns a's.
            Object::prop_at(a, 16).write(Value::entity(Tag::Reference, r as *mut RcHeader));
            (*r).value = Value::entity(Tag::Object, a as *mut RcHeader);
        }

        let census = unsafe { heap_census() };
        assert!(
            census.by_kind[EntityKind::Reference as usize] >= 1,
            "the box is walked"
        );

        unsafe { collect_cycles() };
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)), "the object died");
        assert!(!seen.contains(&(r as usize)), "the box died");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "a's destructor ran");
        arena.reset(|_| {});
    }

    /// An array's out-edges are its elements **and** its string keys: the
    /// table holds a reference to each string it keys on, so a walk that
    /// counts only elements under-counts the in-edges of every key and
    /// pins it as a root. Before the Array arm existed the walk yielded
    /// nothing at all here, which is conservative but makes a ring
    /// through an array uncollectable.
    #[test]
    fn an_array_is_traced_through_its_elements_and_its_string_keys() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::string::ll_string_new;
        let _g = crate::memory::block_pool::test_guard();

        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let key = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, b"k") };
        let value = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, b"v") };
        unsafe {
            crate::array::testing::insert(
                a,
                Key::Str(key),
                Value::entity(Tag::String, value as *mut RcHeader),
            );
            // An integer key with a plain value adds no edge of its own,
            // and a hole must add none either.
            crate::array::testing::insert(a, Key::Int(7), Value::int(7));
            let _ = crate::array::testing::remove(a, Key::Int(7));
        }

        let mut seen = Vec::new();
        unsafe { trace_entity(a as *mut RcHeader, |child| seen.push(child as usize)) };

        assert!(
            seen.contains(&(key as usize)),
            "the string key is not an out-edge"
        );
        assert!(
            seen.contains(&(value as usize)),
            "the element is not an out-edge"
        );
        assert_eq!(seen.len(), 2, "a hole or an integer key produced an edge");

        unsafe {
            crate::array::entity::dispose_storage(a, crate::array::entity::category_of(a));
            // Each of the three is released before it is killed, so its
            // slot reaches the free list carrying the refcount-0 header
            // the process-global enumerators use as their occupancy test.
            // Killing at 1 leaves a freed slot that every later census in
            // the process reads as a live entity (`PLAN.md`, the census
            // flake).
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_entity_die(a as *mut RcHeader);
            assert!(ll_release(key as *mut RcHeader));
            crate::object::ll_entity_die(key as *mut RcHeader);
            assert!(ll_release(value as *mut RcHeader));
            crate::object::ll_entity_die(value as *mut RcHeader);
        }
    }

    /// The collector's reader sees an array's children. This is the arm
    /// item 12 exists for: until it landed the concurrent tracer took the
    /// empty default on kind 2, so an array's out-edges were invisible to
    /// the epoch while its in-edge was not.
    #[cfg(feature = "rc-walk")]
    #[test]
    fn the_relaxed_reader_sees_an_arrays_children() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::string::ll_string_new;
        let _g = crate::memory::block_pool::test_guard();

        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let key = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, b"k") };
        let value = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, b"v") };
        unsafe {
            // Retained before the entry is published, per `Table::insert`.
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(value as *mut RcHeader);
            crate::array::testing::insert(
                a,
                Key::Str(key),
                Value::entity(Tag::String, value as *mut RcHeader),
            );
        }

        let mut seen = Vec::new();
        unsafe {
            trace_cells::<RelaxedCells>(a as *mut RcHeader, EntityKind::Array as u32, |c| {
                seen.push(c.child as usize)
            })
        };

        assert!(
            seen.contains(&(key as usize)),
            "the string key is not an out-edge for the collector"
        );
        assert!(
            seen.contains(&(value as usize)),
            "the element is not an out-edge for the collector"
        );

        unsafe {
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_entity_die(a as *mut RcHeader);
            assert!(ll_release(key as *mut RcHeader));
            crate::object::ll_entity_die(key as *mut RcHeader);
            assert!(ll_release(value as *mut RcHeader));
            crate::object::ll_entity_die(value as *mut RcHeader);
        }
    }

    /// The two readers must agree on a quiescent heap, for every walked
    /// entity and every kind that is producible.
    ///
    /// This is the pin the crate lacked. The stride used to be written
    /// out once per walk, so the walks could disagree and nothing said
    /// so: when the interpolated template moved its value count from the
    /// class to the instance, two walkers learned it and the third did
    /// not, and the miss was a leak rather than a crash — found by
    /// review, not by the suite. With one stride under two readers a
    /// divergence can only come from the readers, and this catches that;
    /// with two strides it would catch their divergence too.
    ///
    /// Quiescent is the whole precondition: the relaxed reader exists for
    /// a racing mutator, and here there is none, so the two must return
    /// the same set rather than merely compatible ones.
    #[cfg(feature = "rc-walk")]
    #[test]
    fn both_readers_agree_on_a_quiet_heap() {
        use crate::class::ClassBuilder;
        use crate::memory::arena::Arena;
        use crate::memory::context::LLContext;
        use crate::object::new_constructed;
        let _g = crate::memory::block_pool::test_guard();

        let cls = ClassBuilder::new("TwoReaders")
            .prop("a", true)
            .prop("b", true)
            .build();
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let child = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let boxed = crate::reference::ll_reference_new();
        unsafe {
            tie(holder, 16, child);
            // The second property stays empty, so an unoccupied cell has
            // to be skipped by both readers rather than by one.
            crate::memory::barrier::write_value_slot(
                &raw mut (*boxed).value,
                Value::entity(Tag::Object, child as *mut RcHeader),
            );
            crate::refcount::ll_retain(child as *mut RcHeader);
        }

        let mut disagreed = Vec::new();
        let mut walked = 0usize;
        unsafe {
            for_each_entity_slot(|entity| {
                walked += 1;
                let kind = entity_kind(entity);
                let mut plain = Vec::new();
                let mut relaxed = Vec::new();
                trace_cells::<PlainCells>(entity, kind, |c| plain.push(c.child as usize));
                trace_cells::<RelaxedCells>(entity, kind, |c| relaxed.push(c.child as usize));
                if plain != relaxed {
                    disagreed.push((entity as usize, plain, relaxed));
                }
            });
        }

        assert!(walked >= 3, "the heap under test was empty");
        assert!(
            disagreed.is_empty(),
            "the two readers disagree: {disagreed:?}"
        );

        // Everything this test made has to go: an entity left alive holds
        // its block out of the pool for the rest of the run, and a later
        // test asking for a fresh block gets a different heap shape than
        // it would alone. The box's Value goes first, then the holder's
        // dispose releases the child last.
        unsafe {
            use crate::refcount::ll_release;
            assert!(ll_release(boxed as *mut RcHeader));
            crate::object::ll_entity_die(boxed as *mut RcHeader);
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_entity_die(holder as *mut RcHeader);
        }

        arena.reset(|_| {});
    }

    /// A ring whose two nodes are both too large for any size class, one
    /// in each half of the population: a pooled block of its own, which
    /// the region scan reaches, and an OS-direct run, which no region
    /// contains and only `large_entity`'s registry names. The walk has to
    /// find both, and it has to count one slot in each — a stride would
    /// read rows out of the nodes' own cells.
    #[test]
    fn a_cycle_through_large_entities_is_walked_and_collected() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let pooled_cls = wide_ring_class("PooledRingNode", POOLED_FILLERS);
        let run_cls = wide_ring_class("RunRingNode", RUN_FILLERS);

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, pooled_cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, run_cls, MemoryCategory::GcHeap) };
        unsafe {
            let kind_of = |o: *mut Object| {
                *(((o as usize) & !crate::memory::block_pool::BLOCK_MASK) as *const u32)
            };

            assert_eq!(
                kind_of(a),
                crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE,
                "the smaller node is a pooled block of its own"
            );
            assert_eq!(
                kind_of(b),
                crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN,
                "and the larger one is a run"
            );
            tie(a, 16, b);
            tie(b, 16, a);
        }

        let seen = walked_addresses();
        assert!(
            seen.contains(&(a as usize)) && seen.contains(&(b as usize)),
            "both halves of the population are enumerated"
        );

        let stats = unsafe { collect_cycles() };
        assert!(stats.collected >= 2, "the ring is garbage");
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "__destruct ran for both"
        );
        let after = walked_addresses();
        assert!(!after.contains(&(a as usize)) && !after.contains(&(b as usize)));
        arena.reset(|_| {});
    }
}

/// A ring with no external reference is what no refcount path can
/// reclaim and the whole of this collector's job; two rings joined
/// by one edge are a single weakly-connected component and die
/// together with the acyclic subtree they hold. Neither an object
/// nor a candidate buffer has to appear anywhere in the ring: this
/// walk finds it from the entity blocks alone.
mod what_the_collection_reclaims {
    use super::*;

    /// A pure two-object ring with no external references is exactly what
    /// no refcount path can reclaim — and the whole of this collector's
    /// job. No candidate buffer is involved: the walk finds it from the
    /// entity blocks alone (the leak-detector property).
    #[test]
    fn a_pure_cycle_is_collected_and_destructed() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("RingNode")
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

        let stats = unsafe { collect_cycles() };
        assert!(stats.collected >= 2, "the ring is garbage");
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "__destruct ran for both"
        );
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

    /// The smallest cycle: an object holding itself. Doubles as DC5's
    /// runtime-side witness: the test body's raw pointer is exactly an
    /// uncounted borrow, and it does not root — the compiler obligation
    /// (`rfc/model/memory/static-lifetimes.md`, "What may own a borrow")
    /// is the only thing that makes such borrows legal.
    #[test]
    fn a_self_loop_is_collected() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("SelfLoop").prop("child", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe { tie(a, 16, a) };

        unsafe { collect_cycles() };
        assert!(!walked_addresses().contains(&(a as usize)));
        arena.reset(|_| {});
    }

    /// Two rings joined by one edge are ONE weakly-connected component
    /// and die together in a single collection, along with a hanging
    /// acyclic subtree the ring holds (its counts balance inside the
    /// component, so the exact test covers it too).
    #[test]
    fn a_garland_and_its_hanging_subtree_die_as_one_unit() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("GarlandNode")
            .prop("child", true)
            .prop("link", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let mk = |ctx: &mut LLContext| unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };
        let (a, b, c, d, leaf) = (
            mk(&mut ctx),
            mk(&mut ctx),
            mk(&mut ctx),
            mk(&mut ctx),
            mk(&mut ctx),
        );
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a); // ring 1
            tie(c, 16, d);
            tie(d, 16, c); // ring 2
            crate::refcount::ll_retain(c as *mut RcHeader);
            tie(b, 32, c); // garland link: c now held by d's slot and b's slot
            tie(d, 32, leaf); // hanging subtree off ring 2
        }

        let stats = unsafe { collect_cycles() };
        assert!(stats.collected >= 5, "both rings and the leaf died");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 5);
        let seen = walked_addresses();
        for &o in &[a, b, c, d, leaf] {
            assert!(!seen.contains(&(o as usize)));
        }

        arena.reset(|_| {});
    }

    /// A ring with no object anywhere in it. Phase 1 traced both arrays
    /// and Phase 2 judged the component garbage; what was missing was the
    /// arm that severs it, so the drain guarded the two members, severed
    /// nothing, and un-guarded them back to the counts they started at.
    /// The failure was not a crash and not a leak the next pass finds: the
    /// collector reported a confirmed component and freed none of it, and
    /// repeated the whole walk, judge, guard and un-guard on the same ring
    /// on every later call. Seen failing at `collected: 0`.
    #[test]
    fn a_ring_of_two_arrays_and_no_object_is_collected() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::refcount::{ll_release, ll_retain};
        let _g = crate::memory::block_pool::test_guard();

        let a = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let b = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            // Retained before the entry is published, per `Table::insert`.
            ll_retain(b as *mut RcHeader);
            crate::array::testing::insert(
                a,
                Key::Int(0),
                Value::entity(Tag::Array, b as *mut RcHeader),
            );
            ll_retain(a as *mut RcHeader);
            crate::array::testing::insert(
                b,
                Key::Int(0),
                Value::entity(Tag::Array, a as *mut RcHeader),
            );
            // Drop the creation references: each array is now held by the
            // other and by nothing else, which is the ring.
            assert!(!ll_release(a as *mut RcHeader), "a is still held by b");
            assert!(!ll_release(b as *mut RcHeader), "b is still held by a");
        }

        let stats = unsafe { collect_cycles() };
        // At least one, not exactly one: `collect_cycles` measures the
        // whole process and the tests share it, so an exact count is a
        // claim about every other test rather than about this ring. The
        // proof that severing happened is `collected` below, which is
        // what read zero before the arm existed.
        assert!(
            stats.candidate_components >= 1,
            "the ring was not judged garbage, so this proves nothing about severing"
        );
        assert!(
            stats.collected >= 2,
            "the component was confirmed and then not freed"
        );
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
    }

    /// The rc-walk twin of
    /// `gc::tests::a_ring_whose_last_release_lands_on_a_reference_box_is_collected`:
    /// `$a[0] = &$a`, with the ring's only external hold landing on the
    /// box. This walk computes its roots and buffers no candidates, so
    /// which kind took the last decrement cannot reach it. The twin runs
    /// to check that independence rather than assume it.
    #[test]
    fn a_ring_through_a_reference_box_and_an_array_is_collected() {
        use crate::array::entity::ll_array_new;
        use crate::array::table::Key;
        use crate::refcount::{ll_release, ll_retain};
        let _g = crate::memory::block_pool::test_guard();

        let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let boxed = crate::reference::ll_reference_new();
        unsafe {
            // The box takes the array's creation reference, as `&$a` does.
            (*boxed).value = Value::entity(Tag::Array, array as *mut RcHeader);
            // Retained before the entry is published, per `Table::insert`.
            ll_retain(boxed as *mut RcHeader);
            crate::array::testing::insert(
                array,
                Key::Int(0),
                Value::entity(Tag::Reference, boxed as *mut RcHeader),
            );
            assert!(
                !ll_release(boxed as *mut RcHeader),
                "the box is still held by the array's element"
            );
        }

        let stats = unsafe { collect_cycles() };
        assert!(
            stats.candidate_components >= 1,
            "the ring was not judged garbage, so this proves nothing about severing"
        );
        assert!(
            stats.collected >= 2,
            "the component was confirmed and then not freed"
        );
        let seen = walked_addresses();
        assert!(!seen.contains(&(array as usize)) && !seen.contains(&(boxed as usize)));
    }
}

/// `RC - IN > 0` is the central identity: one external reference
/// keeps a ring alive, and self-edges inflating `IN` may not mask
/// it. Its corollary is that an entity the walk does not enumerate
/// becomes a root source, which is why a ring among promoted
/// survivors was uncollectable until the reset kept its object index
/// — skipping costs recall and never correctness.
mod what_roots_a_component {
    use super::*;

    /// One external counted reference is a computed root (`RC − IN > 0`):
    /// the ring survives untouched, and dies on the collect after the
    /// reference is dropped.
    #[test]
    fn a_rooted_cycle_survives_until_the_root_is_dropped() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("RootedRing")
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
            crate::refcount::ll_retain(a as *mut RcHeader); // the frame slot
        }

        unsafe { collect_cycles() };
        let seen = walked_addresses();
        assert!(
            seen.contains(&(a as usize)) && seen.contains(&(b as usize)),
            "rooted: must survive"
        );
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            0,
            "no destructor on a live ring"
        );

        assert!(
            !unsafe { ll_release(a as *mut RcHeader) },
            "ring still holds a"
        );
        unsafe { collect_cycles() };
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
        arena.reset(|_| {});
    }

    /// DC1's arithmetic seed (`rfc/model/gc/rc-walk-danger-cases.md`):
    /// self-edges inflate IN, and the diff must still see the frame
    /// reference. Two self-edges + one external ref: `rc 3 > in 2` —
    /// root, survives; drop the external ref and `rc 2 = in 2` —
    /// collected. (The full DC1 kill needs stale concurrent reads and
    /// the `byte_only` broken variant — build step 3 material; the TLC
    /// battery already kills it at the model level.)
    #[test]
    fn self_edges_do_not_mask_an_external_reference() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("SelfLooper")
            .prop("child", true)
            .prop("link", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let s = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(s, 16, s); // self-edge 1 owns the initial ref
            crate::refcount::ll_retain(s as *mut RcHeader);
            tie(s, 32, s); // self-edge 2 owns the second
            crate::refcount::ll_retain(s as *mut RcHeader); // the frame ref
        }

        unsafe { collect_cycles() };
        assert!(
            walked_addresses().contains(&(s as usize)),
            "rc 3 > in 2: the frame reference must root it"
        );

        assert!(!unsafe { ll_release(s as *mut RcHeader) });
        unsafe { collect_cycles() };
        assert!(
            !walked_addresses().contains(&(s as usize)),
            "rc 2 = in 2: garbage"
        );
        arena.reset(|_| {});
    }

    /// The corollary of the central identity, DC2's sound half: an
    /// un-walked holder (here LongLived — no `rc[]` row, no recorded
    /// edge) pins its GcHeap child as a root. Skipping costs recall,
    /// never correctness.
    #[test]
    fn an_unwalked_holder_roots_its_gc_heap_child() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("UnwalkedHolder")
            .prop("child", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::LongLived) };
        let child = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe { tie(holder, 16, child) }; // holder's slot owns child's ref

        unsafe { collect_cycles() };
        assert!(
            walked_addresses().contains(&(child as usize)),
            "the holder's edge is in RC and never in IN: child is a computed root"
        );

        // Cleanup. LongLived has no teardown policy yet; free the pair by
        // hand, keeping the entity-block invariant that a slot is freed
        // only at refcount zero (bytes 0–7 survive the free and ARE the
        // occupancy test — a nonzero header in a freed slot would fake a
        // live entity to every later walk).
        unsafe {
            assert!(ll_release(child as *mut RcHeader));
            crate::object::ll_object_die(child);
            (*holder).rc.refcount = 0;
            crate::memory::stdapi::ll_free(holder as *mut u8);
        }

        arena.reset(|_| {});
    }

    /// A live acyclic graph must pass through the collector untouched —
    /// membership asserts, not totals: the walk is process-global.
    #[test]
    fn a_live_graph_is_not_collected() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("LiveNode").prop("child", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let root = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let leaf = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe { tie(root, 16, leaf) };

        unsafe { collect_cycles() };
        let seen = walked_addresses();
        assert!(seen.contains(&(root as usize)) && seen.contains(&(leaf as usize)));

        unsafe {
            assert!(ll_release(root as *mut RcHeader));
            crate::object::ll_object_die(root);
        }

        arena.reset(|_| {});
    }

    /// A ring that survived an arena reset lives in retained blocks,
    /// which an arena's bump allocator left with mixed sizes and no
    /// stride. Until the reset kept its survivor list as the block's
    /// object index, those occupants were never enumerated: they were
    /// root sources by the derived-roots corollary, their out-edges
    /// landed in `RC` and never in `IN`, and the ring was uncollectable
    /// forever (`rfc/model/gc/retained-block-walk.md`).
    #[test]
    fn a_ring_among_promoted_survivors_is_collected() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let node = ClassBuilder::new("PromotedRing")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();
        let holder_cls = ClassBuilder::new("PromotedRingHolder")
            .prop("head", true)
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe { new_constructed(&mut ctx, holder_cls, MemoryCategory::GcHeap) };
        let a = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };
        let b = unsafe { new_constructed(&mut ctx, node, MemoryCategory::RequestArena) };

        unsafe {
            tie(a, 16, b); // arena→arena, no log
            tie(b, 16, a);
            // The escape that promotes `a`, and `b` behind it.
            let slot = Object::prop_at(holder, 16);
            assert!(crate::memory::barrier::ref_store(
                &mut arena,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, a as *mut RcHeader),
            ));
        }

        unsafe { crate::promote::arena_reset_full(&mut arena) };

        let block = crate::memory::block_pool::BlockHeader::of_ptr(a as *const u8);
        assert_eq!(
            unsafe { (*block).kind.load(Ordering::Relaxed) },
            crate::memory::block_pool::BLOCK_KIND_RETAINED,
            "the survivors' block is retained, not returned"
        );

        // Drop the last external hold. The ring is now pure garbage
        // that no refcount path can reach — and it is not in an entity
        // block, which is the whole point of the test.
        unsafe {
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
        }

        let seen = walked_addresses();
        assert!(
            seen.contains(&(a as usize)) && seen.contains(&(b as usize)),
            "a retained block's occupants must be enumerable"
        );

        let stats = unsafe { collect_cycles() };
        assert!(stats.collected >= 2, "the promoted ring is garbage");
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "__destruct ran for both"
        );
    }
}

/// A destructor that stores `$this` somewhere lasting gives its
/// member `RC > IN` beyond the guard, so the re-verify acquits the
/// whole component and the destructor is never run again. A child
/// held from outside survives the ring and loses exactly the ring's
/// one reference, through the deferred drop that runs after the
/// members are freed. A member reading refcount 0 died ordinarily
/// since the verdict was posted, so the message is dropped whole
/// before any field is traced or guard written.
mod what_the_drain_meets_when_it_arrives {
    use super::*;

    /// A destructor that stores `$this` somewhere lasting gives the
    /// member `RC > IN` beyond the guard: the re-verify acquits the whole
    /// component, survivors keep true counts, and the destructor is
    /// never run again — the ring dies silently once the resurrection
    /// reference is dropped.
    #[test]
    fn a_resurrecting_destructor_acquits_and_never_reruns() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let lazarus_cls = ClassBuilder::new("WalkLazarus")
            .prop("child", true)
            .destructor(resurrecting_destructor as *const ())
            .build();
        let plain_cls = ClassBuilder::new("WalkLazarusPeer")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, lazarus_cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, plain_cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
        }

        let stats = unsafe { collect_cycles() };
        assert_eq!(stats.collected, 0, "resurrection must acquit the component");
        assert!(stats.acquitted >= 1);
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "both destructors did run"
        );
        assert_eq!(RESURRECTED.load(Ordering::Relaxed), a as usize);
        let seen = walked_addresses();
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));
        assert_eq!(
            unsafe { (*a).rc.refcount },
            2,
            "slot + resurrection, guards off"
        );

        // The lasting reference goes away; the ring is garbage again, its
        // destructors already behind it.
        assert!(!unsafe { ll_release(a as *mut RcHeader) });
        unsafe { collect_cycles() };
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "__destruct exactly once per object"
        );
        arena.reset(|_| {});
    }

    /// A ring member holding a child that is ALSO externally held: the
    /// child is live (a computed root keeps it marked), survives the
    /// ring's death, and loses exactly the ring's one reference — through
    /// the deferred external drop that runs only after the members are
    /// freed.
    #[test]
    fn a_live_external_child_survives_the_ring_and_loses_one_reference() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("RingWithTenant")
            .prop("child", true)
            .prop("link", true)
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let a = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let b = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let v = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            tie(a, 16, b);
            tie(b, 16, a);
            crate::refcount::ll_retain(v as *mut RcHeader); // the live frame slot
            tie(a, 32, v); // the ring's reference
        }

        unsafe { collect_cycles() };
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        assert!(
            seen.contains(&(v as usize)),
            "externally held: must survive"
        );
        assert_eq!(
            unsafe { (*v).rc.refcount },
            1,
            "the ring's reference was dropped"
        );
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            2,
            "only the ring destructed"
        );

        unsafe {
            assert!(ll_release(v as *mut RcHeader));
            crate::object::ll_object_die(v);
        }

        arena.reset(|_| {});
    }

    /// The corpse rule (eager-death amendment, 2026-07-27): the drain
    /// header-scans before it trusts — any member reading `rc 0` died
    /// ordinarily since the verdict was posted, and the message is
    /// dropped whole before any field is traced or guard written. No
    /// second teardown, no destructor re-run, and the live peer is
    /// untouched.
    #[cfg(feature = "rc-walk")]
    #[test]
    fn a_corpse_in_a_posted_component_drops_the_message_whole() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn counting(_o: *mut crate::object::Object) {
            DESTRUCTS.fetch_add(1, Ordering::Relaxed);
        }

        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("PostedCorpse")
            .destructor(counting as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let peer = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let ptr = obj as *mut RcHeader;

        // Epoch active: the ordinary death parks the slot instead of
        // recycling it — identity holds from walk to drain.
        crate::memory::deferred_free::begin_epoch();
        assert!(unsafe { ll_release(ptr) });
        unsafe { crate::object::ll_object_die(obj) };
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            1,
            "died ordinarily, eagerly, once"
        );

        // The stale message arrives naming the corpse and a live peer.
        let outcome = unsafe { drain_confirmed(&[ptr, peer as *mut RcHeader]) };
        assert!(outcome.acquitted, "the corpse drops the message whole");
        assert_eq!(outcome.collected, 0, "nothing is torn by a drop");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "no destructor re-run");
        assert_eq!(
            unsafe { crate::refcount::header_refcount(peer as *mut RcHeader) },
            1,
            "the live peer is untouched — no guard was written"
        );

        crate::memory::deferred_free::end_epoch();
        assert!(unsafe { crate::memory::deferred_free::flush() } >= 1);
        unsafe {
            assert!(ll_release(peer as *mut RcHeader));
            crate::object::ll_object_die(peer);
        }

        arena.reset(|_| {});
    }
}
