//! The poll traces once the thread's slot bytes pass the threshold, and not
//! before: a garbage ring under the minimum stands through the poll, and one
//! past it is freed by the poll's trace.

use super::*;
use crate::gc::ll_gc_maybe_collect;

#[test]
fn the_poll_traces_once_the_bytes_pass_the_minimum() {
    let _g = test_guard();
    let mut arena = Arena::new();
    let class = node_class("TbPolledRing", std::ptr::null());

    let small = unsafe { ring(&mut arena, [class; 2]) };
    assert!(
        crate::memory::heap::entity_bytes_in_owned_blocks() < threshold::MINIMUM_BYTES,
        "the case starts under the minimum"
    );
    let _ = unsafe { ll_gc_maybe_collect() };
    assert!(
        small.iter().all(|&member| live(member)),
        "a poll under the minimum traces nothing"
    );

    // A ring whose slots alone pass the minimum.
    let size = unsafe { (*class).object_size } as usize;
    let members = threshold::MINIMUM_BYTES / size + 1;
    let large = unsafe { crate::cycle::testing::long_ring(&mut arena, class, members) };
    let freed = unsafe { ll_gc_maybe_collect() };
    assert!(
        freed >= members + small.len(),
        "the poll's trace freed both rings: {freed}"
    );
    assert!(!live(large[0]) && !live(small[0]));
}
