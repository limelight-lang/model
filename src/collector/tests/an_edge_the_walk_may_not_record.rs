//! A word the walk reads can point anywhere, so an edge into a slot
//! recycled mid-epoch maps to no walked row and one into a slot's
//! interior fails the census division: both are dropped rather than
//! snapped to a row, a fabricated edge being able to balance a live
//! component into collection. A newcomer created after the snapshot
//! is never judged and its store pins its target as a root, and
//! DC1's stale count masked by self-loops is caught twice — by the
//! Phase 3 re-read and, independently, by the Phase 4 exact test.

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
