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
        !unsafe { &(*record()).token }.mutator_waits(),
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
/// ask and leaves the batch unwalked.
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
        "{container:?}: a recalled trace is abandoned, its roots unwalked"
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

/// A recall of a batch of several roots posts each of them once, unwalked,
/// and advances R past all of them: `FinishThePosts` on the recall's path.
#[test]
fn a_recalled_batch_posts_every_root_once_unwalked_and_advances_r() {
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
            .map(|&root| (root, Verdict::Unwalked))
            .collect::<Vec<_>>(),
        "every root posted once, unwalked, in R's order"
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
/// with no batch: R keeps its root, and no trace is read.
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
    token.recall_for_test(true);
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

/// A mutator whose consent the collector holds while it traces another
/// mutator's batch, and which then asks for its token, is released within a
/// stride of that batch, and the batch goes on to its end: the grant held
/// behind it has no batch of its own to abandon.
///
/// The collector is the case's thread with a list of its own on a slot no
/// thread stands in, as in `the_standing_list`.
#[test]
fn a_grant_held_behind_another_mutators_batch_is_released_within_a_stride() {
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

    // At the traced batch's start the mutator behind consents and asks for
    // its token; the trace goes on once it stands in the token's wait.
    let (took, behind_took) = std::sync::mpsc::channel::<Sent<Instant>>();
    let behind_jobs = behind.jobs.clone();
    let behind_token = unsafe { &raw const (*behind.record).token } as usize;
    let waiting_from = std::sync::Arc::new(std::sync::Mutex::new(None));
    let stamp = std::sync::Arc::clone(&waiting_from);
    testing::at_the_start_of_the_next_trace(Box::new(move || {
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
    assert_eq!(
        released_at, RECALL_STRIDE,
        "the grant was released at the first reading after its mutator stood in the wait"
    );
}
