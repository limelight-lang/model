//! The pair of records a reset writes, read back from the journal: the
//! third operand of `KIND_ARENA_RESET_END` is how many blocks the reset
//! took out of circulation, and nothing else in the crate asks for that
//! number.
//!
//! The sites exist only in the `debug-journal` build, which is why the
//! group carries that `cfg`. What it pins is the counter S47.3 put in
//! place of a set's length: a counter incremented at the wrong arm reads
//! the same as one incremented at none until somebody asks the ring.

use super::*;
use crate::journal::kinds;
use crate::journal::{Event, Window, between, mark};

/// Every event the answers carry, whichever ring it came from.
fn events(windows: Vec<Window>) -> Vec<Event> {
    windows
        .into_iter()
        .flat_map(|window| match window {
            Window::Records(records) => records,
            _ => Vec::new(),
        })
        .collect()
}

/// One escapee, one block: the record says one survivor and one block
/// retained. The arena is fresh and the object small, so the survivor and
/// the list it gets both sit in the arena's first block, and a second
/// retention would mean the reset counted a block twice.
#[test]
fn a_reset_records_the_survivors_and_the_blocks_it_kept() {
    let _sites = kinds::set_sites_for_test(kinds::DEFAULT_KINDS);
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("RecordedSurvivor")
        .prop("x", true)
        .build();
    let holder_cls = ClassBuilder::new("RecordedCache")
        .prop("last", true)
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;

    let holder = unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let obj = unsafe { new_constructed(&mut *context_ptr, cls, MemoryCategory::RequestArena) };
    unsafe { store_prop(arena_ptr, holder, 16, obj) };

    let start = mark();
    unsafe { arena_reset_full(&mut *arena_ptr) };
    let end = mark();

    let closing: Vec<Event> = events(between(&start, &end))
        .into_iter()
        .filter(|event| {
            event.kind == kinds::KIND_ARENA_RESET_END && event.subject == arena_ptr as u64
        })
        .collect();

    assert_eq!(closing.len(), 1, "the reset closed once");
    assert_eq!(
        closing[0].a, 1,
        "one survivor was promoted out of the arena"
    );
    assert_eq!(closing[0].b, 1, "one block was taken out of circulation");

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}
