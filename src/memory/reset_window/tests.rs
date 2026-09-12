//! What the window itself owns: its nesting, the stack of deferred frees,
//! the log the reconciliation reads and the refusal it answers, and the
//! question it answers about a retained block. What a reset makes of all this is observable only
//! through a reset, and those tests live beside it (`promote::tests`).

use super::*;

use crate::memory::block_pool::{
    BLOCK_KIND_ENTITY_LARGE, BLOCK_KIND_FREE, BlockHeader, load_block_kind, test_guard,
};
use crate::refcount::{EntityKind, MemoryCategory};

/// A window opened inside another restores it rather than replacing it:
/// a destructor run by one reset can resolve a second arena and reset it.
#[test]
fn a_nested_window_restores_the_one_it_displaced() {
    let _g = test_guard();
    assert!(!is_open(), "a window was left open by an earlier test");

    let mut outer = ResetWindow::closed();
    let outer_guard = open(&mut outer);
    let outer_address = WINDOW.with(|cell| cell.get());
    {
        let mut inner = ResetWindow::closed();
        let _inner = open(&mut inner);
        assert_ne!(
            WINDOW.with(|cell| cell.get()),
            outer_address,
            "the inner open reused the outer window"
        );
        assert_eq!(depth(), 2);
    }

    assert_eq!(
        WINDOW.with(|cell| cell.get()),
        outer_address,
        "the inner guard's drop did not restore the outer window"
    );

    drop(outer_guard);
    assert!(!is_open(), "the outer guard's drop left a window open");
}

/// A dead large entity in a pooled block of its own, as `ll_free` finds one:
/// a zero count, which every enumerator skips, and no class word, which no
/// reader of a dead slot reads.
unsafe fn dead_pooled_large_entity() -> (*mut u8, *mut BlockHeader) {
    let entity = crate::memory::large_entity::alloc(crate::memory::heap::MAX_SMALL + 16);
    assert!(!entity.is_null());
    let header = entity as *mut RcHeader;
    unsafe {
        header.write(RcHeader::new(
            MemoryCategory::GcHeap,
            EntityKind::Object.to_flags(),
        ));
        crate::refcount::set_header_refcount(header, 0);
    }
    let block = BlockHeader::of_ptr(entity as *const u8);
    assert_eq!(
        unsafe { load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_ENTITY_LARGE
    );
    (entity, block)
}

/// A large body freed inside an inner window is freed by the outermost
/// close and by nothing before it: the stack is the chain's, and the inner
/// close hands nothing over because there is nothing to hand.
///
/// The link is read off byte 8 of the bodies, which is what says the stack
/// stands in the bodies themselves and draws nothing.
#[test]
fn a_deferred_free_is_made_by_the_outermost_close_alone() {
    let _g = test_guard();
    let (first, first_block) = unsafe { dead_pooled_large_entity() };
    let (second, second_block) = unsafe { dead_pooled_large_entity() };

    let mut outer = ResetWindow::closed();
    let outer_guard = open(&mut outer);
    unsafe { crate::memory::stdapi::ll_free(first) };
    {
        let mut inner = ResetWindow::closed();
        let _inner = open(&mut inner);
        unsafe { crate::memory::stdapi::ll_free(second) };
        assert_eq!(
            DEFERRED_FREES.with(|cell| cell.get()),
            second,
            "the body freed last is the head of the stack"
        );
        assert_eq!(
            unsafe { deferred_link(second).read() },
            first,
            "and names the one deferred before it through byte 8"
        );
        assert!(
            unsafe { deferred_link(first).read() }.is_null(),
            "which names null, the end of the stack"
        );
        let _ = take_counters();
    }

    // A pop inside the outer window would take each body again and defer it
    // afresh, which the count of takes is what shows.
    assert_eq!(
        take_counters().0,
        0,
        "the inner close popped the stack and re-deferred every body"
    );
    for block in [first_block, second_block] {
        assert_eq!(
            unsafe { load_block_kind(&raw const (*block).kind) },
            BLOCK_KIND_ENTITY_LARGE,
            "the inner close freed a body the outer reset may still read"
        );
    }

    drop(outer_guard);
    assert!(
        DEFERRED_FREES.with(|cell| cell.get()).is_null(),
        "the outermost close left the stack standing"
    );
    for block in [first_block, second_block] {
        assert_eq!(
            unsafe { load_block_kind(&raw const (*block).kind) },
            BLOCK_KIND_FREE,
            "the outermost close did not make the deferred free"
        );
    }
}

/// A heap header a log record may name as its holder, in one of the three
/// shapes a holder is in when the log is read; the reading is the same for
/// all three.
#[derive(Clone, Copy)]
enum Holder {
    /// Alive, its count above zero.
    Live,
    /// Torn down as `ll_free` leaves one.
    TornDown,
    /// Torn down while a queue entry names it: the candidate bit stands
    /// beside the free's, and the reading is the free's alone.
    TornDownCandidate,
}

fn holder(shape: Holder) -> RcHeader {
    let mut header = RcHeader::new(MemoryCategory::GcHeap, EntityKind::Object.to_flags());
    let entity: *mut RcHeader = &raw mut header;
    let live = matches!(shape, Holder::Live);
    unsafe { crate::refcount::set_header_refcount(entity, if live { 1 } else { 0 }) };
    let bits = match shape {
        Holder::Live => 0,
        Holder::TornDown => crate::refcount::DEAD_IN_PLACE,
        Holder::TornDownCandidate => {
            crate::refcount::DEAD_IN_PLACE | crate::refcount::CANDIDATE_BIT
        }
    };
    unsafe { crate::refcount::update_header_flags(entity, |flags| flags | bits) };
    header
}

/// The log keeps every record across more than one segment, and answers a
/// deferred increment for every record with a holder, whatever became of
/// the holder since — standing, torn down, or torn down with a queue entry
/// still naming it; a decrement is a record with no holder.
///
/// The segments come from the thread's heap and go back at the close,
/// which the block's occupancy is what shows.
#[test]
fn the_log_answers_an_increment_per_edge_whatever_the_holders_fate() {
    let _g = test_guard();
    let mut live = holder(Holder::Live);
    let mut torn_down = holder(Holder::TornDown);
    let mut candidate = holder(Holder::TornDownCandidate);
    let live: *mut RcHeader = &raw mut live;
    let torn_down: *mut RcHeader = &raw mut torn_down;
    let candidate: *mut RcHeader = &raw mut candidate;
    let child_of_live = 0x1000 as *mut RcHeader;
    let child_of_torn_down = 0x2000 as *mut RcHeader;
    let child_of_candidate = 0x2800 as *mut RcHeader;
    let decremented = 0x3000 as *mut RcHeader;

    // More records than one segment holds, so the walk crosses a segment
    // boundary and the close gives more than one back.
    let per_kind = RECORDS_PER_SEGMENT / 3 + 1;

    let mut window = ResetWindow::closed();
    let guard = open(&mut window);
    let _ = take_refused_records();
    for _ in 0..per_kind {
        record_promotion_edge(live, child_of_live);
        record_promotion_edge(torn_down, child_of_torn_down);
        record_promotion_edge(candidate, child_of_candidate);
        record_deferred_decrement(decremented);
    }
    assert_eq!(take_refused_records(), 0, "the manager refused a segment");

    let (mut live_increments, mut increments, mut candidate_increments, mut decrements, mut others) =
        (0, 0, 0, 0, 0);
    for_each_correction(|child, correction| match (child, correction) {
        (child, Correction::DeferredIncrement) if child == child_of_live => live_increments += 1,
        (child, Correction::DeferredIncrement) if child == child_of_torn_down => increments += 1,
        (child, Correction::DeferredIncrement) if child == child_of_candidate => {
            candidate_increments += 1
        }
        (child, Correction::DeferredDecrement) if child == decremented => decrements += 1,
        _ => others += 1,
    });
    assert_eq!(
        (
            live_increments,
            increments,
            candidate_increments,
            decrements,
            others
        ),
        (per_kind, per_kind, per_kind, per_kind, 0),
        "every edge is owed once, whatever became of its holder; a \
         decrement is a record with no holder"
    );

    let newest = unsafe { (*WINDOW.with(|cell| cell.get())).log };
    let older = unsafe { (*newest).header.next };
    assert!(
        !older.is_null(),
        "four times {per_kind} records fit in one segment, so the boundary was never crossed"
    );
    let block = BlockHeader::of_ptr(newest as *const u8) as *mut u8;
    let occupancy_before = unsafe { crate::memory::heap::block_occupancy(block) };
    let segments_in_block = {
        let mut count = 0;
        let mut segment = newest;
        while !segment.is_null() {
            if BlockHeader::of_ptr(segment as *const u8) as *mut u8 == block {
                count += 1;
            }

            segment = unsafe { (*segment).header.next };
        }

        count as u32
    };

    drop(guard);
    assert_eq!(
        unsafe { crate::memory::heap::block_occupancy(block) },
        occupancy_before - segments_in_block,
        "the close did not give the segments back"
    );
}

/// A segment the manager refuses is answered as a refusal: the record is
/// not kept, the reset reads the refusal once for the round, and the log
/// takes records again once the manager answers. Nothing aborts.
#[test]
fn a_refused_segment_is_answered_to_the_recorder() {
    let _g = test_guard();
    let mut live = holder(Holder::Live);
    let live: *mut RcHeader = &raw mut live;
    let child = 0x1000 as *mut RcHeader;

    let mut window = ResetWindow::closed();
    let guard = open(&mut window);
    let _ = take_refused_records();
    assert!(
        !take_refused_promotion_edge(),
        "a round with no refusal read one"
    );

    let refused = RefusedSegments::arm();
    record_promotion_edge(live, child);
    record_deferred_decrement(child);
    drop(refused);

    assert_eq!(take_refused_records(), 2, "the refusals were not counted");
    assert!(
        take_refused_promotion_edge(),
        "the reset was not told of the refusal"
    );
    assert!(
        !take_refused_promotion_edge(),
        "the reading did not clear the flag"
    );

    let mut kept = 0;
    for_each_correction(|_, _| kept += 1);
    assert_eq!(kept, 0, "a refused record was kept");

    // The log grows again once the manager answers.
    record_promotion_edge(live, child);
    let mut kept = 0;
    for_each_correction(|_, _| kept += 1);
    assert_eq!(kept, 1, "the record after the refusal was not kept");
    assert_eq!(take_refused_records(), 0);
    drop(guard);
}

/// The absorb question has three answers, and only one of them is true:
/// a reset takes back its own torn-down entity's free, leaves an earlier
/// reset's occupant alone, and answers nothing at all outside a reset. The
/// question is asked of the block's count word and not of its list, so
/// the block here counts one occupant and lists nothing, which is the
/// state a reset leaves a block in when it could place no list.
#[test]
fn only_an_uncounted_block_inside_a_reset_is_absorbed() {
    let _g = test_guard();
    let block = crate::memory::retained::bare_retained_block();
    let cell = BlockHeader::payload_start(block as *mut BlockHeader) as *mut u64;
    unsafe { cell.write(1) };

    assert!(
        !unsafe { absorbs_retained_free(block) },
        "a free outside a reset was absorbed"
    );

    let mut window = ResetWindow::closed();
    let guard = open(&mut window);
    assert!(
        unsafe { absorbs_retained_free(block) },
        "the reset did not absorb the free of a block whose count it has not established"
    );

    assert!(
        !unsafe {
            crate::memory::retained::register(block, &[cell as usize], std::ptr::null_mut())
        },
        "a block with a live occupant is not empty on arrival"
    );
    assert!(
        !unsafe { absorbs_retained_free(block) },
        "an occupant an earlier reset counted was absorbed by this one"
    );

    assert!(
        unsafe { crate::memory::retained::occupant_freed(block) },
        "the count outlived the test that established it"
    );
    unsafe { cell.write(0) };
    unsafe { crate::memory::retained::release_emptied(block) };
    drop(guard);
}

/// The module holds no Rust container, read off its own source with the
/// comments cut: a `Vec`, `Box`, map or set here is an allocation the
/// manager never sees and a refusal that ends the process
/// (`dev/DECISIONS.md`, "the reset window's memory comes from the manager,
/// and an allocation it cannot get is a refusal").
///
/// A spelling net rather than a type check: it catches the containers by
/// the names they are written with, and a global allocation spelled another
/// way passes it. The deny run over a reset is the reading that catches
/// those, and that reading waits on `promote`'s own containers.
#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; the file read is unavailable under Miri's isolation, \
              and the abort takes the whole slice with it"
)]
fn the_window_holds_no_container() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/memory/reset_window.rs");
    let source = std::fs::read_to_string(path).expect("the module's source is readable");
    let code: String = source
        .lines()
        .map(|line| line.split("//").next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    for container in [
        "Vec<",
        "Vec::",
        "VecDeque",
        "vec!",
        "to_vec",
        "to_owned",
        "Box<",
        "Box::",
        "boxed",
        "HashMap",
        "HashSet",
        "BTreeMap",
        "BTreeSet",
        "Arc<",
        "Rc<",
        "Rc::",
        "String",
        "format!",
        "alloc::alloc",
        "Layout",
    ] {
        assert!(
            !code.contains(container),
            "`{container}` stands in the window's code"
        );
    }
}
