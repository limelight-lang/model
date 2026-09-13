//! Measurement probe, not a correctness test: what the reset's grouping of
//! survivors by block costs on the clock, against the `HashMap<usize,
//! Vec<usize>>` it replaced (`dev/BENCHMARKS.md`, "S47.6 the reset's
//! grouping leaves the global allocator").
//!
//! No benchmark drives `arena_reset_full` — `benches/lifecycle.rs` reaches
//! the ABI and builds no arena of survivors — so the arm that leaves has to
//! be timed here, on the shape the grouping is sensitive to: how many
//! survivors a reset promotes, and over how many blocks. The figure is the
//! whole reset rather than the grouping alone, because the grouping has no
//! entry point of its own.
//!
//! **The two shapes differ by more than the block count**, and a reading
//! that treats their difference as the per-block cost overstates it: at 120
//! survivors a block the fixture fills every block, so every list misses the
//! block's own tail and is placed elsewhere with a hold taken for it, and
//! the reset returns nineteen more blocks to the pool inside the timed
//! region. What the two shapes do bound is the direction, and each of them
//! compares like with like across the change.
//!
//! **The timed region is one call**, and `black_box` stands where an
//! optimizer could otherwise fold the shape away: around the arena the reset
//! is given, and around the fixture's own bound, which decides how many
//! blocks the reset meets (`dev/BENCHMARKS.md`, "what the release-at-reset
//! record costs, and the statistic that decides the answer", on why a bound
//! a compiler can see changes what is measured).
//!
//! Run it in release, explicitly:
//! `cargo test --release --lib what_the_grouping_costs -- --ignored --nocapture`.

use super::*;
use std::hint::black_box;
use std::time::Instant;

/// One reset's worth of survivors: a chain of arena objects, the first of
/// them escaped into a heap holder, cut into blocks of `per_block` by
/// filling each block's tail once it has taken that many.
///
/// One raw pointer per arena and per context, reused, as every fixture here
/// does (`dev/WORKFLOW.md`, Miri).
struct Chain {
    arena: Box<Arena>,
    holder: *mut Object,
    blocks: usize,
}

unsafe fn chain(name: &str, survivors: usize, per_block: usize) -> Chain {
    let link_cls = ClassBuilder::new(&format!("{name}Link"))
        .prop("next", true)
        .build();
    let holder_cls = ClassBuilder::new(&format!("{name}Root"))
        .prop("first", true)
        .build();

    let mut arena = Box::new(Arena::new());
    let arena_ptr: *mut Arena = &mut *arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let mut blocks = 1;
    let mut previous: *mut Object = std::ptr::null_mut();
    for index in 0..survivors {
        let link =
            unsafe { new_constructed(&mut *context_ptr, link_cls, MemoryCategory::RequestArena) };
        if previous.is_null() {
            // The one escape of the whole chain: every other link is reached
            // by the descent, which is the shape a request leaves behind.
            unsafe { store_prop(arena_ptr, holder, 16, link) };
        } else {
            unsafe { store_prop(arena_ptr, previous, 16, link) };
        }

        previous = link;
        if index % per_block == per_block - 1 && index + 1 < survivors {
            let room = unsafe { (*arena_ptr).room_left() };
            assert!(!unsafe { (*arena_ptr).alloc(room) }.is_null());
            blocks += 1;
        }
    }

    Chain {
        arena,
        holder,
        blocks,
    }
}

/// Nanoseconds one reset of `survivors` survivors over blocks of
/// `per_block` takes, as (minimum, median) over `rounds` fresh arenas, and
/// the block count the shape produced.
unsafe fn time_reset(
    name: &str,
    survivors: usize,
    per_block: usize,
    rounds: usize,
) -> (u128, u128, usize) {
    let mut blocks = 0;
    let (minimum, median) = min_and_median_nanos(rounds, |round| {
        let mut shape =
            unsafe { chain(&format!("{name}{round}"), survivors, black_box(per_block)) };
        blocks = shape.blocks;
        let arena_ptr: *mut Arena = &mut *shape.arena;
        let started = Instant::now();
        unsafe { arena_reset_full(black_box(arena_ptr)) };
        let taken = started.elapsed().as_nanos();

        // The promoted survivors are the holder's, and the holder's death
        // is what returns every block the reset retained: a round that kept
        // them would charge the next round's reset with its blocks.
        unsafe {
            assert!(crate::refcount::ll_release(shape.holder as *mut RcHeader));
            ll_object_die(shape.holder);
        }

        taken
    });
    (minimum, median, blocks)
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_the_grouping() {
    let _g = crate::memory::block_pool::test_guard();
    let rounds = 15;
    let survivors = 2_400;

    for per_block in [120, 1_200] {
        let (minimum, median, blocks) = unsafe {
            time_reset(
                &format!("Grouping{per_block}"),
                survivors,
                per_block,
                rounds,
            )
        };
        println!(
            "{survivors} survivors over {blocks} blocks ({per_block} per block): \
             minimum {minimum} ns, median {median} ns, \
             {:.1} ns per survivor at the minimum",
            minimum as f64 / survivors as f64
        );
    }
}
