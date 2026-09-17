//! A death on a thread whose token another thread holds is withheld until
//! that holder lets go, and the returns are then made by the mutator — at its
//! next free, at the safepoint poll, and before its exit.
//!
//! The holder here traces nothing: what the gate reads is the token alone,
//! since a trace on another thread may hold an address the mutator's free
//! would pull out from under it between reading a cell and meeting the row
//! (`rfc/model/gc/rc-cycle.md`, "The deferral's contract"). The trace that
//! does read while the mutator frees is `cells::tests::what_a_collector_thread_reads`'
//! subject once this window stands; here the question is the window itself.

use super::*;
use crate::cycle::token::testing::HeldByACollector;
use crate::cycle::token::this_thread_token;
use crate::test_support::prop_offset;

/// Red without the foreign window: the free list is LIFO, so the allocation
/// under the holder would receive the slot just freed.
#[test]
fn a_death_under_a_foreign_holder_waits_for_the_release() {
    let _guard = test_guard();
    let dead = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!dead.is_null());
    let dead = unsafe { dead_entity(dead) };

    let mut holder = HeldByACollector::take(this_thread_token(), false);
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(
        foreign_withheld_count(),
        1,
        "the death is withheld while another thread holds this thread's token"
    );
    assert_eq!(
        deferred_slot_count(),
        0,
        "no window of this thread's own is open"
    );

    let fresh = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!fresh.is_null());
    assert_ne!(
        fresh, dead as *mut u8,
        "the withheld slot was handed out under the holder"
    );
    let fresh = unsafe { live_entity(fresh, 1) };

    holder.release();
    assert_eq!(
        foreign_withheld_count(),
        1,
        "the release makes no return by itself: the mutator makes them"
    );

    // The mutator's next free is the first place the returns are made, ahead
    // of that free's own.
    unsafe { crate::refcount::set_header_refcount(fresh, 0) };
    unsafe { crate::memory::stdapi::ll_free(fresh as *mut u8) };
    assert_eq!(foreign_withheld_count(), 0);

    let mut handed_out = [
        unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) },
        unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) },
    ];
    handed_out.sort();
    let mut expected = [dead as *mut u8, fresh as *mut u8];
    expected.sort();
    assert_eq!(
        handed_out, expected,
        "both slots came back to the allocator once the holder let go"
    );
    for slot in handed_out {
        let header = unsafe { dead_entity(slot) };
        unsafe { crate::memory::stdapi::ll_free(header as *mut u8) };
    }
}

/// The safepoint poll makes the returns too, so a thread that frees nothing
/// after the release still gives the slots back at its next safepoint.
#[test]
fn the_poll_makes_the_returns_a_holder_left_behind() {
    let _guard = test_guard();
    let dead = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!dead.is_null());
    let dead = unsafe { dead_entity(dead) };

    let mut holder = HeldByACollector::take(this_thread_token(), false);
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    assert_eq!(foreign_withheld_count(), 1);
    holder.release();

    unsafe { crate::gc::ll_gc_maybe_collect() };
    assert_eq!(foreign_withheld_count(), 0, "the poll made the return");
    let again = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert_eq!(
        again, dead as *mut u8,
        "the slot is the allocator's again, and the free list is LIFO"
    );
    let again = unsafe { dead_entity(again) };
    unsafe { crate::memory::stdapi::ll_free(again as *mut u8) };
}

/// The occupancy the heap reads for the block `addr` stands in, and the
/// block's kind beside it, off `heap::describe_slot`'s own line.
fn kind_and_used(addr: usize) -> (u32, u32) {
    let line = crate::memory::heap::describe_slot(addr);
    let field = |name: &str| -> u32 {
        let start = line.find(&format!(" {name} ")).expect(name) + name.len() + 2;
        line[start..]
            .split(' ')
            .next()
            .and_then(|word| word.parse().ok())
            .expect(name)
    };
    (field("kind"), field("used"))
}

/// A holder that takes the token again before the mutator has made its
/// returns leaves them standing: the pop stops at a held token rather than
/// handing a slot back under the new trace.
#[test]
fn a_second_holder_keeps_the_returns_withheld() {
    let _guard = test_guard();
    let dead = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!dead.is_null());
    let dead = unsafe { dead_entity(dead) };

    let mut first = HeldByACollector::take(this_thread_token(), false);
    unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
    first.release();
    let mut second = HeldByACollector::take(this_thread_token(), false);

    unsafe { crate::gc::ll_gc_maybe_collect() };
    assert_eq!(
        foreign_withheld_count(),
        1,
        "the second holder stands, and the slot stays withheld under it"
    );
    second.release();
    unsafe { crate::gc::ll_gc_maybe_collect() };
    assert_eq!(foreign_withheld_count(), 0);
}

/// A thread's exit makes the returns after its wait for the holder, so an
/// abandoned heap carries no slot that is neither live nor on a free list.
#[test]
fn the_exit_makes_the_returns_after_its_wait() {
    let _guard = test_guard();
    let (dead, used_while_withheld) = std::thread::spawn(|| {
        assert!(crate::memory::heap::ll_thread_init());
        let dead = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!dead.is_null());
        let dead = unsafe { dead_entity(dead) };

        // Released once the exit's wait is entered, which is the holder's
        // `until_waited` arm.
        let _holder = HeldByACollector::take(this_thread_token(), true);
        unsafe { crate::memory::stdapi::ll_free(dead as *mut u8) };
        assert_eq!(foreign_withheld_count(), 1);
        // A withheld slot still counts as an occupant: `used` falls at the
        // physical return and never at the free.
        let (kind, used) = kind_and_used(dead as usize);
        assert_eq!(kind, crate::memory::block_pool::BLOCK_KIND_ENTITY);
        crate::memory::heap::ll_thread_exit();
        (dead as usize, used)
    })
    .join()
    .expect("the thread exited with its returns made");

    let (kind, used) = kind_and_used(dead);
    assert!(
        kind != crate::memory::block_pool::BLOCK_KIND_ENTITY || used < used_while_withheld,
        "the exit made the return: the block's occupancy fell, or the emptied block went back \
         (kind {kind}, used {used} against {used_while_withheld})"
    );
}

/// A pointer handed to the thread that frees it from outside.
struct Sent(*mut u8);

unsafe impl Send for Sent {}

impl Sent {
    /// The pointer, through a method so that a closure captures the wrapper
    /// rather than its field.
    fn pointer(&self) -> *mut u8 {
        self.0
    }
}

/// A slot another thread freed is reclaimed by the mutator onto its free list,
/// and that reclaim is a return of the mutator's memory: under a holder it
/// waits on the block's remote stack, where nothing hands it out, and once
/// the holder lets go the next reclaim takes it.
#[test]
fn a_cross_thread_free_is_not_reclaimed_under_a_holder() {
    let _guard = test_guard();
    let dead = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
    assert!(!dead.is_null());
    unsafe { dead_entity(dead) };
    let block = block_of(dead);

    // Fill the block, so that the allocation under the holder reaches the
    // reclaim of its remote frees rather than its bump.
    let mut filler = Vec::new();
    loop {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        unsafe { live_entity(slot, 1) };
        filler.push(slot);
        if block_of(slot) != block {
            break;
        }
    }

    let sent = Sent(dead);
    std::thread::spawn(move || unsafe { crate::memory::heap::free_foreign(sent.pointer()) })
        .join()
        .expect("the foreign free returned");

    let mut holder = HeldByACollector::take(this_thread_token(), false);
    let mut under_the_holder = Vec::new();
    for _ in 0..filler.len() {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        assert_ne!(
            slot, dead,
            "the cross-thread free was reclaimed under the holder"
        );
        unsafe { live_entity(slot, 1) };
        under_the_holder.push(slot);
    }

    holder.release();
    let mut reclaimed = false;
    let mut after = Vec::new();
    for _ in 0..filler.len() * 2 {
        let slot = unsafe { crate::memory::heap::entity_alloc(ENTITY_SIZE) };
        assert!(!slot.is_null());
        if slot == dead {
            reclaimed = true;
            break;
        }

        unsafe { live_entity(slot, 1) };
        after.push(slot);
    }

    assert!(reclaimed, "the release let the next reclaim take the slot");

    for slot in filler.into_iter().chain(under_the_holder).chain(after) {
        unsafe { crate::refcount::set_header_refcount(slot as *mut RcHeader, 0) };
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    unsafe { dead_entity(dead) };
    unsafe { crate::memory::stdapi::ll_free(dead) };
}

/// The trace that reads while the mutator frees: a collector thread traces the
/// ring a thousand times while the mutator kills the leaf its first member
/// holds and hangs a fresh one there, so every death goes through `ll_free`
/// under the holder. Each slot freed under the trace is written down, and no
/// allocation made under the trace receives one of them — the reuse the
/// window prevents — while the poll after the release gives every one back.
#[test]
fn a_death_under_a_running_trace_is_not_reused_before_the_release() {
    let _guard = test_guard();
    let mut arena = Arena::new();
    let [a, b, c] = unsafe {
        crate::cycle::testing::ring_with_a_spare_property(&mut arena, "ForeignTraceChurn")
    };
    let leaf = crate::class::ClassBuilder::new("ForeignTraceChurnLeaf").build();
    let new_leaf = |arena: &mut Arena| {
        let mut context = LLContext { arena };
        let leaf = unsafe { new_constructed(&mut context, leaf, MemoryCategory::GcHeap) };
        assert!(!leaf.is_null(), "the factory refused");
        leaf
    };

    let (storing_sender, storing) = std::sync::mpsc::channel();
    let (done_sender, done) = std::sync::mpsc::channel();
    let collector = unsafe {
        crate::cycle::testing::traced_from_a_collector_thread(
            a as *mut RcHeader,
            // Under Miri a handful, for the reason
            // `cells::tests::what_a_collector_thread_reads` gives.
            if cfg!(miri) { 20 } else { 1_000 },
            Some(storing),
            move || done_sender.send(()).expect("the mutator waits for this"),
        )
    };

    // The leaf's creation reference is the property's: written into the
    // slot without the barrier's retain, so that its one decrement is final
    // and its death reaches `ll_free` with no candidate registration — the
    // window under test, and not the queue's.
    let slot = unsafe { Object::prop_at(a, prop_offset(1)) };
    let mut held = new_leaf(&mut arena);
    unsafe {
        crate::memory::barrier::write_value_slot(
            slot,
            crate::value::Value::entity(crate::value::Tag::Object, held as *mut RcHeader),
        )
    };
    storing_sender
        .send(())
        .expect("the collector waits for this");

    // The loop ends at the collector's signal or at the first free that
    // found the token released and made the returns: from there a slot
    // freed under the trace is the allocator's again, so the assertion
    // inside holds exactly while every death so far is still withheld.
    let bound = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut freed_under_the_trace = Vec::new();
    let mut deaths = 0;
    while done.try_recv().is_err()
        && foreign_withheld_count() == deaths
        && std::time::Instant::now() < bound
    {
        let next = new_leaf(&mut arena);
        assert!(
            !freed_under_the_trace.contains(&(next as usize)),
            "a slot freed under the trace was handed out again before the release"
        );
        // The slot takes the next leaf and `held` dies: the property's
        // reference was its only one.
        unsafe {
            crate::memory::barrier::write_value_slot(
                slot,
                crate::value::Value::entity(crate::value::Tag::Object, next as *mut RcHeader),
            );
            assert!(crate::refcount::ll_release(held as *mut RcHeader));
            ll_object_die(held);
        }

        freed_under_the_trace.push(held as usize);
        held = next;
        deaths += 1;
    }

    collector.join().expect("the collector thread returned");
    assert!(deaths > 1, "the mutator killed leaves under the trace");
    unsafe { crate::gc::ll_gc_maybe_collect() };
    assert_eq!(foreign_withheld_count(), 0, "the poll made every return");

    unsafe {
        crate::memory::barrier::write_value_slot(slot, crate::value::Value::null());
        assert!(crate::refcount::ll_release(held as *mut RcHeader));
        ll_object_die(held);
        crate::cycle::testing::dismantle_ring(&mut arena, [a, b, c]);
    }
}
