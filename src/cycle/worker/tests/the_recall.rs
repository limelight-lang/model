//! The recall of the token: a mutator that asks for its token while a
//! collector traces for it waits through at most [`RECALL_STRIDE`] positions
//! of storage, the batch's posts and one reset of the arena, whatever the
//! positions hold (`dev/S65-PLAN-CRITIC.md`, F1). The five containers are
//! strides in which a position yields no counted reference — a vector of
//! scalars, a hash whose entries hold none, an object of null Box fields, one
//! of null typed fields and a class's outside storage of empty cells — so
//! that a check made per edge would read each of them whole between two
//! readings of the recall.
//!
//! The mutator asks between the mark and the scan, so what the scan reads
//! after the ask is the recall's bound, and the interval from the instant
//! the mutator stands in the token's wait to the release is what it waited.

use super::*;
use crate::array::entity::{LLArray, ll_array_new};
use crate::array::table::Key;
use crate::cells::{Cell, OutsideCarry, OutsideCells};
use crate::class::{Class, ClassBuilder};
use crate::cycle::arena::RECALL_STRIDE;
use crate::cycle::queue::candidate_count;
use crate::cycle::queue::verdicts::{Verdict, standing_verdicts};
use crate::memory::arena::Arena;
use crate::memory::barrier::write_value_slot;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::test_support::prop_offset;
use crate::value::{Tag, Value};
use std::ops::ControlFlow;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

/// Positions of the three smaller containers: sixty-four strides, so that a
/// check per edge, which none of them yields, reads every one of them.
const POSITIONS: usize = if cfg!(miri) {
    4 * RECALL_STRIDE
} else {
    64 * RECALL_STRIDE
};

/// Cells of the scalar vector, the plan's own size.
const VECTOR_CELLS: usize = if cfg!(miri) {
    4 * RECALL_STRIDE
} else {
    1_000_000
};

/// Fields of each object of null fields: its body is a large entity, and a
/// class of more properties costs the case its build time and adds nothing.
const NULL_FIELDS: usize = if cfg!(miri) {
    4 * RECALL_STRIDE
} else {
    16 * RECALL_STRIDE
};

#[derive(Clone, Copy, Debug)]
enum Container {
    ScalarVector,
    ScalarHash,
    NullFields,
    NullPointerFields,
    EmptyOutsideStorage,
}

/// Build `container` at count one, its creation reference the caller's, and
/// the tag a `Value` naming it carries.
///
/// # Safety
/// A quiescent heap under [`test_guard`], `context` this thread's.
unsafe fn build(context: &mut LLContext, container: Container) -> (*mut RcHeader, Tag) {
    match container {
        Container::ScalarVector => {
            let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
            assert!(!array.is_null(), "the array was allocated");
            for cell in 0..VECTOR_CELLS {
                assert!(
                    unsafe { crate::array::testing::push(array, Value::int(cell as i64)) },
                    "the vector grew"
                );
            }

            (array as *mut RcHeader, Tag::Array)
        }
        Container::ScalarHash => {
            let array: *mut LLArray =
                unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
            assert!(!array.is_null(), "the array was allocated");
            for key in 0..POSITIONS {
                assert!(
                    unsafe {
                        crate::array::testing::insert(
                            array,
                            Key::Int(key as i64),
                            Value::int(key as i64),
                        )
                    }
                    .is_some(),
                    "the hash grew"
                );
            }

            (array as *mut RcHeader, Tag::Array)
        }
        Container::NullFields => {
            let object =
                unsafe { new_constructed(context, null_fields_class(), MemoryCategory::GcHeap) };
            (object as *mut RcHeader, Tag::Object)
        }
        Container::NullPointerFields => {
            let object = unsafe {
                new_constructed(context, null_pointer_fields_class(), MemoryCategory::GcHeap)
            };
            (object as *mut RcHeader, Tag::Object)
        }
        Container::EmptyOutsideStorage => {
            let class = ClassBuilder::new("RecallEmptyOutsideStorage")
                .outside_cells(&EMPTY_STORAGE_GROUP)
                .build();
            let object = unsafe { new_constructed(context, class, MemoryCategory::GcHeap) };
            (object as *mut RcHeader, Tag::Object)
        }
    }
}

/// The class of [`NULL_FIELDS`] Box properties, built once: a descriptor is
/// immortal, and one of this width takes its build time.
fn null_fields_class() -> *const Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CLASS.get_or_init(|| {
        let mut builder = ClassBuilder::new("RecallNullFields");
        for field in 0..NULL_FIELDS {
            builder = builder.prop(&format!("f{field}"), true);
        }

        builder.build() as usize
    }) as *const Class
}

/// The class of [`NULL_FIELDS`] typed properties, built once: a declared
/// class type is a bare pointer, traced as a pointer run rather than a Box
/// run, and null until assigned.
fn null_pointer_fields_class() -> *const Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CLASS.get_or_init(|| {
        let mut builder = ClassBuilder::new("RecallNullPointerFields");
        for field in 0..NULL_FIELDS {
            builder = builder.prop_pointer(&format!("f{field}"));
        }

        builder.build() as usize
    }) as *const Class
}

/// The outside storage every instance of the empty-storage class walks:
/// [`POSITIONS`] cells, each of them null, read by the concurrent walk
/// through the collector's reader as a class's own storage would be.
static EMPTY_STORAGE: [AtomicU64; 2 * POSITIONS] = [const { AtomicU64::new(0) }; 2 * POSITIONS];

static EMPTY_STORAGE_GROUP: OutsideCells = OutsideCells {
    walk_plain: walk_nothing,
    walk_concurrent: walk_the_empty_storage,
    sever: sever_nothing,
    sever_one: sever_one_of_nothing,
    free: free_nothing,
    carry: carry_nothing,
};

/// The plain walk yields no cell, the storage holding none.
unsafe fn walk_nothing(_: *mut u8, _: *const Class, _: &mut dyn FnMut(Cell)) {}

/// Every position of the storage, read as the collector's reader reads one.
unsafe fn walk_the_empty_storage(
    _: *mut u8,
    _: *const Class,
    visit: &mut dyn FnMut(Option<Cell>) -> ControlFlow<()>,
) -> ControlFlow<()> {
    for index in 0..POSITIONS {
        let at = (&raw const EMPTY_STORAGE[2 * index]) as *const u8;
        visit(unsafe { crate::cells::counted_box_cell::<crate::cells::AtomicCells>(at) })?;
    }

    ControlFlow::Continue(())
}

unsafe fn sever_nothing(_: *mut RcHeader, _: &mut dyn FnMut(*mut RcHeader)) {}

unsafe fn sever_one_of_nothing(_: *mut RcHeader, _: Cell, _: &mut dyn FnMut(*mut RcHeader)) {
    unreachable!("the walk yields no cell to sever");
}

unsafe fn free_nothing(_: *mut RcHeader) {}

unsafe fn carry_nothing(_: *mut Arena, _: *mut RcHeader) -> OutsideCarry {
    OutsideCarry::Nothing
}

/// A root holding `container` by its one property, registered by a retain
/// and a non-final release; its creation reference stays the case's, so the
/// trace reads it live and expands it in both phases.
///
/// # Safety
/// As [`build`].
unsafe fn a_root_over(context: &mut LLContext, container: *mut RcHeader, tag: Tag) -> *mut Object {
    let root =
        unsafe { new_constructed(context, node_class("RecallRoot"), MemoryCategory::GcHeap) };
    unsafe {
        // The container's creation reference is spent into the slot.
        write_value_slot(
            Object::prop_at(root, prop_offset(0)),
            Value::entity(tag, container),
        );
        ll_retain(root as *mut RcHeader);
        assert!(
            !ll_release(root as *mut RcHeader),
            "the case's reference holds the root"
        );
    }
    root
}

/// Give back `root` and, through its death, the container it holds.
///
/// # Safety
/// `root` came from [`a_root_over`] and no collection is running.
unsafe fn let_go(root: *mut Object) {
    unsafe {
        assert!(
            ll_release(root as *mut RcHeader),
            "the case's reference was the last"
        );
        ll_object_die(root);
    }
}

/// A recall a case raised by hand, cleared on the unwind too: the record goes
/// to the next thread the registry hands it to, and a recall left standing
/// would stop that thread's collectors at their first stride.
struct ClearTheRecall<'a>(&'a crate::cycle::token::TraceToken);

impl Drop for ClearTheRecall<'_> {
    fn drop(&mut self) {
        self.0.recall_for_test(false);
    }
}

/// The mutator's side of the ask: a receiver the hook between the next
/// batch's phases sends to, and the instant the collector saw the mutator
/// standing in its token's wait, stamped before the scan goes on.
fn asked_between_the_phases() -> (
    std::sync::mpsc::Receiver<()>,
    std::sync::Arc<std::sync::Mutex<Option<Instant>>>,
) {
    let (ask, asked) = std::sync::mpsc::channel();
    let waiting_from = std::sync::Arc::new(std::sync::Mutex::new(None));
    let stamp = std::sync::Arc::clone(&waiting_from);
    let token = unsafe { &raw const (*record()).token } as usize;
    testing::between_the_next_phases(Box::new(move || {
        let token = unsafe { &*(token as *const crate::cycle::token::TraceToken) };
        let before = token.waits();
        let _ = ask.send(());
        let deadline = Instant::now() + A_BIRTH;
        while token.waits() == before {
            assert!(
                Instant::now() < deadline,
                "the mutator stood in its token's wait"
            );
            std::hint::spin_loop();
        }

        *stamp
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
    }));
    (asked, waiting_from)
}

/// Birth the elder over this thread's record at a round threshold of one
/// entry and run `on_the_ask` as the mutator once the hook between the
/// phases asks; answer the one batch it traced and what the mutator waited,
/// from its standing in the token's wait to the release.
fn a_batch_asked_between_its_phases(on_the_ask: impl FnOnce()) -> (testing::TracedBatch, Duration) {
    let (asked, waiting_from) = asked_between_the_phases();
    testing::serve_rounds_at(1);
    testing::read_traced_batches(true);
    let _ = testing::take_released_at();
    testing::confine_rounds_to(record());
    testing::permit_births(true);
    ensure_thread();
    let mut on_the_ask = Some(on_the_ask);
    assert!(
        wait_until(
            || {
                if asked.try_recv().is_ok() {
                    (on_the_ask.take().expect("the hook asks once"))();
                }

                on_the_ask.is_none()
            },
            A_BIRTH
        ),
        "the batch's mark ended and the hook asked"
    );

    assert!(
        !unsafe { &(*record()).token }.is_recalled(),
        "the take that recalled the token cleared the recall when it returned"
    );
    let released_at = testing::take_released_at().expect("the grant was released");
    let batches = testing::take_traced_batches();
    testing::read_traced_batches(false);
    testing::retire();
    // The first batch is the hooked one; a round after it may take the ring
    // again before the case retires the thread.
    let batch = *batches.first().expect("the batch was traced");
    let from = waiting_from
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .expect("the mutator stood in the token's wait");
    (batch, released_at - from)
}

/// The recall's bound over one container: a pressure collection asked for
/// between the phases, and the scan that reads at most one stride after the
/// ask and stops, the batch posting its snapshot.
fn a_pressure_collection_recalls_the_grant_over(container: Container) {
    let _g = test_guard();
    let _end = RetireOnDrop;
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let (built, tag) = unsafe { build(&mut context, container) };
    let root = unsafe { a_root_over(&mut context, built, tag) };
    assert_eq!(candidate_count(), 1, "R holds the root alone");

    let (batch, waited) = a_batch_asked_between_its_phases(|| {
        unsafe { crate::cycle::collect::collect_under_pressure() };
    });
    eprintln!("{container:?}: {batch:?}, the mutator waited {waited:?}");
    let positions = batch
        .positions_after_the_hook
        .expect("the hook ran between the phases");
    assert!(
        positions <= RECALL_STRIDE,
        "{container:?}: the scan read {positions} positions after the ask, past one stride of \
         {RECALL_STRIDE}"
    );
    assert!(
        !batch.complete,
        "{container:?}: a recalled trace stops where it stands"
    );

    unsafe { let_go(root) };
    reset_lanes();
}

#[test]
fn over_a_vector_of_scalars() {
    a_pressure_collection_recalls_the_grant_over(Container::ScalarVector);
}

#[test]
fn over_a_hash_whose_entries_hold_no_reference() {
    a_pressure_collection_recalls_the_grant_over(Container::ScalarHash);
}

#[test]
fn over_an_object_of_null_fields() {
    a_pressure_collection_recalls_the_grant_over(Container::NullFields);
}

#[test]
fn over_an_object_of_null_pointer_fields() {
    a_pressure_collection_recalls_the_grant_over(Container::NullPointerFields);
}

#[test]
fn over_an_outside_storage_of_empty_cells() {
    a_pressure_collection_recalls_the_grant_over(Container::EmptyOutsideStorage);
}

/// A recall of a batch of several roots posts each of them once and advances
/// R past all of them: raised between the phases, after the mark's first
/// regions ended, it finds each root's row above zero — the case holds every
/// root — and posts it read live, the snapshot's rule for a root whose own
/// region was expanded (`dev/plans/S67.md`, S67.9, revision 3, G3).
#[test]
#[cfg_attr(
    all(feature = "collector-chain", not(feature = "hold-by-generation")),
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_recalled_batch_posts_every_root_once_read_live_and_advances_r() {
    const ROOTS: usize = 5;
    let _g = test_guard();
    let _end = RetireOnDrop;
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let roots: Vec<*mut Object> = (0..ROOTS)
        .map(|_| unsafe {
            let (built, tag) = build(&mut context, Container::NullFields);
            a_root_over(&mut context, built, tag)
        })
        .collect();
    assert_eq!(candidate_count(), ROOTS, "R holds the roots alone");

    let (batch, _) = a_batch_asked_between_its_phases(|| {
        // At `POSTED` the hold leaves the byte as it stands, so the posts
        // are read before any collection disposes of them.
        drop(crate::cycle::token::HeldToken::take_or_hold_posted());
    });
    assert!(!batch.complete, "the recalled trace was abandoned");
    let posted = standing_verdicts();
    assert_eq!(
        posted
            .iter()
            .map(|&(root, verdict)| (root as *mut Object, verdict))
            .collect::<Vec<_>>(),
        roots
            .iter()
            .map(|&root| (root, Verdict::ReadLive))
            .collect::<Vec<_>>(),
        "every root posted once, read live, in R's order"
    );
    assert_eq!(candidate_count(), 0, "R advanced past the batch");

    assert_eq!(
        unsafe { crate::gc::ll_gc_maybe_collect() },
        0,
        "the roots are live"
    );
    for root in roots {
        unsafe { let_go(root) };
    }

    reset_lanes();
}

/// Under the collector's chain the recalled batch's roots, read live by the
/// snapshot once the first regions had ended, go to the chain's waiting part,
/// each once, as a completed trace's roots read live do; nothing goes into P,
/// and R advances past them as without the chain. The roots are of the second
/// generation: under `hold-by-generation` a younger one goes into P
/// (`the_generations_in_the_chain`).
#[test]
#[cfg(feature = "collector-chain")]
fn under_the_chain_a_recalled_batch_puts_every_root_once_in_the_waiting_part() {
    const ROOTS: usize = 5;
    let _g = test_guard();
    let _end = RetireOnDrop;
    crate::cycle::chain::testing::dismantle_this_threads();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let roots: Vec<*mut Object> = (0..ROOTS)
        .map(|_| unsafe {
            let (built, tag) = build(&mut context, Container::NullFields);
            a_root_over(&mut context, built, tag)
        })
        .collect();
    for &root in &roots {
        unsafe { crate::cycle::testing::as_of_the_second_generation(root as *mut RcHeader) };
    }

    let (batch, _) = a_batch_asked_between_its_phases(|| {
        drop(crate::cycle::token::HeldToken::take_or_hold_posted());
    });
    assert!(!batch.complete, "the recalled trace was abandoned");
    assert_eq!(standing_verdicts(), Vec::new(), "nothing in P");
    let (ready, waiting) = crate::cycle::chain::testing::roots_of_this_threads();
    assert_eq!(
        (
            ready.len(),
            waiting
                .iter()
                .map(|&root| root as *mut Object)
                .collect::<Vec<_>>(),
        ),
        (0, roots.clone()),
        "every root once, in R's order, in the waiting part"
    );
    assert_eq!(candidate_count(), 0, "R advanced past the batch");

    for root in roots {
        unsafe { let_go(root) };
    }
    crate::cycle::chain::testing::dismantle_this_threads();
    reset_lanes();
}

/// Both phases through the collector's reader on an arena opened for a
/// mutator whose recall stands answer `Recalled` at the first reading of it,
/// one stride of positions in, and a mark through the owner's reader over the
/// same entity counts nothing and reads it whole.
#[test]
fn a_trace_under_a_standing_recall_stops_within_a_stride_and_a_plain_one_does_not() {
    use crate::cells::{AtomicCells, PlainCells};
    use crate::cycle::arena::TraceScratchArena;
    use crate::cycle::mark::{MarkResult, mark};
    use crate::cycle::scan::{ScanResult, scan};

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let (built, tag) = unsafe { build(&mut context, Container::ScalarVector) };
    let root = unsafe { a_root_over(&mut context, built, tag) };
    let token = unsafe { &(*record()).token };
    let _clear = ClearTheRecall(token);

    token.recall_for_test(true);
    let mut recalled =
        unsafe { TraceScratchArena::open_for_owner(record()) }.expect("the workspace was drawn");
    let answer = unsafe { mark::<AtomicCells>(&mut recalled, root as *mut RcHeader) };
    let read = recalled.positions_inspected();
    recalled.reset();
    drop(recalled);
    token.recall_for_test(false);

    // The scan stands on a completed mark, so the recall is raised between
    // the two phases, where the collector's case raises it.
    let mut scanned =
        unsafe { TraceScratchArena::open_for_owner(record()) }.expect("the workspace was drawn");
    let marked = unsafe { mark::<AtomicCells>(&mut scanned, root as *mut RcHeader) };
    let from = scanned.positions_inspected();
    token.recall_for_test(true);
    let scan_answer = unsafe { scan::<AtomicCells>(&mut scanned, root as *mut RcHeader) };
    let scan_read = scanned.positions_inspected() - from;
    token.recall_for_test(false);
    scanned.reset();
    drop(scanned);

    token.recall_for_test(true);
    let mut plain =
        unsafe { TraceScratchArena::open_for_owner(record()) }.expect("the workspace was drawn");
    let plain_answer = unsafe { mark::<PlainCells>(&mut plain, root as *mut RcHeader) };
    let plain_read = plain.positions_inspected();
    plain.reset();
    drop(plain);
    token.recall_for_test(false);

    assert_eq!(
        (answer, read),
        (MarkResult::Recalled, RECALL_STRIDE),
        "the collector's mark stopped at the first reading of the recall"
    );
    assert_eq!(
        marked,
        MarkResult::Complete,
        "the recall was clear for the mark"
    );
    assert_eq!(
        scan_answer,
        ScanResult::Recalled,
        "the scan stopped at the recall"
    );
    assert!(
        scan_read <= RECALL_STRIDE,
        "the scan read {scan_read} positions, past one stride"
    );
    assert_eq!(
        (plain_answer, plain_read),
        (MarkResult::Complete, 0),
        "the owner's mark counts no position and is recalled by nobody"
    );

    unsafe { let_go(root) };
    reset_lanes();
}

/// A growth of a collector's arena under a standing recall draws no block and
/// answers as recalled, where the same growth with the recall clear draws one.
#[test]
fn a_growth_under_a_standing_recall_draws_no_block() {
    use crate::cycle::arena::TraceScratchArena;
    use crate::memory::block_pool::BLOCK_PAYLOAD;

    let _g = test_guard();
    let token = unsafe { &(*record()).token };
    let _clear = ClearTheRecall(token);

    // A request of a whole payload passes what the workspace has left, so
    // each of the two is a growth.
    let mut clear =
        unsafe { TraceScratchArena::open_for_owner(record()) }.expect("the workspace was drawn");
    let held_before = clear.blocks_held();
    let drawn = !clear.alloc(BLOCK_PAYLOAD).is_null();
    let clear_answer = (
        drawn,
        clear.was_recalled(),
        clear.blocks_held() - held_before,
    );
    clear.reset();
    drop(clear);

    token.recall_for_test(true);
    let mut recalled =
        unsafe { TraceScratchArena::open_for_owner(record()) }.expect("the workspace was drawn");
    let held_before = recalled.blocks_held();
    let drawn = !recalled.alloc(BLOCK_PAYLOAD).is_null();
    let recalled_answer = (
        drawn,
        recalled.was_recalled(),
        recalled.blocks_held() - held_before,
    );
    recalled.reset();
    drop(recalled);
    token.recall_for_test(false);

    assert_eq!(
        clear_answer,
        (true, false, 1),
        "with the recall clear the growth drew a block"
    );
    assert_eq!(
        recalled_answer,
        (false, true, 0),
        "under the recall the growth drew nothing and read as recalled"
    );
}

/// A grant whose mutator's recall stands before the batch is made goes back
/// with no batch: R keeps its root, and no trace is read. The recall is the
/// one a consent raises over a stack of withheld deaths already at its mark
/// (`crate::cycle::deferred_slot_reuse`, "The marks by stack length"), a
/// recall standing from before the consent being cleared by it.
#[test]
fn a_grant_recalled_before_its_batch_is_released_with_no_batch() {
    let _g = test_guard();
    let _end = RetireOnDrop;
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    // An empty vector: its closure is a stride's fraction, so a batch made
    // over it would complete before any reading of the recall.
    let empty = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    assert!(!empty.is_null(), "the array was allocated");
    let root = unsafe { a_root_over(&mut context, empty as *mut RcHeader, Tag::Array) };
    assert_eq!(candidate_count(), 1, "R holds the root alone");

    let token = unsafe { &(*record()).token };
    let _clear = ClearTheRecall(token);
    withhold_deaths_to_the_mark();
    let _ = testing::take_outcomes();
    testing::serve_rounds_at(1);
    testing::read_traced_batches(true);
    testing::confine_rounds_to(record());
    testing::permit_births(true);
    ensure_thread();
    let (mut grants, mut idle, mut batches) = (0, 0, 0);
    assert!(
        wait_until(
            || {
                let outcomes = testing::take_outcomes();
                grants += outcomes.grants;
                idle += outcomes.idle;
                batches += outcomes.batches;
                grants > 0 && idle > 0
            },
            A_BIRTH
        ),
        "a grant was read and released"
    );
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    testing::retire();
    token.recall_for_test(false);

    assert_eq!(
        (batches, traced.len()),
        (0, 0),
        "no batch was made under the recall"
    );
    assert_eq!(candidate_count(), 1, "R kept its root");

    unsafe { let_go(root) };
    reset_lanes();
}

/// The recall a stack at its mark raises at the consent stands before the
/// consent's swap publishes the grant, so the collector's reading before the
/// batch sees it: the mutator is held between its swap and anything it does
/// after it until the collector has chosen, and the grant still goes back
/// with no batch.
#[test]
fn a_consent_at_a_mark_is_read_as_a_recall_before_the_batch() {
    let _g = test_guard();
    let _end = RetireOnDrop;
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let empty = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    assert!(!empty.is_null(), "the array was allocated");
    let root = unsafe { a_root_over(&mut context, empty as *mut RcHeader, Tag::Array) };
    assert_eq!(candidate_count(), 1, "R holds the root alone");

    let token = unsafe { &(*record()).token };
    let _clear = ClearTheRecall(token);
    withhold_deaths_to_the_mark();
    let _ = testing::take_outcomes();
    testing::serve_rounds_at(1);
    testing::read_traced_batches(true);
    let before = testing::idle_and_traced_so_far();
    testing::after_the_next_consents_swap(Box::new(move || {
        let deadline = Instant::now() + A_BIRTH;
        while testing::idle_and_traced_so_far() == before {
            assert!(
                Instant::now() < deadline,
                "the collector chose over the grant"
            );
            std::thread::yield_now();
        }
    }));
    testing::confine_rounds_to(record());
    testing::permit_births(true);
    ensure_thread();
    let (mut grants, mut idle, mut batches) = (0, 0, 0);
    assert!(
        wait_until(
            || {
                let outcomes = testing::take_outcomes();
                grants += outcomes.grants;
                idle += outcomes.idle;
                batches += outcomes.batches;
                grants > 0 && (idle > 0 || batches > 0)
            },
            A_BIRTH
        ),
        "a grant was read and answered"
    );
    let traced = testing::take_traced_batches();
    testing::read_traced_batches(false);
    testing::retire();
    token.recall_for_test(false);

    assert_eq!(
        (batches, traced.len()),
        (0, 0),
        "no batch was made under the recall the consent raised"
    );
    assert_eq!(candidate_count(), 1, "R kept its root");

    unsafe { let_go(root) };
    reset_lanes();
}

/// Leave this thread's stack of deaths withheld under a foreign holder at
/// its mark, the holder gone and no drain run, as a grant ends before the
/// mutator's next free: the next consent then recalls the grant it opens.
fn withhold_deaths_to_the_mark() {
    use crate::cycle::deferred_slot_reuse::{DEATHS_MARK, foreign_withheld_count};
    // Dead arrays of no storage, freed as a teardown frees them: nothing
    // reads a dead slot's body but its first word.
    let slots: Vec<_> = (0..DEATHS_MARK)
        .map(|_| {
            let slot = unsafe { crate::memory::heap::entity_alloc(64) };
            assert!(!slot.is_null(), "the heap served");
            let header = slot as *mut RcHeader;
            unsafe {
                header.write(RcHeader::new(
                    MemoryCategory::GcHeap,
                    crate::refcount::EntityKind::Array.to_flags(),
                ));
                crate::refcount::set_header_refcount(header, 0);
            }
            slot
        })
        .collect();
    let mut holder = crate::cycle::token::testing::HeldByACollector::take(record_token(), false);
    for &slot in &slots {
        unsafe { crate::memory::stdapi::ll_free(slot) };
    }

    holder.release();
    let _ = crate::cycle::worker::take_the_recall_of(ELDER);
    assert_eq!(
        foreign_withheld_count(),
        DEATHS_MARK,
        "the deaths stand at the mark"
    );
}

fn record_token() -> *const crate::cycle::token::TraceToken {
    unsafe { &raw const (*record()).token }
}

/// A mutator whose consent the collector holds while it traces another
/// mutator's batch, and which then asks for its token at the start of that
/// batch's trace, is released at the trace's first reading of the recall, the
/// first root of the pass before the trace, and the batch goes on to its end:
/// the grant held behind it has no batch of its own to abandon.
#[test]
fn a_grant_held_behind_another_mutators_batch_is_released_within_a_stride() {
    let released_at = released_behind_another_batch(testing::at_the_start_of_the_next_trace);
    assert_eq!(
        released_at, 0,
        "the grant was released at the first reading after its mutator stood in the wait"
    );
}

/// The same grant, its mutator asking between the mark and the scan of the
/// other batch's trace, is released at the next reading of the stride.
#[test]
fn a_grant_behind_another_trace_is_released_at_the_strides_next_reading() {
    let released_at = released_behind_another_batch(testing::between_the_next_phases);
    assert!(
        released_at > 0 && released_at % RECALL_STRIDE == 0,
        "the grant was released at a reading of the stride, at {released_at} positions"
    );
}

/// Hold one mutator's grant behind another's batch over a vector of scalars,
/// have the first ask for its token at the point `ask_at` installs its act,
/// and answer the positions the other's trace had read when a reading released
/// the grant. The mutator behind took its token before the other batch ended.
///
/// The collector is the case's thread with a list of its own on a slot no
/// thread stands in, as in `the_standing_list`.
fn released_behind_another_batch(ask_at: fn(Box<dyn FnOnce() + Send>)) -> usize {
    const SLOT: usize = 6;
    let _g = test_guard();
    reset_lanes();
    let _wait = testing::HeldRequestWait::of(Duration::from_millis(100));

    // Asleep to every request: its request stands, the record on the list.
    let behind = Mutator::start_idling_with(|_| {});
    let behind_root = behind.run(|arena| {
        let mut context = LLContext { arena };
        let empty = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        assert!(!empty.is_null(), "the array was allocated");
        Sent(unsafe { a_root_over(&mut context, empty as *mut RcHeader, Tag::Array) })
    });
    let mut standing = Standing::new(SLOT);
    standing.start_a_round();
    assert_eq!(
        unsafe { serve(behind.record, SLOT, 1, &mut standing, serve_clock_now()) },
        Served::Unanswered,
        "the sleeping mutator's request stands"
    );

    // Consents at its next reading, between jobs.
    let traced = Mutator::start();
    let traced_root = traced.run(|arena| {
        let mut context = LLContext { arena };
        let (vector, tag) = unsafe { build(&mut context, Container::ScalarVector) };
        Sent(unsafe { a_root_over(&mut context, vector, tag) })
    });

    // Where `ask_at` places it, the mutator behind consents and asks for its
    // token; the trace goes on once it stands in the token's wait.
    let (took, behind_took) = std::sync::mpsc::channel::<Sent<Instant>>();
    let behind_jobs = behind.jobs.clone();
    let behind_token = unsafe { &raw const (*behind.record).token } as usize;
    let waiting_from = std::sync::Arc::new(std::sync::Mutex::new(None));
    let stamp = std::sync::Arc::clone(&waiting_from);
    ask_at(Box::new(move || {
        let token = unsafe { &*(behind_token as *const crate::cycle::token::TraceToken) };
        let before = token.waits();
        behind_jobs
            .send(Box::new(move |_| {
                crate::cycle::token::read_and_act_on_this_thread();
                drop(crate::cycle::token::HeldToken::take());
                took.send(Sent(Instant::now())).expect("the case waits");
            }))
            .expect("the mutator behind runs");
        let deadline = Instant::now() + A_BIRTH;
        while token.waits() == before {
            assert!(
                Instant::now() < deadline,
                "the mutator behind stood in its token's wait"
            );
            std::hint::spin_loop();
        }

        *stamp
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
    }));
    crate::cycle::arena::GRANTS_RELEASED_AT.store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
    standing.start_a_round();
    let served = unsafe { serve(traced.record, SLOT, 1, &mut standing, serve_clock_now()) };
    let released_at =
        crate::cycle::arena::GRANTS_RELEASED_AT.load(std::sync::atomic::Ordering::Relaxed);
    let traced_batch_ended = Instant::now();
    // A grant the trace did not release is released by the next pass at the
    // latest, the take behind it having recalled it before the batch.
    standing.start_a_round();
    let _ = unsafe { serve(super::record(), SLOT, 1, &mut standing, serve_clock_now()) };
    let behind_took = behind_took
        .recv_timeout(A_BIRTH)
        .expect("the mutator behind took its token")
        .into_inner();
    let waiting_from = waiting_from
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .expect("the mutator behind stood in the token's wait");
    eprintln!(
        "the mutator behind waited {:?}; the traced batch ended {:?} after it stood in the wait",
        behind_took.saturating_duration_since(waiting_from),
        traced_batch_ended.saturating_duration_since(waiting_from)
    );

    traced.run(move |_| unsafe {
        crate::gc::ll_gc_maybe_collect();
        let_go(traced_root.into_inner());
    });
    behind.run(move |_| unsafe { let_go(behind_root.into_inner()) });
    for mutator in [traced, behind] {
        mutator.run(|_| unsafe {
            crate::gc::ll_gc_collect_cycles();
        });
    }
    reset_lanes();

    assert!(
        matches!(served, Served::Batch { complete: true, .. }),
        "the traced batch ran to its end: {served:?}"
    );
    assert!(
        behind_took < traced_batch_ended,
        "the mutator behind waited out the other's batch"
    );
    released_at
}

/// Registered elements the array of the case below holds: fewer than a
/// stride, so that the first descent reads no recall, and more than the
/// stride's remainder after it, so that the first pass does.
const HELD_ELEMENTS: usize = 600;

const _: () = assert!(HELD_ELEMENTS < RECALL_STRIDE && 2 * HELD_ELEMENTS > RECALL_STRIDE);

/// A pass over held entries counts each toward the recall, as a cell is
/// counted: an array of registered elements held from outside is met in the
/// first descent (one position an element), every element is held, and the
/// pass that reads them above zero reaches the stride's reading and answers
/// `Recalled`. The elements have no cells, so a pass that counted nothing
/// would leave the final drain nothing to count either and the mark would
/// read `Complete` under a standing recall.
#[test]
fn a_pass_over_held_entries_reads_the_recall() {
    use crate::array::testing::push;
    use crate::cells::AtomicCells;
    use crate::cycle::arena::TraceScratchArena;
    use crate::cycle::mark::{MarkResult, mark};

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let empty = ClassBuilder::new("RecallHeldElement").build();
    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let elements: Vec<*mut Object> = (0..HELD_ELEMENTS)
        .map(|_| unsafe {
            let element = new_constructed(&mut context, empty, MemoryCategory::GcHeap);
            // `push` counts nothing, so the array's reference is retained
            // here; a retain and a non-final release register the element.
            ll_retain(element as *mut RcHeader);
            assert!(push(
                array,
                Value::entity(Tag::Object, element as *mut RcHeader)
            ));
            ll_retain(element as *mut RcHeader);
            assert!(!ll_release(element as *mut RcHeader));
            element
        })
        .collect();
    let root = unsafe { a_root_over(&mut context, array as *mut RcHeader, Tag::Array) };
    let token = unsafe { &(*record()).token };
    let _clear = ClearTheRecall(token);

    token.recall_for_test(true);
    let mut recalled =
        unsafe { TraceScratchArena::open_for_owner(record()) }.expect("the workspace was drawn");
    let answer = unsafe { mark::<AtomicCells>(&mut recalled, root as *mut RcHeader) };
    let read = recalled.positions_inspected();
    recalled.reset();
    drop(recalled);
    token.recall_for_test(false);

    assert_eq!((answer, read), (MarkResult::Recalled, RECALL_STRIDE));

    unsafe {
        let_go(root);
        for element in elements {
            assert!(
                ll_release(element as *mut RcHeader),
                "the case's reference was the last"
            );
            ll_object_die(element);
        }
    }
    reset_lanes();
}

/// Unregistered elements under a root whose first region a stop cuts: three
/// strides of the array's slots, so that the first reading falls inside it.
const CUT_ELEMENTS: usize = 3 * RECALL_STRIDE;

/// A registered root the case holds, over an array of `elements`
/// unregistered elements, each held by the array alone: the root's first
/// region is the array's slots.
///
/// # Safety
/// As [`build`].
unsafe fn a_root_over_a_wide_region(
    context: &mut LLContext,
    element: *const Class,
    elements: usize,
) -> *mut Object {
    use crate::array::testing::push;

    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    for _ in 0..elements {
        // `push` counts nothing: the element's creation reference is the
        // array's, and no decrement registers it.
        let element = unsafe { new_constructed(context, element, MemoryCategory::GcHeap) };
        assert!(unsafe { push(array, Value::entity(Tag::Object, element as *mut RcHeader),) });
    }
    unsafe { a_root_over(context, array as *mut RcHeader, Tag::Array) }
}

/// One serve of this thread's record whose trace the recall stops at its
/// first stride reading: the batch it traced and the verdicts it posted, the
/// recall cleared and P left standing.
fn a_serve_recalled_at_the_first_reading() -> (testing::TracedBatch, Vec<(*mut Object, Verdict)>) {
    testing::read_traced_batches(true);
    testing::recall_at_the_reading(1);
    let served = super::the_batch::served_by_a_collector();
    unsafe { &(*record()).token }.recall_for_test(false);
    let batches = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    let posted = standing_verdicts()
        .iter()
        .map(|&(root, verdict)| (root as *mut Object, verdict))
        .collect();
    (*batches.first().expect("the batch was traced"), posted)
}

/// A stop inside the mark's first regions leaves every root above zero
/// *unwalked* and halves K; at one root K halves no further, and the root
/// cut inside its own region is read live, out of R, so that it does not come
/// back cut at every grant (`dev/plans/S67.md`, S67.9, revision 3, G3,
/// "*unwalked* once"). Each root is held by the case and heads three strides
/// of unregistered elements, so the first reading falls inside the first
/// root's region.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_stop_inside_the_first_regions_posts_unwalked_and_halves_k_down_to_one_read_live_root() {
    use crate::journal::kinds::BATCH_END_RECALLED_IN_THE_TRACE;

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let element = ClassBuilder::new("CutRegionElement").build();
    let roots: Vec<*mut Object> = (0..2)
        .map(|_| unsafe { a_root_over_a_wide_region(&mut context, element, CUT_ELEMENTS) })
        .collect();
    assert_eq!(candidate_count(), 2, "R holds the roots alone");
    let _clear = ClearTheRecall(unsafe { &(*record()).token });
    unsafe { &*record() }.set_batch_size(2);

    let (batch, posted) = a_serve_recalled_at_the_first_reading();
    assert_eq!(batch.ending, BATCH_END_RECALLED_IN_THE_TRACE);
    assert_eq!(
        posted,
        roots
            .iter()
            .map(|&root| (root, Verdict::Unwalked))
            .collect::<Vec<_>>(),
        "both roots above zero, the stop inside the first regions"
    );
    assert_eq!(
        super::the_batch::record_batch_size(),
        1,
        "a stop inside the first regions halves K"
    );
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 2, "both written back into R untraced");

    let (batch, posted) = a_serve_recalled_at_the_first_reading();
    assert_eq!(batch.ending, BATCH_END_RECALLED_IN_THE_TRACE);
    assert_eq!(batch.roots, 1);
    assert_eq!(
        posted
            .iter()
            .map(|&(_, verdict)| verdict)
            .collect::<Vec<_>>(),
        vec![Verdict::ReadLive],
        "the one root cut inside its own region is read live"
    );
    assert_eq!(
        super::the_batch::record_batch_size(),
        1,
        "never below one root"
    );
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert_eq!(candidate_count(), 1, "the other root waits in R");

    for root in roots {
        unsafe { let_go(root) };
    }
    reset_lanes();
}

/// A stop inside the scan reads the colours the scan already gave: a root
/// whose met row reads zero but which a live root's scan reached is read
/// live, not proposed, so the owner walks nothing from it. R holds the inner
/// root first, then the root that holds it, then a root over three strides
/// whose scan the recall, raised between the phases, stops; the inner root's
/// only reference is the outer root's.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_stop_inside_the_scan_reads_a_root_the_scan_coloured_live_as_live() {
    use crate::journal::kinds::BATCH_END_RECALLED_IN_THE_TRACE;

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let element = ClassBuilder::new("ScanStopElement").build();
    let inner = unsafe { new_constructed(&mut context, element, MemoryCategory::GcHeap) };
    unsafe {
        ll_retain(inner as *mut RcHeader);
        assert!(
            !ll_release(inner as *mut RcHeader),
            "registered, held by the case"
        );
    }
    let outer = unsafe { a_root_over(&mut context, inner as *mut RcHeader, Tag::Object) };
    let wide = unsafe { a_root_over_a_wide_region(&mut context, element, CUT_ELEMENTS) };
    assert_eq!(candidate_count(), 3, "R holds the three roots alone");
    let _clear = ClearTheRecall(unsafe { &(*record()).token });
    unsafe { &*record() }.set_batch_size(3);

    let token = unsafe { &raw const (*record()).token } as usize;
    testing::between_the_next_phases(Box::new(move || {
        unsafe { &*(token as *const crate::cycle::token::TraceToken) }.recall_for_test(true)
    }));
    testing::read_traced_batches(true);
    let served = super::the_batch::served_by_a_collector();
    unsafe { &(*record()).token }.recall_for_test(false);
    let batches = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    assert_eq!(batches[0].ending, BATCH_END_RECALLED_IN_THE_TRACE);
    assert_eq!(
        standing_verdicts()
            .iter()
            .map(|&(root, verdict)| (root as *mut Object, verdict))
            .collect::<Vec<_>>(),
        vec![
            (inner, Verdict::ReadLive),
            (outer, Verdict::ReadLive),
            (wide, Verdict::ReadLive),
        ],
        "the inner root's row reads zero and its color live"
    );

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    unsafe {
        let_go(outer);
        let_go(wide);
    }
    reset_lanes();
}

/// Unregistered elements under a root whose trace the pool refuses: enough
/// rows and worklist segments to outgrow the workspace's first block.
const REFUSED_ELEMENTS: usize = if cfg!(miri) {
    4 * RECALL_STRIDE
} else {
    64 * RECALL_STRIDE
};

/// The reserve's blocks a hook on the collector's thread took, as addresses,
/// for the case to give back.
static RESERVE_TAKEN: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// A pool that refuses the collector inside the mark stops the trace as a
/// recall does and posts the snapshot: both roots above zero, the stop inside
/// the first regions, are *unwalked*, the end is the refusal's own, and K
/// halves. The hook at the trace's start, on the collector's thread, budgets
/// that thread's pool to nothing and takes its reserve, so the first growth
/// of the arena past its workspace is refused.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_pool_refusal_inside_the_mark_posts_the_snapshot() {
    use crate::journal::kinds::BATCH_END_REFUSED_IN_THE_TRACE;

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let element = ClassBuilder::new("RefusedRegionElement").build();
    let roots: Vec<*mut Object> = (0..2)
        .map(|_| unsafe { a_root_over_a_wide_region(&mut context, element, REFUSED_ELEMENTS) })
        .collect();
    unsafe { &*record() }.set_batch_size(2);

    testing::at_the_start_of_the_next_trace(Box::new(|| {
        // The collector thread ends with the serve, and its budget with it.
        std::mem::forget(crate::memory::block_pool::budget_blocks(0));
        let mut taken = RESERVE_TAKEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            let block = crate::memory::critical::draw();
            if block.is_null() {
                break;
            }
            taken.push(block as usize);
        }
    }));
    testing::read_traced_batches(true);
    let served = super::the_batch::served_by_a_collector();
    let batches = testing::take_traced_batches();
    testing::read_traced_batches(false);
    for block in std::mem::take(
        &mut *RESERVE_TAKEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    ) {
        crate::memory::critical::give_back(block as *mut crate::memory::block_pool::BlockHeader);
    }

    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    assert_eq!(batches[0].ending, BATCH_END_REFUSED_IN_THE_TRACE);
    assert_eq!(
        standing_verdicts()
            .iter()
            .map(|&(root, verdict)| (root as *mut Object, verdict))
            .collect::<Vec<_>>(),
        roots
            .iter()
            .map(|&root| (root, Verdict::Unwalked))
            .collect::<Vec<_>>(),
    );
    assert_eq!(super::the_batch::record_batch_size(), 1, "a stop halves K");

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    for root in roots {
        unsafe { let_go(root) };
    }
    reset_lanes();
}

/// R in the order the wind-down cases want: a root over three strides of
/// unregistered elements, then an inner root held by the outer one alone,
/// then the outer root the case holds. The worklist expands the outer root
/// first, which subtracts the inner one's only reference, and the first
/// stride reading falls inside the wide root's region.
unsafe fn wide_then_inner_then_outer(
    context: &mut LLContext,
) -> (*mut Object, *mut Object, *mut Object) {
    let element = ClassBuilder::new("WindDownElement").build();
    let wide = unsafe { a_root_over_a_wide_region(context, element, CUT_ELEMENTS) };
    let inner = unsafe { new_constructed(context, element, MemoryCategory::GcHeap) };
    unsafe {
        ll_retain(inner as *mut RcHeader);
        assert!(
            !ll_release(inner as *mut RcHeader),
            "registered, held by the case"
        );
    }
    let outer = unsafe { a_root_over(context, inner as *mut RcHeader, Tag::Object) };
    (wide, inner, outer)
}

/// One serve whose trace the recall at `level` reaches at its first stride
/// reading, inside the mark: the batch traced and the verdicts it posted.
fn a_serve_recalled_in_the_mark_at(
    level: u8,
) -> (testing::TracedBatch, Vec<(*mut Object, Verdict)>) {
    testing::read_traced_batches(true);
    testing::recall_at_the_reading_at_level(1, level);
    let served = super::the_batch::served_by_a_collector();
    unsafe { &(*record()).token }.recall_for_test(false);
    let batches = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    let posted = standing_verdicts()
        .iter()
        .map(|&(root, verdict)| (root as *mut Object, verdict))
        .collect();
    (*batches.first().expect("the batch was traced"), posted)
}

/// A wind-down inside the mark ends the mark, scans, and posts off the
/// colours: the inner root, whose row the mark read zero, is reached live from
/// the outer one by the scan and is not proposed. The same recall at the stop
/// level posts the snapshot, which proposes it — the owner then refutes it.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_wind_down_inside_the_mark_scans_and_proposes_no_root_a_live_one_reaches() {
    use crate::cycle::token::{RECALL_STOP, RECALL_WIND_DOWN};
    use crate::journal::kinds::{BATCH_END_RECALLED_IN_THE_TRACE, BATCH_END_WOUND_DOWN};

    for (level, ending, inner_verdict) in [
        (RECALL_WIND_DOWN, BATCH_END_WOUND_DOWN, Verdict::Unwalked),
        (
            RECALL_STOP,
            BATCH_END_RECALLED_IN_THE_TRACE,
            Verdict::Proposed,
        ),
    ] {
        let _g = test_guard();
        reset_lanes();
        let mut arena = Arena::new();
        let mut context = LLContext { arena: &mut arena };
        let (wide, inner, outer) = unsafe { wide_then_inner_then_outer(&mut context) };
        assert_eq!(candidate_count(), 3);
        let _clear = ClearTheRecall(unsafe { &(*record()).token });
        unsafe { &*record() }.set_batch_size(3);

        let (batch, posted) = a_serve_recalled_in_the_mark_at(level);
        assert_eq!(batch.ending, ending, "level {level}");
        let verdict_of = |root: *mut Object| {
            posted
                .iter()
                .find(|&&(posted, _)| posted == root)
                .map(|&(_, verdict)| verdict)
        };
        assert_eq!(verdict_of(inner), Some(inner_verdict), "level {level}");

        crate::cycle::queue::verdicts::discard_standing_verdicts();
        unsafe {
            let_go(outer);
            let_go(wide);
        }
        reset_lanes();
    }
}

/// A wind-down raised after the mark asks for nothing the scan does not
/// already do: the scan runs to its end and every root is posted off its
/// colours, the inner root read live; only the live list's walk stops at it.
/// The same recall at the stop level stops the scan
/// (`a_stop_inside_the_scan_reads_a_root_the_scan_coloured_live_as_live`).
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_wind_down_after_the_mark_lets_the_scan_end() {
    use crate::cycle::token::RECALL_WIND_DOWN;
    use crate::journal::kinds::BATCH_END_RECALLED_AFTER_THE_TRACE;

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let (wide, inner, outer) = unsafe { wide_then_inner_then_outer(&mut context) };
    let _clear = ClearTheRecall(unsafe { &(*record()).token });
    unsafe { &*record() }.set_batch_size(3);

    let token = unsafe { &raw const (*record()).token } as usize;
    testing::between_the_next_phases(Box::new(move || {
        unsafe { &*(token as *const crate::cycle::token::TraceToken) }
            .recall_at_level_for_test(RECALL_WIND_DOWN)
    }));
    testing::read_traced_batches(true);
    let served = super::the_batch::served_by_a_collector();
    unsafe { &(*record()).token }.recall_for_test(false);
    let batches = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    assert_eq!(batches[0].ending, BATCH_END_RECALLED_AFTER_THE_TRACE);
    assert_eq!(
        standing_verdicts()
            .iter()
            .map(|&(root, verdict)| (root as *mut Object, verdict))
            .collect::<Vec<_>>(),
        vec![
            (wide, Verdict::ReadLive),
            (inner, Verdict::ReadLive),
            (outer, Verdict::ReadLive),
        ]
    );

    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    unsafe {
        let_go(outer);
        let_go(wide);
    }
    reset_lanes();
}

/// R for the wind-down cases past the first regions: a garbage ring of two,
/// then a root the case holds over three strides of registered elements, each
/// held by the array alone. The worklist expands the held root's array first
/// — three strides, its elements held — then the ring; the pass over the held
/// elements reads one position each, so the fourth reading falls inside it.
unsafe fn a_ring_then_a_held_root_over_registered_elements(
    context: &mut LLContext,
) -> (Vec<*mut Object>, *mut Object) {
    use crate::array::testing::push;

    let node = ClassBuilder::new("WindDownRingNode")
        .prop("next", true)
        .build();
    let ring = unsafe { crate::cycle::testing::long_ring(&mut *context.arena, node, 2) };
    let element = ClassBuilder::new("WindDownHeldElement").build();
    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let items: Vec<*mut Object> = (0..CUT_ELEMENTS)
        .map(|_| unsafe {
            let item = new_constructed(context, element, MemoryCategory::GcHeap);
            ll_retain(item as *mut RcHeader);
            assert!(push(
                array,
                Value::entity(Tag::Object, item as *mut RcHeader)
            ));
            item
        })
        .collect();
    let held = unsafe { a_root_over(context, array as *mut RcHeader, Tag::Array) };
    // Registered behind the held root, so that a batch of three takes the
    // ring and the held root: the case's creation reference goes.
    for item in items {
        assert!(
            !unsafe { ll_release(item as *mut RcHeader) },
            "the array holds it"
        );
    }
    (ring, held)
}

/// A wind-down past the first regions scans and posts off the colours: the
/// garbage ring proposed with its set and freed by the owner, the held root,
/// whose own row reads above zero, read live. Red without the scan: every row
/// stays unclassified and nothing is posted.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_wind_down_past_the_first_regions_proposes_the_ring_and_reads_the_held_root_live() {
    use crate::cycle::token::RECALL_WIND_DOWN;
    use crate::journal::kinds::BATCH_END_WOUND_DOWN;

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let (ring, held) = unsafe { a_ring_then_a_held_root_over_registered_elements(&mut context) };
    let _clear = ClearTheRecall(unsafe { &(*record()).token });
    unsafe { &*record() }.set_batch_size(3);

    testing::read_traced_batches(true);
    testing::recall_at_the_reading_at_level(4, RECALL_WIND_DOWN);
    let served = super::the_batch::served_by_a_collector();
    unsafe { &(*record()).token }.recall_for_test(false);
    let batches = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    assert_eq!(batches[0].ending, BATCH_END_WOUND_DOWN);
    assert_eq!(
        standing_verdicts()
            .iter()
            .map(|&(root, verdict)| (root as *mut Object, verdict))
            .collect::<Vec<_>>(),
        vec![
            (ring[0], Verdict::Proposed),
            (ring[1], Verdict::Proposed),
            (held, Verdict::ReadLive),
        ]
    );

    assert_eq!(
        unsafe { crate::gc::ll_gc_maybe_collect() },
        2,
        "the ring is freed"
    );
    unsafe { let_go(held) };
    reset_lanes();
}

/// A stop raised inside the scan a wind-down runs cuts it, as a take does,
/// and the batch posts the snapshot under its own ending. Red with a scan
/// after a wind-down that stops at nothing: the take would wait for the whole
/// scan.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_stop_inside_a_wind_downs_scan_cuts_it() {
    use crate::cycle::token::RECALL_WIND_DOWN;
    use crate::journal::kinds::BATCH_END_WOUND_DOWN_THEN_CUT;

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let (_ring, held) = unsafe { a_ring_then_a_held_root_over_registered_elements(&mut context) };
    let _clear = ClearTheRecall(unsafe { &(*record()).token });
    unsafe { &*record() }.set_batch_size(3);

    testing::read_traced_batches(true);
    testing::recall_at_the_reading_at_level(1, RECALL_WIND_DOWN);
    testing::then_stop_at_the_reading(2);
    let served = super::the_batch::served_by_a_collector();
    unsafe { &(*record()).token }.recall_for_test(false);
    let batches = testing::take_traced_batches();
    testing::read_traced_batches(false);
    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    assert_eq!(batches[0].ending, BATCH_END_WOUND_DOWN_THEN_CUT);

    crate::cycle::queue::verdicts::discard_standing_verdicts();
    unsafe { let_go(held) };
    reset_lanes();
}

/// Past the first regions a wind-down reads live a root whose own row reads
/// above zero, and sends back *unwalked* one whose row reads zero and which
/// the scan coloured live only through another root: after a cut mark that
/// colour may stand on a referrer the mark never expanded, and a root read
/// live waits an epoch in the deferred lane. R holds the inner root, then the
/// outer root that alone holds it, then the held root over registered
/// elements, whose pass the fourth reading falls inside.
#[test]
#[cfg_attr(
    feature = "collector-chain",
    ignore = "under the chain the collector keeps a root read live or unwalked in its chain, not in P (`crate::cycle::chain`)"
)]
fn a_wind_down_sends_back_a_root_read_live_only_through_another() {
    use crate::array::testing::push;
    use crate::cycle::token::RECALL_WIND_DOWN;

    let _g = test_guard();
    reset_lanes();
    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let element = ClassBuilder::new("WindDownInnerElement").build();
    let inner = unsafe { new_constructed(&mut context, element, MemoryCategory::GcHeap) };
    unsafe {
        ll_retain(inner as *mut RcHeader);
        assert!(
            !ll_release(inner as *mut RcHeader),
            "registered, held by the case"
        );
    }
    let outer = unsafe { a_root_over(&mut context, inner as *mut RcHeader, Tag::Object) };
    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let items: Vec<*mut Object> = (0..CUT_ELEMENTS)
        .map(|_| unsafe {
            let item = new_constructed(&mut context, element, MemoryCategory::GcHeap);
            ll_retain(item as *mut RcHeader);
            assert!(push(
                array,
                Value::entity(Tag::Object, item as *mut RcHeader)
            ));
            item
        })
        .collect();
    let held = unsafe { a_root_over(&mut context, array as *mut RcHeader, Tag::Array) };
    for item in items {
        assert!(
            !unsafe { ll_release(item as *mut RcHeader) },
            "the array holds it"
        );
    }
    let _clear = ClearTheRecall(unsafe { &(*record()).token });
    unsafe { &*record() }.set_batch_size(3);

    testing::recall_at_the_reading_at_level(4, RECALL_WIND_DOWN);
    let served = super::the_batch::served_by_a_collector();
    unsafe { &(*record()).token }.recall_for_test(false);
    assert!(
        matches!(
            served,
            Served::Batch {
                complete: false,
                ..
            }
        ),
        "{served:?}"
    );
    assert_eq!(
        standing_verdicts()
            .iter()
            .map(|&(root, verdict)| (root as *mut Object, verdict))
            .collect::<Vec<_>>(),
        vec![
            (outer, Verdict::ReadLive),
            (held, Verdict::ReadLive),
            (inner, Verdict::Unwalked),
        ]
    );

    crate::cycle::queue::verdicts::discard_standing_verdicts();
    unsafe {
        let_go(outer);
        let_go(held);
    }
    reset_lanes();
}
