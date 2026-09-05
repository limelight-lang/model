//! The registry is a list threaded through the runs, so its correctness
//! is two things a test can see: the header layout the links sit in, and
//! what the list holds after a run is taken out of the middle, the head
//! and the tail of it.

use super::*;
use std::mem::{offset_of, size_of};

/// The header's field offsets, pinned to the layout the design names.
/// Every accessor reaches its field by name, so no offset here is
/// load-bearing on its own and the pin exists to make a change to the
/// layout deliberate rather than incidental. The one constraint that is
/// load-bearing is the last assertion: the entity starts at `+LINE_SIZE`
/// and the header may not reach it.
#[test]
fn the_header_the_registry_threads_itself_through() {
    assert_eq!(offset_of!(LargeEntityHeader, kind), 0);
    assert_eq!(offset_of!(LargeEntityHeader, _pad), 4);
    assert_eq!(offset_of!(LargeEntityHeader, size), 8);
    assert_eq!(offset_of!(LargeEntityHeader, run_bytes), 16);
    assert_eq!(offset_of!(LargeEntityHeader, prev), 24);
    assert_eq!(offset_of!(LargeEntityHeader, next), 32);
    assert_eq!(offset_of!(LargeEntityHeader, row), 40);
    assert_eq!(offset_of!(LargeEntityHeader, marked_next), 48);
    assert_eq!(size_of::<LargeEntityHeader>(), 56);
    assert!(
        size_of::<LargeEntityHeader>() <= LINE_SIZE,
        "the entity starts at +LINE_SIZE and the header may not reach it"
    );
}

/// The runs this test registered, sorted. A run leaked by an earlier
/// test is residue in a process-global list and not this test's subject,
/// so it is subtracted rather than asserted about.
fn ours(before: &[usize]) -> Vec<usize> {
    let mut live: Vec<usize> = snapshot()
        .into_iter()
        .filter(|addr| !before.contains(addr))
        .collect();
    live.sort_unstable();
    live
}

fn sorted(blocks: &[*mut u8]) -> Vec<usize> {
    let mut addresses: Vec<usize> = blocks.iter().map(|&b| b as usize).collect();
    addresses.sort_unstable();
    addresses
}

fn run() -> *mut u8 {
    let entity = alloc(BLOCK_PAYLOAD + 1);
    assert!(!entity.is_null(), "the system served the mapping");
    (entity as usize & !BLOCK_MASK) as *mut u8
}

/// Three runs, freed from the middle, then the head, then what is left:
/// the three positions the unlink arms distinguish. Registration puts
/// the newest at the head, so `c` is the head and `a` is the tail.
///
/// Dropping any one of the three writes an unlink makes turns this test
/// red, and as a fault rather than as a mismatch: a link left standing
/// names a mapping the operating system has taken back, and the next walk
/// reads it. The membership assertions carry the other direction — a run
/// that never entered the list, or one that left it while its neighbours
/// still named it.
#[test]
fn a_run_freed_from_the_middle_the_head_or_the_tail_leaves_the_rest_linked() {
    let _g = test_guard();
    let before = snapshot();

    let a = run();
    let b = run();
    let c = run();
    assert_eq!(ours(&before), sorted(&[a, b, c]), "all three registered");

    unsafe { free(b, BLOCK_KIND_ENTITY_LARGE_RUN) };
    assert_eq!(
        ours(&before),
        sorted(&[a, c]),
        "the middle went and joined its neighbours to each other"
    );

    unsafe { free(c, BLOCK_KIND_ENTITY_LARGE_RUN) };
    assert_eq!(
        ours(&before),
        sorted(&[a]),
        "the head went and the list starts at what followed it"
    );

    unsafe { free(a, BLOCK_KIND_ENTITY_LARGE_RUN) };
    assert_eq!(
        ours(&before),
        Vec::new(),
        "and the last of them leaves none of the three behind"
    );
}
