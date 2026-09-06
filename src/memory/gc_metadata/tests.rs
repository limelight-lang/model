use super::*;

use crate::cycle::arena::WORKSPACE_BUMP_BYTES;
use crate::memory::block_pool::{
    BLOCK_KIND_GC_METADATA, BlockPool, force_oom, load_block_kind, test_guard,
};

/// The blocks *this thread* has taken and not given back. The process figure
/// beside it is moved by every other test the suite is running at the same
/// time, so an exact assertion cannot be made against it
/// (`dev/POSTMORTEM.md`, "an exact assertion cannot be made against a
/// process-global ledger").
fn current() -> usize {
    thread_stats().current_blocks()
}

#[test]
fn a_shadow_arena_is_gc_owned_until_both_exit_paths_return_it() {
    let _g = test_guard();
    let before = current();
    let mut arena = crate::cycle::testing::open_arena();

    // The workspace is this thread's already, so the block this case follows
    // is the one the bump grows into past it.
    let room = arena.room_left();
    assert!(!arena.alloc(room).is_null());
    assert_eq!(
        current(),
        before,
        "the arena's first grant drew a block the guard had not already drawn"
    );

    let byte = arena.alloc(1);
    assert!(!byte.is_null());
    assert_eq!(current(), before + 1);
    let block = BlockHeader::of_ptr(byte);
    assert_eq!(
        unsafe { load_block_kind(&raw const (*block).kind) },
        BLOCK_KIND_GC_METADATA
    );

    arena.reset();
    assert_eq!(current(), before);

    // The reset rewound the bump into the workspace, so a second block takes
    // a second growth — and the drop is the other path that returns it.
    let room = arena.room_left();
    assert!(!arena.alloc(room).is_null());
    assert!(!arena.alloc(1).is_null());
    assert_eq!(current(), before + 1);
    drop(arena);
    assert_eq!(current(), before);
}

#[test]
fn a_threads_exit_ends_every_block_it_acquired() {
    let _g = test_guard();
    // The process figure, the claim being about blocks a thread that no longer
    // exists gave back — a reading a third thread can move, and one no
    // per-thread figure can replace (`PLAN.md`, "A gate flake, measured
    // 2026-09-03 and pre-existing").
    let before = stats().current_blocks();
    let blocks_before = BlockPool::global().blocks_out();

    std::thread::spawn(|| {
        assert!(crate::memory::heap::ll_thread_init());
        // One base block and the two spare segments the init fills, counted
        // for the child alone, which starts at nothing.
        assert!(current() >= 3);
    })
    .join()
    .unwrap();

    assert_eq!(stats().current_blocks(), before);
    let kept = usize::from(cfg!(feature = "debug-journal"));
    assert!(BlockPool::global().blocks_out() <= blocks_before + kept);
}

#[test]
fn a_critical_reserve_block_is_charged_only_while_the_arena_holds_it() {
    let _g = test_guard();
    assert!(crate::memory::critical::replenish());
    let before = current();
    let mut arena = crate::cycle::testing::open_arena();

    // Past the workspace, so the grant below has to ask an allocation path.
    let room = arena.room_left();
    assert!(!arena.alloc(room).is_null());

    let oom = force_oom();
    let byte = arena.alloc(1);
    drop(oom);
    assert!(!byte.is_null(), "the critical reserve served the refusal");
    assert_eq!(current(), before + 1);

    arena.reset();
    assert_eq!(current(), before);
}

#[test]
fn a_block_the_collector_never_owned_is_refused_before_the_counter_moves() {
    let _g = test_guard();
    let before = current();
    let ordinary = BlockPool::global().get();
    assert!(!ordinary.is_null());

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        release(ordinary);
    }));
    assert!(refused.is_err());
    assert_eq!(current(), before);

    BlockPool::global().put(ordinary);
}

#[test]
fn a_second_return_fails_before_the_counter_can_wrap() {
    let _g = test_guard();
    let before = current();
    let block = acquire();
    assert!(!block.is_null());
    release(block);

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        release(block);
    }));
    assert!(refused.is_err());
    assert_eq!(current(), before);
}

/// The three refusals that keep a block from crossing the boundary
/// unaccounted. Each is the only thing standing between a shortcut and a
/// counter that drifts without anyone noticing, so each is exercised rather
/// than trusted.
#[test]
fn the_pool_refuses_a_block_collection_still_owns() {
    let _g = test_guard();
    let before = current();
    let block = acquire();
    assert!(!block.is_null());

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        BlockPool::global().put(block);
    }));
    assert!(refused.is_err(), "the pool took a GC-stamped block");
    assert_eq!(current(), before + 1, "and the block is still charged");

    release(block);
    assert_eq!(current(), before);
}

#[test]
fn the_critical_reserve_refuses_a_block_collection_still_owns() {
    let _g = test_guard();
    let before = current();
    // Below capacity, which is the arm that keeps the block rather than
    // passing it to the pool. At capacity the pool's own refusal would
    // answer and this reserve's would go untested.
    assert!(crate::memory::critical::replenish());
    let drawn = crate::memory::critical::draw();
    assert!(!drawn.is_null());

    let block = acquire();
    assert!(!block.is_null());

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::memory::critical::give_back(block);
    }));
    assert!(refused.is_err(), "the reserve took a GC-stamped block");
    assert_eq!(current(), before + 1, "and the block is still charged");

    release(block);
    assert_eq!(current(), before);
    crate::memory::critical::give_back(drawn);
}

#[test]
fn adoption_refuses_a_source_that_is_not_the_reserve() {
    let _g = test_guard();
    let before = current();
    // Straight from the pool, so it is `FREE` where `adopt` demands the
    // `ARENA` stamp every block in the critical reserve carries.
    let ordinary = BlockPool::global().get();
    assert!(!ordinary.is_null());

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        adopt(ordinary);
    }));
    assert!(refused.is_err(), "adoption crossed the wrong boundary");
    assert_eq!(current(), before, "and charged nothing");

    BlockPool::global().put(ordinary);
}

#[test]
fn bytes_and_high_water_are_derived_from_the_block_count() {
    let _g = test_guard();
    let before = current();
    let block = acquire();
    assert!(!block.is_null());

    // The derivation is arithmetic over one reading, so it holds of the
    // process figure whatever another thread is doing; the count that rose by
    // one is this thread's.
    let held = stats();
    assert_eq!(current(), before + 1);
    assert_eq!(held.current_bytes(), held.current_blocks() * BLOCK_SIZE);
    assert!(held.peak_blocks() >= held.current_blocks());
    assert_eq!(held.peak_bytes(), held.peak_blocks() * BLOCK_SIZE);

    release(block);
    assert_eq!(current(), before);
}

/// Bytes in use inside the blocks collection owns: the half of the answer
/// that says how much of a reserved block is working memory.
fn in_use() -> usize {
    thread_stats().current_bytes_in_use()
}

#[test]
fn a_block_held_and_empty_is_reservation_and_no_bytes_in_use() {
    let _g = test_guard();
    let before = thread_stats();
    let block = acquire();
    assert!(!block.is_null());

    assert_eq!(thread_stats().current_blocks(), before.current_blocks() + 1);
    assert_eq!(
        in_use(),
        before.current_bytes_in_use(),
        "reservation is the physical axis and moves no logical byte"
    );

    release(block);
    assert_eq!(thread_stats().current_blocks(), before.current_blocks());
    assert_eq!(in_use(), before.current_bytes_in_use());
}

#[test]
fn a_reset_enters_the_bump_it_rewinds_in_the_high_water_figure() {
    let _g = test_guard();
    // A high-water figure never falls, so an exact rise is only assertable
    // from a known baseline.
    lower_thread_peak_to_current();
    let before = thread_stats();
    let mut arena = crate::cycle::testing::open_arena();

    assert!(!arena.alloc(1).is_null());
    assert_eq!(
        in_use(),
        before.current_bytes_in_use(),
        "the block still under the bump is reserved rather than published"
    );

    arena.reset();
    assert_eq!(
        in_use(),
        before.current_bytes_in_use(),
        "the rewind releases the bump rather than charging it"
    );
    assert_eq!(
        thread_stats().peak_bytes_in_use(),
        before.current_bytes_in_use() + 8,
        "one byte granted is eight bytes of bump, and the high-water keeps them"
    );
}

#[test]
fn a_block_crossing_publishes_the_bump_it_abandons() {
    let _g = test_guard();
    lower_thread_peak_to_current();
    let before = thread_stats();
    let mut arena = crate::cycle::testing::open_arena();

    // The workspace's bump region and not the whole payload: the grant has to
    // be the one that leaves nothing, or the crossing below is a growth over a
    // block that still had room and the figure it publishes is not the
    // workspace's.
    assert!(!arena.alloc(WORKSPACE_BUMP_BYTES).is_null());
    assert_eq!(in_use(), before.current_bytes_in_use());

    // The second grant cannot fit, so the workspace leaves the bump —
    // consumed to the byte, which is the instant its figure is exact. Held
    // rather than returned, and charged all the same: the bytes stay in use
    // until the reset rewinds over them. The fixed region at the workspace's
    // head is outside the figure, being memory the thread holds whether or
    // not a collection is running (`cycle::arena::TraceScratchArena::residue`).
    assert!(!arena.alloc(8).is_null());
    assert_eq!(
        in_use(),
        before.current_bytes_in_use() + WORKSPACE_BUMP_BYTES,
        "the bump region the workspace left is published whole"
    );

    arena.reset();
    assert_eq!(in_use(), before.current_bytes_in_use());
    assert_eq!(
        thread_stats().peak_bytes_in_use(),
        before.current_bytes_in_use() + WORKSPACE_BUMP_BYTES + 8,
        "the crossing and the reset are both in the high-water figure"
    );
}

#[test]
fn a_second_reset_publishes_nothing_and_the_figure_cannot_underflow() {
    let _g = test_guard();
    lower_thread_peak_to_current();
    let before = thread_stats();
    let mut arena = crate::cycle::testing::open_arena();
    assert!(!arena.alloc(64).is_null());

    arena.reset();
    let after = thread_stats();
    arena.reset();

    assert_eq!(after.current_bytes_in_use(), before.current_bytes_in_use());
    assert_eq!(
        after.peak_bytes_in_use(),
        before.current_bytes_in_use() + 64
    );
    assert_eq!(in_use(), after.current_bytes_in_use());
    assert_eq!(
        thread_stats().peak_bytes_in_use(),
        after.peak_bytes_in_use(),
        "the second reset finds a rewound bump and enters nothing over a settled ledger"
    );
}

/// What the per-thread reading is for: another thread's work moves the process
/// figures under an assertion and leaves this thread's alone.
///
/// The two threads are ordered by the channels rather than by a sleep, so the
/// concurrent charge lands strictly between the two readings every run. This
/// is the failure that reached the gate about once in twenty-five runs at
/// sixteen threads, made to happen every time (`dev/POSTMORTEM.md`, "an exact
/// assertion cannot be made against a process-global ledger").
///
/// The process figures are read by no assertion here. They are what the other
/// thread moves, and every other thread the suite is running moves them too,
/// so an exact claim about them is the defect this case exists for.
#[test]
fn this_threads_figures_do_not_move_when_another_thread_charges() {
    use std::sync::mpsc::channel;

    // The guard here and none on the child, which is the shape of the defect:
    // a thread that never took the lock charges while a thread holding it is
    // reading. Held rather than skipped, because the child is itself a mover —
    // the cases above that still read the process figures are entitled to the
    // exclusion the guard gives them.
    let _g = test_guard();
    let (charge_now, charge_when_told) = channel();
    let (charged, wait_for_the_charge) = channel();
    let other = std::thread::spawn(move || {
        charge_when_told.recv().expect("the reading was opened");
        // Read against the child's own zero, which is the second half of the
        // claim: a reading that always answered nothing would satisfy the
        // parent's assertion below and say nothing at all.
        let untouched = thread_stats();
        assert_eq!(untouched.current_blocks(), 0);
        assert_eq!(untouched.current_bytes_in_use(), 0);

        let block = acquire();
        assert!(!block.is_null());
        charge(64);
        // At least, rather than exactly: a `debug-journal` build draws the
        // thread's ring on this same path, and it is GC memory too.
        let held = thread_stats();
        assert!(held.current_blocks() >= 1, "the child counts its own block");
        assert!(held.current_bytes_in_use() >= 64, "and its own charge");

        charged.send(()).expect("the reading is still open");
        charge_when_told.recv().expect("the reading was closed");
        discharge(64);
        release(block);
        let given_back = thread_stats();
        assert_eq!(
            given_back.current_blocks(),
            held.current_blocks() - 1,
            "and gave the block back"
        );
        assert_eq!(
            given_back.current_bytes_in_use(),
            held.current_bytes_in_use() - 64,
            "and the charge"
        );
        assert_eq!(
            (given_back.peak_blocks(), given_back.peak_bytes_in_use()),
            (held.current_blocks(), held.current_bytes_in_use()),
            "the child's high-water pair keeps what it held"
        );
    });

    lower_thread_peak_to_current();
    let before = thread_stats();

    charge_now.send(()).expect("the other thread is running");
    wait_for_the_charge
        .recv()
        .expect("the other thread charged");

    // Every field, the high-water pair included: a concurrent charge and its
    // discharge leave the current figures where they were and the high-water
    // pair above it (`dev/POSTMORTEM.md`, "an exact assertion cannot be made
    // against a process-global ledger").
    assert_eq!(
        thread_stats(),
        before,
        "another thread's block and bytes were counted against this one"
    );

    charge_now.send(()).expect("the other thread is waiting");
    other.join().expect("the other thread gave everything back");
    assert_eq!(thread_stats(), before);
}

/// Every write to a process figure carries the write to this thread's beside
/// it.
///
/// A textual guard over the module's own source, in the form this crate uses
/// for a convention no type states — the thread-local list in
/// `memory::critical::tests::where_the_first_touch_happens` is the pattern.
/// The mirror is five hand-written blocks, and a transition added later moves
/// the process figures while leaving this thread's still; the roughly forty
/// exact assertions that read `thread_stats` would then stop constraining that
/// path, and every one of them would keep passing.
#[test]
#[cfg_attr(
    miri,
    ignore = "reads the crate's sources; `opendir` is unavailable under Miri's isolation, \
              and the abort takes the whole slice with it"
)]
fn every_write_to_a_process_figure_moves_this_threads_figures_too() {
    /// The four process counters, and the operations that move one. A read is
    /// `load`, which no mirror is owed.
    const FIGURES: [&str; 4] = ["CURRENT", "PEAK", "IN_USE", "IN_USE_PEAK"];
    const WRITES: [&str; 5] = [
        "fetch_add(",
        "fetch_sub(",
        "fetch_max(",
        "fetch_update(",
        "store(",
    ];

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/memory/gc_metadata.rs");
    let source = std::fs::read_to_string(&path).expect("the module's own source is readable");

    // Function by function, split at a line that opens one: every writer in
    // this module is a top-level function, and the mirror it owes sits in the
    // same body.
    let mut bodies: Vec<(String, String)> = Vec::new();
    for line in source.lines() {
        let opener = line
            .strip_prefix("pub(crate) fn ")
            .or_else(|| line.strip_prefix("pub fn "))
            .or_else(|| line.strip_prefix("fn "));
        if let Some(rest) = opener {
            let name = rest.split(['(', '<']).next().unwrap_or(rest).to_owned();
            bodies.push((name, String::new()));
        }

        if let Some((_, body)) = bodies.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }

    // Whitespace out, comments out: a `fetch_update` sits on the line below the
    // counter it moves, and a counter named in a doc comment moves nothing.
    let writes_a_process_figure = |body: &str| {
        let code: String = body
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .flat_map(|line| line.chars().filter(|c| !c.is_whitespace()))
            .collect();
        FIGURES.iter().any(|figure| {
            WRITES
                .iter()
                .any(|write| code.contains(&format!("{figure}.{write}")))
        })
    };

    let unmirrored: Vec<&str> = bodies
        .iter()
        .filter(|(_, body)| writes_a_process_figure(body) && !body.contains("move_thread_figures"))
        .map(|(name, _)| name.as_str())
        .collect();

    assert!(
        unmirrored.is_empty(),
        "these move a process figure and not this thread's: {unmirrored:?}"
    );
    // The guard is worth nothing if the search finds nothing: the five writers
    // are named here, so a rename of a counter fails this rather than emptying
    // it.
    let mirrored: Vec<&str> = bodies
        .iter()
        .filter(|(_, body)| writes_a_process_figure(body))
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        mirrored,
        ["charge", "mark_peak", "discharge", "acquired", "released"]
    );
}
