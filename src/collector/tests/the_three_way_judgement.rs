//! A frame-held ring is a computed root in every epoch, and a ring
//! born under one is stamped new there, so it matures in that epoch
//! and dies in the next. Phase 3 acquits a component the mutator
//! touched between condemn and re-check, while a borrow taken and
//! returned restores the snapshot exactly and confirms — correctly,
//! the Phase 4 exact test proving every reference is in-component at
//! drain time.
//!
//! **Reverting a fix to watch a test here go red needs a timeout.** A
//! failed assertion between the post and the drain leaves the epoch's
//! verdicts unacked, and `Epoch::drop` waits for that ack while the
//! panic has taken the mutator off the checkpoint path — so the run
//! prints the assertion and then hangs rather than ending. That is the
//! harness rather than these tests, and it is what makes a bare
//! `cargo test` the wrong instrument for the check.

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

/// The Phase 3 filter reads the whole cell rather than its first
/// half: a sixteen-byte `Value` is published as two relaxed stores,
/// so the flags beside the payload have to still say "entity" at
/// re-check time, and a cell that stopped holding one acquits the
/// component it was recorded for.
///
/// The half-landed write is applied here by hand — the second word
/// of `$a->child = 1` without its first — because the two stores are
/// relaxed and no scheduling reaches their reordering on demand.
/// What the test pins is therefore Phase 3's contract rather than
/// the interleaving that arrives at it: the recorded cell no longer
/// designates a counted child, and any difference acquits.
///
/// That interleaving is what makes the contract worth having. A walk
/// that reads the new payload against the old flags records an edge
/// to whatever row the program's integer names, and the payload word
/// re-reads unchanged afterwards — the payload store being the one
/// that had landed — so eight bytes confirm a component holding an
/// entity nobody points at.
#[test]
fn a_cell_that_stopped_holding_an_entity_acquits() {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("CollectorHalfWritten")
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

    // The flags of `a`'s recorded cell stop saying "entity" while
    // its payload word still reads `b`.
    let meta_at = unsafe { (Object::prop_at(a, 16) as *const u8).add(8) } as *const AtomicU64;
    let flags = unsafe { (*meta_at).load(Ordering::Relaxed) };
    unsafe { (*meta_at).store(Value::int(1).into_words()[1], Ordering::Relaxed) };

    e.recheck_and_post();
    unsafe { (*meta_at).store(flags, Ordering::Relaxed) };
    assert_eq!(
        e.stats.acquitted, 1,
        "the cell no longer holds the child the walk recorded"
    );
    assert_eq!(e.stats.confirmed, 0);
    let _ = e.close();
    checkpoint(); // flush
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        0,
        "acquitted: nothing died"
    );

    let stats = stepped_epoch();
    assert_eq!(stats.confirmed, 1, "restored and untouched: collected");
    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 2);
    arena.reset(|_| {});
}

/// A ring that runs through an array is garbage like any other, and
/// the epoch could not see it until the array's entries could be read
/// coherently: the edge *into* the array was counted and the edge
/// *out* was not, so the holder read `RC` above `IN` and was a
/// computed root every epoch, forever.
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
        crate::array::testing::insert(
            table,
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

/// An array that moved its entries between the walk and the re-check
/// leaves the recorded cell in a chunk the epoch parks: intact,
/// written by nobody, and reading the walk's own value for the rest
/// of the epoch. So the re-read agrees with itself while the array
/// has given the child away, and the count agrees too — the move out
/// is a retain and a release around it.
///
/// Nothing about the cell can see this, which is why the walk keeps
/// the storage version beside it. Growth here is
/// the mover; compaction and the migration between representations
/// are the same event to the head.
///
/// **What the check disabled costs is a verdict, not a member.** Run
/// this way it reports `acquitted 0, confirmed 1` and then Phase 4's
/// exact test drops the message on the current chunk, where entry
/// zero is a hole: no destructor runs and the live array keeps its
/// element. That measurement is why the assertion below is on the
/// counters rather than on a member outliving the drain — the
/// counters are what this defect reaches.
#[test]
fn a_component_whose_array_moved_its_entries_is_acquitted() {
    use crate::array::entity::ll_array_new;
    use crate::array::table::Key;
    use crate::array::testing;
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTS.store(0, Ordering::Relaxed);
    let cls = ClassBuilder::new("CollectorMovedEntries")
        .prop("table", true)
        .destructor(counting_destructor as *const ())
        .build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::GcHeap) };
    let ring = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    // The holder the member ends up in. It keeps the count its
    // factory returned and nothing points at it, so it is a root
    // rather than a candidate.
    let live = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    assert!(!ring.is_null() && !live.is_null());
    unsafe {
        Object::prop_at(holder, 16).write(Value::entity(Tag::Array, ring as *mut RcHeader));
        crate::refcount::ll_retain(holder as *mut RcHeader);
        testing::insert(
            ring,
            Key::Int(0),
            Value::entity(Tag::Object, holder as *mut RcHeader),
        );
    }

    assert!(!unsafe { ll_release(holder as *mut RcHeader) });

    stepped_epoch(); // mature

    let mut e = Epoch::open();
    checkpoint();
    e.snapshot();
    e.walk();
    e.judge();
    assert_eq!(e.stats.candidates, 1, "the ring through the array");
    e.condemn();

    // Integer keys until the entries move, which is what parks the
    // chunk the walk recorded an address in.
    let chunk = unsafe { testing::storage_and_capacity(ring).0 };
    let mut key = 1i64;
    while unsafe { testing::storage_and_capacity(ring).0 } == chunk {
        assert!(key < 1000, "the table never grew");
        unsafe { testing::insert(ring, Key::Int(key), Value::int(key)) };
        key += 1;
    }

    // The member moves to a holder outside the component, by the
    // three steps a move takes: the count goes up, the entry hands
    // its reference over, the borrow ends. The walk's row for the
    // holder therefore reads back exactly.
    unsafe {
        crate::refcount::ll_retain(holder as *mut RcHeader);
        let (value, string_key) = testing::remove(ring, Key::Int(0)).expect("the entry");
        assert!(string_key.is_null(), "an integer key holds no string");
        testing::insert(live, Key::Int(0), value);
        assert!(!ll_release(holder as *mut RcHeader));
    }

    checkpoint(); // ack
    e.recheck_and_post();
    assert_eq!(
        e.stats.acquitted, 1,
        "the recorded cell is in a chunk the array has left"
    );
    assert_eq!(e.stats.confirmed, 0);
    let _ = e.close();
    checkpoint(); // flush
    assert_eq!(
        DESTRUCTS.load(Ordering::Relaxed),
        0,
        "acquitted: the member a live array holds is untouched"
    );
    assert_eq!(
        unsafe { testing::get(live, Key::Int(0)) }
            .expect("the live array still holds it")
            .entity_ptr(),
        holder as *mut RcHeader
    );

    // The chain gives itself back: the live array's last reference
    // goes, which releases the member, which releases the ring.
    unsafe {
        assert!(ll_release(live as *mut RcHeader), "the test was its holder");
        crate::object::ll_entity_die(live as *mut RcHeader);
    }

    assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "the member died last");
    arena.reset(|_| {});
}
