//! The weak table's storage, measured at the one site that can answer:
//! the test binary's counting global allocator.
//!
//! The four allocation counts were seen red on `8ccf426`, where the table is a
//! `Box<HashMap<_, _>>`; what each records is the figure the change erases.
//! The tests added after them read the table's own accessors and could not
//! have been compiled against that tree.

use super::*;

use crate::test_support::allocation_probe;

/// Draw this thread's long-lived buffer arena before the window opens.
///
/// The arena itself is one `Box` per thread, made on its first use and
/// nothing to do with the table that happens to be the first user of it in a
/// test. Taken outside every measurement below, so what each measures is the
/// path it names.
fn warm_the_buffer_arena() {
    crate::memory::buffer_arena::with_buffer_arena(|_| ());
}

/// One GC-heap object of a class of its own, so two targets never share
/// an address the table could collapse.
unsafe fn a_target(ctx: *mut LLContext, name: &str) -> *mut Object {
    let cls = ClassBuilder::new(name).build();
    unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) }
}

unsafe fn retire(target: *mut Object, cell: *mut LLWeakRef) {
    unsafe {
        assert!(ll_release(target as *mut RcHeader));
        crate::object::ll_object_die(target);
        assert!(ll_release(cell as *mut RcHeader));
        crate::object::ll_entity_die(cell as *mut RcHeader);
    }
}

#[test]
fn the_first_create_reaches_no_global_allocator() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();
    with_ctx(|ctx| {
        let target = unsafe { a_target(ctx, "FirstCreateTarget") };

        warm_the_buffer_arena();
        let _ = allocation_probe::take_all();
        let cell = unsafe { ll_weakref_create(ctx, target as *mut RcHeader) };
        let (heap, _pool) = allocation_probe::take_all();

        assert!(!cell.is_null());
        assert_eq!(
            heap, 0,
            "the table's first row went to the global allocator"
        );
        unsafe { retire(target, cell) };
    });
}

#[test]
fn a_table_that_grows_reaches_no_global_allocator() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();
    const TARGETS: usize = 200;
    with_ctx(|ctx| {
        let mut pairs = Vec::with_capacity(TARGETS);
        for index in 0..TARGETS {
            pairs.push(unsafe { a_target(ctx, &format!("GrowTarget{index}")) });
        }

        warm_the_buffer_arena();
        let mut cells: Vec<*mut LLWeakRef> = Vec::with_capacity(TARGETS);
        let capacity_before = table::capacity();
        let _ = allocation_probe::take_all();
        for target in &pairs {
            cells.push(unsafe { ll_weakref_create(ctx, *target as *mut RcHeader) });
        }

        let (heap, _pool) = allocation_probe::take_all();
        assert_eq!(
            heap, 0,
            "growing to {TARGETS} rows went to the global allocator"
        );
        assert_eq!(capacity_before, 0, "the run started with a table already");
        assert_eq!(
            table::capacity(),
            512,
            "{TARGETS} rows reach 512 through three growths, and a run that \
             measured a table which never grew would pass every count above"
        );
        for (target, cell) in pairs.into_iter().zip(cells) {
            unsafe { retire(target, cell) };
        }
    });
}

#[test]
fn a_death_notification_reaches_no_global_allocator() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();
    with_ctx(|ctx| {
        let target = unsafe { a_target(ctx, "NotifyTarget") };
        let cell = unsafe { ll_weakref_create(ctx, target as *mut RcHeader) };

        warm_the_buffer_arena();
        let _ = allocation_probe::take_all();
        unsafe {
            assert!(ll_release(target as *mut RcHeader));
            crate::object::ll_object_die(target);
        }
        let (heap, _pool) = allocation_probe::take_all();

        assert_eq!(heap, 0, "the row's removal went to the global allocator");
        assert!(unsafe { (*cell).target }.is_null());
        unsafe {
            assert!(ll_release(cell as *mut RcHeader));
            crate::object::ll_entity_die(cell as *mut RcHeader);
        }
    });
}

/// One arena of `TARGETS` objects reset, with a weak reference taken on each
/// when `weak` is set. The answer is what the reset asked the global
/// allocator for, and the cells, which the caller retires.
fn a_reset_of_eight_objects(weak: bool) -> (usize, Vec<*mut LLWeakRef>) {
    const TARGETS: usize = 8;
    let cls = ClassBuilder::new("ArenaWeakTarget").build();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let mut cells = Vec::with_capacity(TARGETS);
    for _ in 0..TARGETS {
        let target = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        if weak {
            cells.push(unsafe { ll_weakref_create(&mut ctx, target as *mut RcHeader) });
        }
    }

    warm_the_buffer_arena();
    let _ = allocation_probe::take_all();
    unsafe { crate::promote::arena_reset_full(&mut arena) };
    let (heap, _pool) = allocation_probe::take_all();
    (heap, cells)
}

#[test]
fn draining_the_weak_log_asks_the_global_allocator_for_nothing_extra() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();

    // The control arm runs the same reset over the same objects with no weak
    // reference taken, so the difference is the weak walk and nothing else:
    // the reset's own collect-first buffers are `promote`'s, and no slice of
    // S36.9 claims them.
    let (without_weak, _) = a_reset_of_eight_objects(false);
    let (with_weak, cells) = a_reset_of_eight_objects(true);

    assert_eq!(
        with_weak, without_weak,
        "the reset's weak walk went to the global allocator"
    );
    for cell in cells {
        assert!(unsafe { (*cell).target }.is_null());
        unsafe {
            assert!(ll_release(cell as *mut RcHeader));
            crate::object::ll_entity_die(cell as *mut RcHeader);
        }
    }
}

/// A raised refusal flag that lowers itself on the way out of the scope,
/// including the way out a panic takes.
struct Refusing(&'static std::sync::atomic::AtomicBool);

impl Refusing {
    fn raise(flag: &'static std::sync::atomic::AtomicBool) -> Self {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        Refusing(flag)
    }
}

impl Drop for Refusing {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

#[test]
fn a_refused_first_table_answers_null_and_leaves_the_target_alone() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();
    with_ctx(|ctx| {
        let target = unsafe { a_target(ctx, "RefusedFirstTable") };
        let before = crate::memory::buffer_arena::refusals();
        let refused = {
            let _r = Refusing::raise(&crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED);
            unsafe { ll_weakref_create(ctx, target as *mut RcHeader) }
        };

        assert!(refused.is_null(), "a table nobody funded served a cell");
        assert!(
            crate::memory::buffer_arena::refusals() > before,
            "the refusal never fired, so the null came from somewhere else"
        );
        assert_eq!(
            unsafe { crate::refcount::entity_flags(target) } & HAS_WEAK_REFERENCES,
            0,
            "the gate bit went up over a row that was never written"
        );

        // The refusal costs the target nothing: the next create builds the
        // cell it would have.
        let cell = unsafe { ll_weakref_create(ctx, target as *mut RcHeader) };
        assert!(!cell.is_null());
        unsafe { retire(target, cell) };
    });
}

#[test]
fn a_refused_growth_answers_null_and_leaves_every_row_standing() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();
    with_ctx(|ctx| {
        // One short of the load the next insert would cross, so the refusal
        // lands on the growth and on nothing else.
        let mut held = Vec::new();
        while table::capacity() == 0 || table::len() < table::capacity() / 2 {
            let index = held.len();
            let target = unsafe { a_target(ctx, &format!("RefusedGrowth{index}")) };
            held.push((target, unsafe {
                ll_weakref_create(ctx, target as *mut RcHeader)
            }));
        }

        let rows = table::len();
        let capacity = table::capacity();
        let target = unsafe { a_target(ctx, "RefusedGrowthLast") };
        let before = crate::memory::buffer_arena::refusals();
        let refused = {
            let _r = Refusing::raise(&crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED);
            unsafe { ll_weakref_create(ctx, target as *mut RcHeader) }
        };

        assert!(refused.is_null());
        assert!(crate::memory::buffer_arena::refusals() > before);
        assert_eq!(table::len(), rows, "the refused growth moved a row");
        assert_eq!(table::capacity(), capacity, "the refused growth took hold");
        for (target, cell) in &held {
            assert_eq!(
                unsafe { ll_weakref_create(ctx, *target as *mut RcHeader) },
                *cell,
                "a row was lost under the refused growth"
            );
            unsafe { crate::refcount::ll_release(*cell as *mut RcHeader) };
        }

        unsafe {
            assert!(ll_release(target as *mut RcHeader));
            crate::object::ll_object_die(target);
        }
        for (target, cell) in held {
            unsafe { retire(target, cell) };
        }
    });
}

#[test]
fn the_weak_table_moves_no_figure_of_the_collection_ledger() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();
    with_ctx(|ctx| {
        // The high-water figures are process-global and never fall, so an
        // equality over them is vacuous until `gc_metadata::lower_peak_to_current`
        // lowers them to the current ones.
        crate::memory::gc_metadata::lower_peak_to_current();
        let before = crate::memory::gc_metadata::stats();
        let mut held = Vec::new();
        for index in 0..40 {
            let target = unsafe { a_target(ctx, &format!("LedgerTarget{index}")) };
            held.push((target, unsafe {
                ll_weakref_create(ctx, target as *mut RcHeader)
            }));
        }

        for (target, cell) in held {
            unsafe { retire(target, cell) };
        }

        crate::weak::dispose();
        assert_eq!(
            crate::memory::gc_metadata::stats(),
            before,
            "the weak table is the mutator's memory and no figure of \
             collection's may move with it"
        );
    });
}

#[test]
fn the_reset_drain_moves_no_figure_of_the_collection_ledger() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();
    crate::memory::gc_metadata::lower_peak_to_current();
    let before = crate::memory::gc_metadata::stats();
    let (_heap, cells) = a_reset_of_eight_objects(true);

    assert_eq!(
        crate::memory::gc_metadata::stats(),
        before,
        "the weak walk of an arena reset moved a figure of collection's ledger"
    );
    for cell in cells {
        assert!(unsafe { (*cell).target }.is_null());
        unsafe {
            assert!(ll_release(cell as *mut RcHeader));
            crate::object::ll_entity_die(cell as *mut RcHeader);
        }
    }
}

#[test]
fn a_growth_gives_the_payload_it_left_back_to_the_buffer_arena() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();
    with_ctx(|ctx| {
        let mut held = Vec::new();
        while table::capacity() == 0 || table::len() < table::capacity() / 2 {
            let index = held.len();
            let target = unsafe { a_target(ctx, &format!("FreedPayload{index}")) };
            held.push((target, unsafe {
                ll_weakref_create(ctx, target as *mut RcHeader)
            }));
        }

        let left_behind = table::payload();
        let left_behind_bytes = table::payload_bytes();
        let target = unsafe { a_target(ctx, "FreedPayloadLast") };
        held.push((target, unsafe {
            ll_weakref_create(ctx, target as *mut RcHeader)
        }));
        assert_ne!(table::payload(), left_behind, "the growth never happened");

        // Under critical pressure the arena serves from its free list by first
        // fit, taking any chunk recorded at or above the request. A request of
        // its own size must find this chunk, which says it was freed at all;
        // a request of twice the size must not, which pins the size the free
        // recorded. In that order: each request frees what it took back to the
        // head of the list, so the wider one would otherwise be the fit the
        // narrower request meets.
        assert_eq!(
            crate::test_support::chunk_from_the_free_list(left_behind_bytes),
            left_behind,
            "the growth kept the payload it copied out of"
        );
        assert_ne!(
            crate::test_support::chunk_from_the_free_list(left_behind_bytes * 2),
            left_behind,
            "the growth returned the payload under the wrong size"
        );

        for (target, cell) in held {
            unsafe { retire(target, cell) };
        }
    });
}

#[test]
fn disposal_gives_the_last_payload_back_to_the_buffer_arena() {
    let _g = crate::memory::block_pool::test_guard();
    crate::weak::dispose();
    with_ctx(|ctx| {
        let target = unsafe { a_target(ctx, "DisposedPayload") };
        let cell = unsafe { ll_weakref_create(ctx, target as *mut RcHeader) };
        let payload = table::payload();
        let bytes = table::payload_bytes();
        unsafe { retire(target, cell) };

        crate::weak::dispose();
        assert!(table::payload().is_null());
        assert_eq!(
            crate::test_support::chunk_from_the_free_list(bytes),
            payload,
            "thread exit would have abandoned the table's payload"
        );
    });
}
