//! The collector's reader, driven from a second thread over a graph the
//! owner built: the same rows the owner's own trace would leave, an
//! instance whose storage is mid-move given up and read once the move
//! ends, and a store beside the trace read whole.
//!
//! The thread here is the stand-in for the collector worker, whose own
//! entry waits on the detach protocol (`rfc/model/gc/rc-cycle.md`,
//! "Worker-to-owner handoff"): it takes the owner's token, opens a
//! workspace of its own and runs the two phases through `AtomicCells`.
//! What it may not do is free anything the owner holds — that window is
//! `PLAN.md` S38.3's — so the mutator half of every case here stores and
//! moves, and never frees, while the trace runs.
//!
//! The last case is the pairing ThreadSanitizer and Miri watch
//! (`dev/WORKFLOW.md`, "ThreadSanitizer"): an atomic store on the owner
//! beside the collector's atomic load of the same word, where a plain
//! access on either side is the report. Both instruments find a race by
//! ordering, not by timing, and the case is worth nothing more in a plain
//! build than that every trace completed.

use super::*;
use crate::array::entity::ll_array_new;
use crate::array::testing::push;
use crate::cycle::arena::TraceScratchArena;
use crate::cycle::mark::{MarkResult, mark};
use crate::cycle::scan::{ScanResult, scan};
use crate::cycle::shadow::Color;
use crate::cycle::testing::{dismantle_ring, ring, row_color};
use crate::cycle::token::testing::Handed;
use crate::cycle::token::this_thread_token;
use crate::memory::barrier::ref_store;
use crate::memory::block_pool::test_guard;
use crate::object::{Object, ll_entity_die};
use crate::refcount::{RcHeader, ll_retain};
use crate::test_support::{outside_block, prop_offset, store_prop};
use std::sync::mpsc;

/// An entity pointer handed to the collector thread. The pointee is the
/// owner's, and the owner joins the thread before it touches the entity
/// again.
struct Sent<T>(T);

unsafe impl<T> Send for Sent<T> {}

impl<T> Sent<T> {
    /// The value, through a method so that a closure captures the wrapper
    /// rather than its field.
    fn into_inner(self) -> T {
        self.0
    }
}

/// Trace `root` from a second thread holding this thread's token, through
/// the collector's reader, and answer what `read` found in the rows before
/// they went back with the collector's workspace.
///
/// # Safety
/// `root` is a candidate of this thread's heap, and nothing frees an entity
/// or a storage it reaches until this returns.
unsafe fn traced_from_a_collector_thread<T: Send + 'static>(
    root: *mut RcHeader,
    read: impl FnOnce() -> T + Send + 'static,
) -> std::thread::JoinHandle<T> {
    let token = Handed(this_thread_token());
    let root = Sent(root);
    std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        let token = token.token();
        let root = root.into_inner();
        assert!(unsafe { (*token).try_take() }, "the owner was tracing");
        let mut arena = TraceScratchArena::open().expect("the collector thread drew a workspace");
        assert_eq!(
            unsafe { mark::<AtomicCells>(&mut arena, root) },
            MarkResult::Complete
        );
        assert_eq!(
            unsafe { scan::<AtomicCells>(&mut arena, root) },
            ScanResult::Complete
        );
        let answer = read();
        arena.reset();
        unsafe { (*token).release() };
        crate::cycle::queue::release_queue_segments();
        answer
    })
}

/// The colours the trace left for `entities`, in their order.
///
/// # Safety
/// As `row_color`: every entity's block was touched by the trace whose
/// rows are still standing.
unsafe fn colors(entities: &[*mut RcHeader]) -> Vec<Color> {
    entities
        .iter()
        .map(|&entity| unsafe { row_color(entity) })
        .collect()
}

/// A ring of three whose first member holds a second property, which is
/// where each case hangs its subject.
fn ring_with_a_spare_property(arena: &mut Arena, name: &str) -> [*mut Object; 3] {
    let first = ClassBuilder::new(&format!("{name}First"))
        .prop("next", true)
        .prop("held", true)
        .build();
    let member = ClassBuilder::new(&format!("{name}Member"))
        .prop("next", true)
        .build();
    unsafe { ring(arena, [first, member, member]) }
}

/// An instance of the outside-block class with its block installed and
/// its creation reference still to spend.
fn holder_with_a_block(arena: &mut Arena, class_name: &str) -> *mut Object {
    let cls = outside_block::class(class_name);
    let mut context = LLContext { arena };
    let holder = unsafe { new_constructed(&mut context, cls, MemoryCategory::GcHeap) };
    assert!(!holder.is_null(), "the factory refused");
    unsafe { outside_block::install_block(&mut context, holder) };
    holder
}

/// The reader finds the cell outside the body: a fourth member hangs off
/// the ring through an outside block, and the ring closes through its
/// cell, so a walk that yielded nothing there would leave the second
/// member with an in-edge unsubtracted and the whole ring read as live.
#[test]
fn a_collector_thread_leaves_the_rows_the_owner_would() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let [a, b, c] = ring_with_a_spare_property(&mut arena, "CollectorReads");
    let holder = holder_with_a_block(&mut arena, "CollectorReadsHolder");
    unsafe {
        store_prop(&mut arena, a, prop_offset(1), holder);
        assert!(!ll_release(holder as *mut RcHeader));
        assert!(
            outside_block::store_cell(
                &mut arena,
                holder,
                0,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, b as *mut RcHeader),
            ),
            "the barrier refused the store this case is built on"
        );
    }

    let members = Sent([
        a as *mut RcHeader,
        b as *mut RcHeader,
        c as *mut RcHeader,
        holder as *mut RcHeader,
    ]);
    let found = unsafe {
        traced_from_a_collector_thread(a as *mut RcHeader, move || colors(&members.into_inner()))
    }
    .join()
    .expect("the collector thread returned");
    assert_eq!(
        found,
        vec![Color::PotentiallyUnreachable; 4],
        "every in-edge is internal, the one through the outside cell included"
    );

    unsafe {
        store_prop(&mut arena, a, prop_offset(1), std::ptr::null_mut());
        dismantle_ring(&mut arena, [a, b, c]);
    }
}

/// An outside block whose move is open is given up — its cell's in-edge
/// goes unsubtracted, so the member it names reads as held from outside
/// and the ring as live — and read once the move closes.
#[test]
fn an_outside_block_mid_move_is_given_up_and_read_once_the_move_ends() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let [a, b, c] = ring_with_a_spare_property(&mut arena, "CollectorMidMove");
    let holder = holder_with_a_block(&mut arena, "CollectorMidMoveHolder");
    unsafe {
        store_prop(&mut arena, a, prop_offset(1), holder);
        assert!(!ll_release(holder as *mut RcHeader));
        assert!(outside_block::store_cell(
            &mut arena,
            holder,
            0,
            std::ptr::null_mut(),
            Value::entity(Tag::Object, b as *mut RcHeader),
        ));
    }

    let members = [
        a as *mut RcHeader,
        b as *mut RcHeader,
        c as *mut RcHeader,
        holder as *mut RcHeader,
    ];

    unsafe { outside_block::begin_move(holder) };
    let sent = Sent(members);
    let found = unsafe {
        traced_from_a_collector_thread(a as *mut RcHeader, move || colors(&sent.into_inner()))
    }
    .join()
    .expect("the collector thread returned");
    assert_eq!(
        found,
        vec![Color::Live; 4],
        "the block is mid-move, so its cell is not read and the ring is held from outside"
    );

    unsafe { outside_block::end_move(holder) };
    let sent = Sent(members);
    let found = unsafe {
        traced_from_a_collector_thread(a as *mut RcHeader, move || colors(&sent.into_inner()))
    }
    .join()
    .expect("the collector thread returned");
    assert_eq!(
        found,
        vec![Color::PotentiallyUnreachable; 4],
        "the move ended, and the cell's in-edge is found"
    );

    unsafe {
        store_prop(&mut arena, a, prop_offset(1), std::ptr::null_mut());
        dismantle_ring(&mut arena, [a, b, c]);
    }
}

/// An array whose head is inside a move is given up the same way, and
/// read once the move ends: the ring closes through its element.
#[test]
fn an_array_mid_move_is_given_up_and_read_once_the_move_ends() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let [a, b, c] = ring_with_a_spare_property(&mut arena, "CollectorArrayMove");
    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    unsafe {
        let slot = Object::prop_at(a, prop_offset(1));
        assert!(ref_store(
            &mut arena,
            a as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        assert!(!ll_release(array as *mut RcHeader));
        // `push` stores the word and counts nothing, so the element's
        // reference is retained here.
        ll_retain(b as *mut RcHeader);
        assert!(push(array, Value::entity(Tag::Object, b as *mut RcHeader)));
    }

    let members = [
        a as *mut RcHeader,
        b as *mut RcHeader,
        c as *mut RcHeader,
        array as *mut RcHeader,
    ];
    let head = unsafe { crate::array::entity::storage_head(array) };

    unsafe { (*head).begin_move() };
    let sent = Sent(members);
    let found = unsafe {
        traced_from_a_collector_thread(a as *mut RcHeader, move || colors(&sent.into_inner()))
    }
    .join()
    .expect("the collector thread returned");
    assert_eq!(
        found,
        vec![Color::Live; 4],
        "the head is mid-move, so the element is not read and the ring is held from outside"
    );

    unsafe { (*head).end_move() };
    let sent = Sent(members);
    let found = unsafe {
        traced_from_a_collector_thread(a as *mut RcHeader, move || colors(&sent.into_inner()))
    }
    .join()
    .expect("the collector thread returned");
    assert_eq!(
        found,
        vec![Color::PotentiallyUnreachable; 4],
        "the move ended, and the element's in-edge is found"
    );

    unsafe {
        store_prop(&mut arena, a, prop_offset(1), std::ptr::null_mut());
        dismantle_ring(&mut arena, [a, b, c]);
    }
}

/// How many traces the collector runs over the ring while the owner stores
/// into it. A thousand is long enough that the owner's loop, released by
/// the same signal, runs stores under most of them. Under Miri a handful:
/// its race detector orders accesses by vector clock and finds the pair on
/// the first trace or never, and a thousand interpreted traces cost two
/// minutes of wall for nothing more (the shape
/// `array::table::tests::what_a_walker_reads_while_the_storage_is_released`
/// takes for its rounds).
fn traces_beside_the_stores() -> usize {
    if cfg!(miri) { 20 } else { 1_000 }
}

/// The bound on the owner's loop, for a collector that never signals: a
/// failed assertion on the collector thread would otherwise leave the owner
/// storing forever.
const STORE_TIME_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// The owner stores into a cell of the ring while the collector traces it,
/// and the overlap is by construction: the collector starts its first trace
/// only after the owner's first store, and the owner stores until the
/// collector's last trace is done.
///
/// What the case proves is the pairing — the owner's atomic store beside the
/// collector's atomic load of the same word — under the two instruments that
/// detect a race by ordering rather than by timing: ThreadSanitizer and Miri
/// report a plain access on either side whether or not the two touched the
/// word at the same instant, and the plain-load mutant of 2026-09-15 was
/// reported this way (`dev/WORKFLOW.md`, "ThreadSanitizer"). In a plain build
/// the case asserts only that every trace completed: an aligned 8-byte load
/// does not tear on any target the crate builds for, and the rows the trace
/// leaves depend on which store each trace met.
#[test]
fn a_store_beside_the_trace_is_read_whole() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let [a, b, c] = ring_with_a_spare_property(&mut arena, "CollectorRaces");
    let leaf = ClassBuilder::new("CollectorRacesLeaf").build();
    let (x, y) = {
        let mut context = LLContext { arena: &mut arena };
        unsafe {
            (
                new_constructed(&mut context, leaf, MemoryCategory::GcHeap),
                new_constructed(&mut context, leaf, MemoryCategory::GcHeap),
            )
        }
    };

    let (storing_sender, storing) = mpsc::channel();
    let (done_sender, done) = mpsc::channel();
    let token = Handed(this_thread_token());
    let root = Sent(a as *mut RcHeader);
    let collector = std::thread::spawn(move || {
        assert!(
            crate::memory::heap::ll_thread_init(),
            "the pool served the collector thread"
        );
        let token = token.token();
        let root = root.into_inner();
        assert!(unsafe { (*token).try_take() }, "the owner was tracing");
        storing.recv().expect("the owner stored once");
        let mut traces = 0;
        for _ in 0..traces_beside_the_stores() {
            let mut arena =
                TraceScratchArena::open().expect("the collector thread drew a workspace");
            assert_eq!(
                unsafe { mark::<AtomicCells>(&mut arena, root) },
                MarkResult::Complete
            );
            assert_eq!(
                unsafe { scan::<AtomicCells>(&mut arena, root) },
                ScanResult::Complete
            );
            arena.reset();
            traces += 1;
        }

        unsafe { (*token).release() };
        crate::cycle::queue::release_queue_segments();
        done_sender.send(()).expect("the owner waits for this");
        traces
    });

    let bound = std::time::Instant::now() + STORE_TIME_BOUND;
    let mut stores = 0;
    let mut storing_sender = Some(storing_sender);
    while done.try_recv().is_err() && std::time::Instant::now() < bound {
        let next = if stores % 2 == 0 { x } else { y };
        unsafe { store_prop(&mut arena, a, prop_offset(1), next) };
        stores += 1;
        if let Some(storing) = storing_sender.take() {
            storing.send(()).expect("the collector waits for this");
        }
    }

    let traces = collector.join().expect("the collector thread returned");
    assert_eq!(traces, traces_beside_the_stores(), "every trace completed");
    assert!(stores > 0, "the owner stored before the first trace");

    unsafe {
        store_prop(&mut arena, a, prop_offset(1), std::ptr::null_mut());
        assert!(ll_release(x as *mut RcHeader));
        ll_entity_die(x as *mut RcHeader);
        assert!(ll_release(y as *mut RcHeader));
        ll_entity_die(y as *mut RcHeader);
        dismantle_ring(&mut arena, [a, b, c]);
    }
}
