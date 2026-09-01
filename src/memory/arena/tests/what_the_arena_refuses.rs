//! An allocation refuses with null and an in-place extension with
//! `false`, and either leaves the arena serving. The rounding
//! saturates instead of wrapping, so a size no block can hold fails
//! the bound rather than becoming a small one.

use super::*;

/// Exhaustion is reported, not fatal. Every pooled path used to abort
/// on it — `carve_region`, `alloc_large`, the buffer arena — while the
/// huge-allocation path returned null, so a C caller got null for
/// 200 KB and a dead process for 40 bytes.
///
/// Revert any of those to `assert!` and this test kills the process
/// rather than failing.
#[test]
fn exhaustion_reports_null_and_leaves_the_arena_usable() {
    let _g = crate::memory::block_pool::test_guard();
    use crate::memory::block_pool::force_oom;

    let mut arena = Arena::new();

    let oom = force_oom();
    let p = arena.alloc(40);
    drop(oom);

    assert!(p.is_null(), "exhaustion must report, not abort");

    // Still usable once memory is available again: the refusal left
    // no half-rotated state behind.
    let q = arena.alloc(40);
    assert!(!q.is_null(), "the arena survived the refusal");
    arena.reset(|_| {});
}

/// A size no block can hold is refused, and the refusal leaves the
/// arena serving: the rounding saturates instead of wrapping, so the
/// request stays huge and fails the bound rather than becoming a
/// small one. A refusal rather than an abort, because the size
/// arrives through `ll_arena_alloc` and a program can name it.
#[test]
fn absurd_size_is_refused_instead_of_wrapping() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    arena.alloc(8); // non-null bump: the fast path is reachable
    assert!(arena.alloc(usize::MAX - 64).is_null());
    assert!(arena.alloc(BLOCK_PAYLOAD + 1).is_null());
    assert!(
        !arena.alloc(8).is_null(),
        "a refusal left the arena unable to serve"
    );
}

#[test]
fn extend_refuses_absurd_size_instead_of_wrapping() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let buf = arena.alloc(64);
    assert!(!arena.try_extend_in_place(buf, 64, usize::MAX - 64));
    assert!(
        arena.try_extend_in_place(buf, 64, 128),
        "sane size still extends"
    );
}
