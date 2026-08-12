//! `RC - IN > 0` is the central identity: one external reference
//! keeps a ring alive, and self-edges inflating `IN` may not mask
//! it. Its corollary is that an entity the walk does not enumerate
//! becomes a root source, which is why a ring among promoted
//! survivors was uncollectable until the reset kept its object index
//! — skipping costs recall and never correctness.

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
