//! Measurement probe, not a correctness test: what settling the COW
//! survivors' counts costs on the clock, against the `Vec` of captures and
//! the `HashMap<usize, i64>` it replaced (`dev/BENCHMARKS.md`, "S47.9 the
//! COW reconciliation is three linear passes").
//!
//! The shape is the one the reconciliation is sensitive to: how many COW
//! survivors a reset promotes. Each is one capture, one edge and one
//! correction of the window's log, and the three passes walk the log once
//! each.
//!
//! The figure is the whole reset, because the reconciliation has no entry
//! point of its own, and the arm that leaves has no benchmark: none drives
//! `arena_reset_full` (`what_the_grouping_costs`).
//!
//! **The two sizes differ by more than the survivor count.** At 2400
//! strings the reset also carries 2400 payloads out of the arena and fills
//! more blocks, so the difference between the rows is not the
//! reconciliation's own slope; what each row bounds is the direction, and
//! each compares like with like across the change.
//!
//! Run it in release, explicitly:
//! `cargo test --release --lib what_the_cow_reconciliation_costs -- --ignored --nocapture`.

use super::*;
use std::hint::black_box;
use std::time::Instant;

/// One reset's worth of COW survivors: an arena array holding `strings`
/// arena strings, the array held by an object the reset promotes.
///
/// One raw pointer per arena and per context, reused, as every fixture here
/// does (`dev/WORKFLOW.md`, Miri).
struct CowChain {
    arena: Box<Arena>,
    /// Boxed and kept beside the arena because the reset resolves it
    /// through the current context the builder mounted: a context in the
    /// builder's own frame would be gone by then. Read by nothing else.
    #[allow(dead_code)]
    context: Box<LLContext>,
    holder: *mut Object,
}

unsafe fn cow_chain(name: &str, strings: usize) -> CowChain {
    let root_cls = ClassBuilder::new(&format!("{name}Root"))
        .prop("items", true)
        .build();
    let holder_cls = ClassBuilder::new(&format!("{name}Cache"))
        .prop("kept", true)
        .build();

    let mut arena = Box::new(Arena::new());
    let arena_ptr: *mut Arena = &mut *arena;
    let mut context = Box::new(LLContext { arena: arena_ptr });
    let context_ptr: *mut LLContext = &mut *context;
    set_current_context(context_ptr);

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let root =
        unsafe { new_constructed(&mut *context_ptr, root_cls, MemoryCategory::RequestArena) };
    let array = unsafe { crate::array::entity::ll_array_new(MemoryCategory::RequestArena) };

    unsafe {
        for index in 0..strings {
            let bytes = format!("{name}-{index}");
            let string = crate::string::ll_string_new(
                context_ptr,
                MemoryCategory::RequestArena,
                bytes.as_bytes(),
            );
            assert!(crate::array::testing::push(
                array,
                Value::entity(Tag::String, string as *mut RcHeader)
            ));
        }

        let slot = Object::prop_at(root, 16);
        assert!(crate::memory::barrier::ref_store(
            arena_ptr,
            root as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        // The one escape of the whole shape, which is what promotes the
        // array and every string in it.
        store_prop(arena_ptr, holder, 16, root);
    }

    CowChain {
        arena,
        context,
        holder,
    }
}

/// Nanoseconds one reset of a shape with `strings` COW survivors takes, as
/// (minimum, median) over `rounds` fresh arenas.
unsafe fn time_reset(name: &str, strings: usize, rounds: usize) -> (u128, u128) {
    min_and_median_nanos(rounds, |round| {
        // `cow_chain` mounts its context, which the reset resolves the
        // strings' payload carry through.
        let mut shape = unsafe { cow_chain(&format!("{name}{round}"), black_box(strings)) };
        let arena_ptr: *mut Arena = &mut *shape.arena;
        let started = Instant::now();
        unsafe { arena_reset_full(black_box(arena_ptr)) };
        let taken = started.elapsed().as_nanos();

        // The promoted survivors are the holder's, and the holder's death
        // is what returns every block the reset retained.
        unsafe {
            assert!(crate::refcount::ll_release(shape.holder as *mut RcHeader));
            ll_object_die(shape.holder);
        }

        taken
    })
}

#[test]
#[ignore = "measurement probe; run explicitly with --ignored (release mode)"]
fn measure_the_reconciliation() {
    let _g = crate::memory::block_pool::test_guard();
    for (strings, rounds) in [(120usize, 15usize), (2400, 15)] {
        let (minimum, median) = unsafe { time_reset("CowCost", strings, rounds) };
        println!(
            "{strings} COW survivors: minimum {:.1} µs, median {:.1} µs, over {rounds} rounds",
            minimum as f64 / 1000.0,
            median as f64 / 1000.0
        );
    }

    set_current_context(std::ptr::null_mut());
}
