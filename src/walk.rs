//! Entity walking: the kind-dispatched tracer and the heap census —
//! build step 1 of the `rc-walk` cycle collector
//! (`rfc/model/gc/rc-walk.md`, "Build order"). No collector exists yet;
//! what this module delivers is the walking substrate: enumerate every
//! live entity through the region registry, and trace an entity's
//! counted children by its kind without touching `+8` unless the kind
//! carries a class pointer there.
//!
//! Knowledge split: `memory::heap` knows blocks, slots and occupancy
//! ([`for_each_entity_slot`]); this module knows entity kinds and what
//! each kind's out-edges are. Neither knows the other's internals.

use crate::memory::heap::for_each_entity_slot;
use crate::object::{Object, for_each_counted_child};
use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind, RcHeader};

/// Visit every counted child of `entity`, dispatching on the kind bits
/// **before** touching `+8`: only Object (0) and Lazy (6) carry a class
/// pointer there, and reaching for `traced_runs` through a class that
/// does not exist is a wild read (`rfc/model/gc/rc-walk.md`, "What the
/// walker traces").
///
/// Kinds this crate does not yet produce (String, Array, Reference, Box,
/// WeakRef — A2) are skipped, which is conservative: an omitted source
/// only removes in-edges, so its targets are pinned as roots. Array and
/// Reference tracing must land with A2, before the collector ships —
/// String, WeakRef and Box stay skipped by design (no out-edge can close
/// a ring / untraceable C payload).
///
/// # Safety
/// `entity` must point to a live entity whose slots are still readable.
pub unsafe fn trace_entity(entity: *mut RcHeader, visit: impl FnMut(*mut RcHeader)) {
    let flags = unsafe { (*entity).flags };
    let kind = (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
    const OBJECT: u32 = EntityKind::Object as u32;
    const LAZY: u32 = EntityKind::Lazy as u32;
    match kind {
        OBJECT | LAZY => unsafe { for_each_counted_child(entity as *mut Object, visit) },
        _ => {}
    }
}

/// A point-in-time census of the walked entity population.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Occupied entity-block slots.
    pub entities: usize,
    /// Entities per kind code (index = kind bits; 7 is reserved).
    pub by_kind: [usize; 8],
    /// Counted out-edges of walked entities, targets anywhere.
    pub edges: usize,
}

/// Count every live entity in the entity-block population, by kind, with
/// its counted out-edges — the whole-heap leak-detector precursor of
/// build step 2.
///
/// # Safety
/// As [`for_each_entity_slot`]: a quiescent mutator.
pub unsafe fn heap_census() -> Census {
    let mut census = Census::default();
    unsafe {
        for_each_entity_slot(|entity| {
            census.entities += 1;
            let kind = ((*entity).flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
            census.by_kind[kind as usize] += 1;
            trace_entity(entity, |_child| census.edges += 1);
        });
    }
    census
}

// --- Synchronous cycle collection (rc-walk build step 2) -------------------

use crate::refcount::{MemoryCategory, is_object, ll_release};
use std::collections::{HashMap, HashSet};

thread_local! {
    /// Reentrancy guard, mirroring `gc::GC_ACTIVE`: a destructor that
    /// somehow reaches `collect_cycles` again becomes a no-op instead of
    /// re-walking a heap whose guards are outstanding (the drain is not
    /// re-entrant by design — finding F8, `rfc/model/gc/rc-walk-proof.md`).
    static WALK_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Statistics of one synchronous collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollectStats {
    /// GcHeap entities whose refcount and out-edges entered the snapshot.
    pub walked: usize,
    /// Weakly-connected garbage components the mark phase produced.
    pub candidate_components: usize,
    /// Components dropped by the exact test or the post-destructor
    /// re-verify (a resurrection).
    pub acquitted: usize,
    /// Entities freed.
    pub collected: usize,
}

/// The whole-heap synchronous cycle collection — rc-walk build step 2
/// (`rfc/model/gc/rc-walk.md`, "Build order"): Phase 1 walk over the
/// entity blocks, Phase 2 diff and mark in private memory, then the full
/// Phase 4 drain inline — exact test included. No collector thread, no
/// Phase 3 filter: with a quiescent mutator every read is already exact,
/// so condemnation and the handshake have nothing to repair. What this
/// buys today: a whole-heap leak detector that needs no candidate buffer,
/// and the correctness harness for the exact test the concurrent
/// collector (build step 3) will rely on.
///
/// The drain is the discipline `gc::run_cyclic_destructors` proves, minus
/// its restore step — these counts are already real: exact test, guard,
/// pending destructors once each, re-verify discounting the guard
/// (`rc − 1 = indeg`, finding F1), then sever and un-guard through the
/// ordinary teardown path.
///
/// # Safety
/// As [`for_each_entity_slot`], and it must fire at a clean point — where
/// refcounts and physical edges agree, never mid-store or mid-teardown
/// (the arm/fire rule of `rfc/model/gc/strategies.md`).
pub unsafe fn collect_cycles() -> CollectStats {
    if WALK_ACTIVE.with(|a| a.get()) {
        return CollectStats::default();
    }
    struct Active;
    impl Drop for Active {
        fn drop(&mut self) {
            WALK_ACTIVE.with(|a| a.set(false));
        }
    }
    WALK_ACTIVE.with(|a| a.set(true));
    let _active = Active;
    unsafe { collect_cycles_inner() }
}

unsafe fn collect_cycles_inner() -> CollectStats {
    let mut stats = CollectStats::default();

    // Phase 1 — WALK: snapshot the walked population. Only GcHeap
    // entities get a row; every other category is a root source by the
    // corollary of the central identity (its edges appear in RC, never
    // in IN). The acyclic-class skip is not taken yet: the flag is
    // compiler-owed and no compiler exists — recall, not correctness.
    let mut entities: Vec<*mut RcHeader> = Vec::new();
    unsafe {
        for_each_entity_slot(|e| {
            if (*e).memory_category() == MemoryCategory::GcHeap {
                entities.push(e);
            }
        });
    }
    let n = entities.len();
    stats.walked = n;
    let ids: HashMap<usize, u32> = entities
        .iter()
        .enumerate()
        .map(|(i, &e)| (e as usize, i as u32))
        .collect();

    // rc[] and edges[]: a child that maps to no walked row contributes to
    // its target's RC and never to IN — dropped, conservative.
    let mut rc = vec![0u32; n];
    let mut edges: Vec<(u32, u32)> = Vec::new();
    for (i, &e) in entities.iter().enumerate() {
        rc[i] = unsafe { (*e).refcount };
        unsafe {
            trace_entity(e, |child| {
                if let Some(&j) = ids.get(&(child as usize)) {
                    edges.push((i as u32, j));
                }
            });
        }
    }

    // Phase 2 — DIFF and MARK, entirely in private memory. Roots are
    // computed, not enumerated: RC − IN > 0 means something outside the
    // walked population holds the entity.
    let mut in_degree = vec![0u32; n];
    for &(_, dst) in &edges {
        in_degree[dst as usize] += 1;
    }
    let mut marked = vec![false; n];
    let mut stack: Vec<u32> = (0..n as u32)
        .filter(|&i| rc[i as usize] > in_degree[i as usize])
        .collect();
    for &i in &stack {
        marked[i as usize] = true;
    }
    // Forward adjacency (CSR) for the mark walk.
    let mut offsets = vec![0u32; n + 1];
    for &(src, _) in &edges {
        offsets[src as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    let mut forward = vec![0u32; edges.len()];
    let mut cursor = offsets.clone();
    for &(src, dst) in &edges {
        forward[cursor[src as usize] as usize] = dst;
        cursor[src as usize] += 1;
    }
    while let Some(i) = stack.pop() {
        for k in offsets[i as usize]..offsets[i as usize + 1] {
            let j = forward[k as usize];
            if !marked[j as usize] {
                marked[j as usize] = true;
                stack.push(j);
            }
        }
    }

    // Unmarked entities are cyclic garbage, grouped into weakly connected
    // components — followed in both directions, so a garland of linked
    // rings dies as one unit (decided 2026-07-26).
    let mut undirected: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(src, dst) in &edges {
        if !marked[src as usize] && !marked[dst as usize] {
            undirected[src as usize].push(dst);
            undirected[dst as usize].push(src);
        }
    }
    let mut component_of = vec![u32::MAX; n];
    let mut components: Vec<Vec<*mut RcHeader>> = Vec::new();
    for i in 0..n as u32 {
        if marked[i as usize] || component_of[i as usize] != u32::MAX {
            continue;
        }
        let id = components.len() as u32;
        let mut members = Vec::new();
        let mut queue = vec![i];
        component_of[i as usize] = id;
        while let Some(v) = queue.pop() {
            members.push(entities[v as usize]);
            for &w in &undirected[v as usize] {
                if component_of[w as usize] == u32::MAX {
                    component_of[w as usize] = id;
                    queue.push(w);
                }
            }
        }
        components.push(members);
    }
    stats.candidate_components = components.len();

    // Phase 4 — VERIFY and RELEASE, inline. The exact test runs first,
    // for every component, before any guard or destructor mutates
    // anything: counted references account exactly, so
    // `refcount == in-component in-degree` says every reference comes
    // from inside the component — garbage by the central identity.
    let mut confirmed: Vec<Vec<*mut RcHeader>> = Vec::new();
    for members in components {
        if unsafe { exact_test(&members, 0) } {
            confirmed.push(members);
        } else {
            stats.acquitted += 1;
        }
    }

    // Guard every confirmed member (`+= 1`): a release from inside a
    // destructor — of any confirmed component — stops at the guard,
    // never at zero.
    for members in &confirmed {
        for &m in members {
            unsafe { (*m).refcount += 1 };
        }
    }
    // Run each pending `__destruct` exactly once. PHP code: it may store,
    // release, allocate, resurrect — a store retains normally.
    //
    // The `is_object` gates here and below must widen to cover the Lazy
    // kind when A2 starts producing it: a lazy object carries a class
    // pointer and destructs/severs like an object, and the raw-free
    // fallback in `unguard` would leak its children's counts.
    let mut any_destructor_ran = false;
    for members in &confirmed {
        for &m in members {
            if is_object(unsafe { (*m).flags }) {
                any_destructor_ran |=
                    unsafe { crate::object::run_pre_destructor(m as *mut Object) };
            }
        }
    }

    // Re-verify with the guard discounted (`rc − 1 = indeg`, finding F1:
    // without the discount the guard itself acquits every component and
    // nothing is ever freed). A destructor that stored a member anywhere
    // gave it RC > IN beyond the guard — the component is acquitted,
    // guards come off through `ll_release`, survivors live on with true
    // counts and their destructors behind them.
    //
    // Skipped wholesale when no destructor ran anywhere: the only writes
    // since the first exact test were our own guards (+1 each, exactly
    // the discount), so the re-verify would recompute the identical
    // equality. Destructor-less classes are the common case, and this
    // saves the second trace of every component. Global flag, not
    // per-component, so the skip owes nothing to any cross-component
    // reasoning about what a destructor can reach.
    for members in confirmed {
        if any_destructor_ran && !unsafe { exact_test(&members, 1) } {
            stats.acquitted += 1;
            unsafe { unguard(&members, &mut stats) };
            continue;
        }
        // Sever: every member's slots are nulled and the displaced
        // children collected; in-component children are then released
        // (they stop at their guards), while **external children are
        // deferred until the members are freed**. The exact test already
        // proves no external reference to any member exists, so an
        // external `__destruct` could not name a member even if it ran
        // here — but deferring makes that a structural property instead
        // of a proof-dependent one: between sever and free, no user code
        // runs at all, so a resurrected-but-hollowed member cannot exist
        // (the hazard `rfc/model/gc/rc-walk-review.md` leaves open around
        // weak references). Then un-guard: every member reaches a true
        // zero and takes the ordinary free path (`dispose` finds the
        // fields already null and `DESTRUCTOR_RAN` already set).
        let member_set: HashSet<usize> = members.iter().map(|&m| m as usize).collect();
        let mut displaced: Vec<*mut RcHeader> = Vec::new();
        for &m in &members {
            if is_object(unsafe { (*m).flags }) {
                unsafe { crate::object::sever_counted_children(m as *mut Object, &mut displaced) };
            }
        }
        let mut external: Vec<*mut RcHeader> = Vec::new();
        for child in displaced {
            if member_set.contains(&(child as usize)) {
                let died = unsafe { ll_release(child) };
                debug_assert!(!died, "a guarded member cannot die of a sever release");
            } else {
                external.push(child);
            }
        }
        unsafe { unguard(&members, &mut stats) };
        // The members are gone; now the severed external children die
        // ordinarily, destructors and all. Members were GcHeap holders,
        // so the barrier's drop handles an arena escapee's hold-count
        // (`escape_lose`) exactly as member teardown would have.
        for child in external {
            unsafe { crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, child) };
        }
    }
    stats
}

/// The exact test over one component's **current** fields:
/// `refcount == in-component in-degree + discount` for every member
/// (`discount` is 1 while the Phase 4 guard is outstanding, else 0).
unsafe fn exact_test(members: &[*mut RcHeader], discount: u32) -> bool {
    let local: HashMap<usize, u32> = members
        .iter()
        .enumerate()
        .map(|(i, &m)| (m as usize, i as u32))
        .collect();
    let mut in_degree = vec![0u32; members.len()];
    for &m in members {
        unsafe {
            trace_entity(m, |child| {
                if let Some(&j) = local.get(&(child as usize)) {
                    in_degree[j as usize] += 1;
                }
            });
        }
    }
    members
        .iter()
        .enumerate()
        .all(|(i, &m)| unsafe { (*m).refcount } == in_degree[i] + discount)
}

/// Drop the Phase 4 guards through `ll_release` — never a raw `-= 1`: a
/// member that reaches zero dies through the proven teardown; an
/// acquittal survivor keeps its true count and lives on. On the
/// confirmed path every member reaches zero here: external drops are
/// deferred past this point, so nothing can have retained a member since
/// the re-verify.
unsafe fn unguard(members: &[*mut RcHeader], stats: &mut CollectStats) {
    for &m in members {
        if unsafe { ll_release(m) } {
            if is_object(unsafe { (*m).flags }) {
                unsafe { crate::object::ll_object_die(m as *mut Object) };
            } else {
                unsafe { crate::memory::stdapi::ll_free(m as *mut u8) };
            }
            stats.collected += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::memory::arena::Arena;
    use crate::memory::context::LLContext;
    use crate::object::new_constructed;
    use crate::refcount::{MemoryCategory, ll_release};
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
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2, "__destruct ran for both");
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        arena.reset(|_| {});
    }

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
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)), "rooted: must survive");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "no destructor on a live ring");

        assert!(!unsafe { ll_release(a as *mut RcHeader) }, "ring still holds a");
        unsafe { collect_cycles() };
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
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
        let (a, b, c, d, leaf) = (mk(&mut ctx), mk(&mut ctx), mk(&mut ctx), mk(&mut ctx), mk(&mut ctx));
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
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2, "both destructors did run");
        assert_eq!(RESURRECTED.load(Ordering::Relaxed), a as usize);
        let seen = walked_addresses();
        assert!(seen.contains(&(a as usize)) && seen.contains(&(b as usize)));
        assert_eq!(unsafe { (*a).rc.refcount }, 2, "slot + resurrection, guards off");

        // The lasting reference goes away; the ring is garbage again, its
        // destructors already behind it.
        assert!(!unsafe { ll_release(a as *mut RcHeader) });
        unsafe { collect_cycles() };
        let seen = walked_addresses();
        assert!(!seen.contains(&(a as usize)) && !seen.contains(&(b as usize)));
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2, "__destruct exactly once per object");
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
        assert!(seen.contains(&(v as usize)), "externally held: must survive");
        assert_eq!(unsafe { (*v).rc.refcount }, 1, "the ring's reference was dropped");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2, "only the ring destructed");

        unsafe {
            assert!(ll_release(v as *mut RcHeader));
            crate::object::ll_object_die(v);
        }
        arena.reset(|_| {});
    }

    /// The smallest cycle: an object holding itself.
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

    /// The census aggregates what the walk yields; with only objects
    /// produced today, every walked entity reports the Object kind.
    #[test]
    fn census_counts_objects_and_their_edges() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Counted").prop("child", true).build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let before = unsafe { heap_census() };
        let parent = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        let child = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
        unsafe {
            Object::prop_at(parent, 16).write(Value::entity(Tag::Object, child as *mut RcHeader));
        }

        let after = unsafe { heap_census() };
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
