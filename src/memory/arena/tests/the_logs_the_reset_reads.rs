//! The destructor log is segmented, so a record may not be lost at a
//! segment boundary — which is what the two growth tests walk past, one
//! per log, because a chain is drained by a loop each log enters on its
//! own. The barrier's log grows from the thread reserve when the
//! pool refuses, because the barrier has no way to report a failure.
//! The reset drains three more logs beside these two: the
//! release-at-reset log, the weak log, and the large runs.

use super::*;

/// The barrier has no way to report a failure, so its log growth must
/// not have one. When the pool refuses, the segment comes from the
/// thread's reserve — and the escape record still lands, which is the
/// whole point: a lost escapee dangles at reset.
///
/// The reserve block must not become the arena's bump block either.
/// If it did, ordinary allocation would spend the memory that exists
/// so the barrier cannot fail, and the null that lets a frame raise
/// would never be returned.
#[test]
fn the_barrier_log_grows_from_the_reserve_when_the_pool_refuses() {
    let _g = crate::memory::block_pool::test_guard();
    use crate::memory::block_pool::FORCE_OOM;
    use std::sync::atomic::Ordering;

    crate::memory::reserve::drain_for_test();
    assert!(crate::memory::reserve::replenish());

    let mut arena = Arena::new();
    let mut entity = RcHeader::new(MemoryCategory::RequestArena, 0);

    FORCE_OOM.store(true, Ordering::Relaxed);
    assert!(
        arena.alloc(16).is_null(),
        "ordinary allocation reports the exhaustion"
    );
    // Records an escapee: this is the path with no channel at all.
    arena.log_escapee(&mut entity);
    assert!(
        arena.alloc(16).is_null(),
        "and still reports it — the reserve is not the arena's bump block"
    );
    FORCE_OOM.store(false, Ordering::Relaxed);

    assert!(
        crate::memory::reserve::is_drawn(),
        "the draw asks the next safepoint for a refill"
    );
    assert_eq!(unsafe { crate::gc::ll_gc_maybe_collect() }, 0);
    assert!(
        !crate::memory::reserve::is_drawn(),
        "which the safepoint answers"
    );

    let mut seen = 0;
    arena.reset_with(|_| {}, |_| seen += 1);
    assert_eq!(seen, 1, "the escapee record survived the exhaustion");
    crate::memory::reserve::drain_for_test();
}

#[test]
fn destructor_log_survives_segment_growth() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();

    // Three segments' worth of tracked objects.
    let n = LOG_SEG_RECORDS * 2 + 137;
    let objs: Vec<*mut RcHeader> = (0..n)
        .map(|_| {
            let obj = arena.alloc(16) as *mut RcHeader;
            unsafe { obj.write(RcHeader::new(MemoryCategory::RequestArena, 0)) };
            assert!(arena.track_destructor(obj));
            obj
        })
        .collect();

    let mut ran = Vec::new();
    arena.reset(|o| ran.push(o));

    assert_eq!(ran.len(), n, "every tracked destructor must be delivered");
    let expected: std::collections::HashSet<_> = objs.iter().map(|p| *p as usize).collect();
    let got: std::collections::HashSet<_> = ran.iter().map(|p| *p as usize).collect();
    assert_eq!(got, expected, "same set of objects, order unspecified");
}

/// The same boundary on the escape log, which the destructor test
/// cannot stand in for: each log heads its own chain and is drained
/// by its own call, so a link dropped in one is invisible in the
/// other. A lost escapee is the worst of the five to lose — reset
/// decides promote-or-drop from the record, and a record that never
/// arrives leaves the entity's external holder pointing into reused
/// bump memory.
#[test]
fn escape_log_survives_segment_growth() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();

    // Three segments' worth, the count taken from the segment size
    // rather than spelled out, and short of a round multiple so the
    // last segment is partly filled.
    let n = LOG_SEG_RECORDS * 2 + 137;
    let escapees: Vec<*mut RcHeader> = (0..n)
        .map(|_| {
            let entity = arena.alloc(16) as *mut RcHeader;
            unsafe { entity.write(RcHeader::new(MemoryCategory::RequestArena, 0)) };
            arena.log_escapee(entity);
            entity
        })
        .collect();

    // Where the segments end, which no count of delivered records can
    // say: a push that grew one record too late writes past its
    // segment's array and reads the same value straight back, so all
    // of them still arrive, exactly once each, over a clobbered
    // neighbour.
    let mut counts = Vec::new();
    let mut seg = arena.escapees;
    while !seg.is_null() {
        unsafe {
            counts.push((*seg).count);
            seg = (*seg).next;
        }
    }

    assert_eq!(
        counts,
        vec![n % LOG_SEG_RECORDS, LOG_SEG_RECORDS, LOG_SEG_RECORDS],
        "newest segment first, and none holds more than it has room for"
    );

    let mut seen = Vec::new();
    arena.reset_with(|_| {}, |e| seen.push(e));

    assert_eq!(seen.len(), n, "every escapee record must reach the reset");
    let expected: std::collections::HashSet<_> = escapees.iter().map(|p| *p as usize).collect();
    let got: std::collections::HashSet<_> = seen.iter().map(|p| *p as usize).collect();
    assert_eq!(got, expected, "same set of entities, order unspecified");
}

#[test]
fn reset_hands_destructors_and_recycles_blocks() {
    let _g = crate::memory::block_pool::test_guard();
    let pool = BlockPool::global();
    let mut arena = Arena::new();

    let obj = arena.alloc(16) as *mut RcHeader;
    unsafe { obj.write(RcHeader::new(MemoryCategory::RequestArena, 0)) };
    assert!(arena.track_destructor(obj));
    let old_block = BlockHeader::of_ptr(obj as *mut u8);

    let mut ran = Vec::new();
    arena.reset(|o| ran.push(o));
    assert_eq!(ran, vec![obj], "destructor list must reach the caller");

    let regions_before = pool.regions_carved();
    let mut second = Arena::new();
    let p = second.alloc(8);
    assert_eq!(
        BlockHeader::of_ptr(p),
        old_block,
        "next arena must reuse the recycled block"
    );
    assert_eq!(pool.regions_carved(), regions_before);
}
