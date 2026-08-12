//! What the collector reads while the mutator rearranges the very
//! chunk it is striding.
//!
//! **`cargo test` cannot judge this either** (`array::head`'s group
//! says the same of the head's placement): the walker's loads are
//! relaxed atomics, the mutator's writes are ordinary, and a run
//! reports nothing whichever way the entries are moved. What decides
//! it is Miri's data-race detector, so the test below is the regression
//! for the in-place slide and its verdict is read from a Miri run rather
//! than from the suite.
//!
//! Gated to `rc-walk`, and on the group rather than on the test: both
//! instruments it needs are that collector's — the relaxed reader and the
//! epoch whose flag parks a freed chunk. rc-trace walks nothing
//! concurrently, so the arrangement cannot be built there at all.

use super::*;

/// A relaxed reader striding the entries while the owner compacts.
///
/// The epoch flag is raised for the duration, which is what makes
/// the test faithful rather than lucky: a compaction that replaces
/// the chunk frees the old one, and only a live epoch parks that
/// free instead of recycling the bytes the walker is still reading
/// (`memory::deferred_free`). The collector never walks outside an
/// epoch, so the production invariant is the same one.
///
/// Nothing is asserted about what the walker sees. A stale reading
/// is a missed edge and later phases repair it; what must not
/// happen is a read of bytes the mutator is writing plainly, and
/// that is not a value any assertion can name.
#[test]
fn a_relaxed_reader_strides_the_entries_while_the_owner_compacts() {
    const ENTRIES: i64 = 24;
    const READINGS: usize = 96;
    let _g = crate::memory::block_pool::test_guard();
    crate::memory::deferred_free::begin_epoch();

    let mut m = t();
    for i in 0..ENTRIES {
        m.insert(Key::Int(i), Value::int(i));
    }

    let handed = Handed(m.0);
    let walker = std::thread::spawn(move || {
        let handed = handed;
        let mut cells = 0usize;
        for _ in 0..READINGS {
            unsafe {
                crate::walk::trace_cells::<crate::walk::RelaxedCells>(
                    handed.0 as *mut crate::refcount::RcHeader,
                    crate::refcount::EntityKind::Array as u32,
                    |_| cells += 1,
                )
            };
            std::thread::yield_now();
        }

        cells
    });

    // Holes, then a compaction to reclaim them, three times over:
    // one compaction is one window, and the walker has to be inside
    // a stride when a window opens for this to prove anything.
    for round in 0..3i64 {
        for i in 0..ENTRIES {
            if i % 2 == round % 2 {
                let _ = m.remove(Key::Int(i));
            }
        }

        assert!(m.compact().is_some(), "the compaction was refused");
        std::thread::yield_now();
        for i in 0..ENTRIES {
            if i % 2 == round % 2 {
                m.insert(Key::Int(i), Value::int(i));
            }
        }
    }

    let cells = walker.join().unwrap();
    assert_eq!(cells, 0, "every element here is an integer, so no cell");
    drop(m);
    crate::memory::deferred_free::end_epoch();
    assert!(unsafe { crate::memory::deferred_free::flush() } > 0);
}
