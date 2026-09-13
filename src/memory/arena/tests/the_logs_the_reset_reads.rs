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
    use crate::memory::block_pool::force_oom;

    crate::memory::reserve::drain_for_test();
    assert!(crate::memory::reserve::replenish());

    let mut arena = Arena::new();
    let mut entity = RcHeader::new(MemoryCategory::RequestArena, 0);

    let oom = force_oom();
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
    drop(oom);

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

/// The chain a `take_*` hands over carries the log's only copy of its
/// records, and `finish_reset`'s "logs must be drained" assert cannot
/// report a chain nobody walked: it reads the arena's fields, which the
/// take has already nulled. The guard that does report it is
/// `DetachedLog`'s own drop.
#[test]
#[should_panic(expected = "a detached log was dropped before it was walked")]
fn a_detached_log_nobody_walks_is_reported() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    // Never dereferenced: the log stores the word and only a walk reads
    // it, which is the thing this test declines to do.
    arena.track_destructor(std::ptr::dangling_mut());
    drop(arena.take_destructors());
}

/// The survivor chain is the one log linked at the tail, because the reset
/// walks it by index and appends to it while it walks. Reversing that link
/// makes the walk read the newest segment first, which no count of
/// delivered records can see — the same records arrive, in the order that
/// breaks the index.
#[test]
fn the_survivor_chain_grows_at_its_tail() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();

    let n = LOG_SEG_RECORDS * 2 + 137;
    let survivors: Vec<*mut RcHeader> = (0..n)
        .map(|_| {
            let entity = arena.alloc(16) as *mut RcHeader;
            unsafe { entity.write(RcHeader::new(MemoryCategory::RequestArena, 0)) };
            assert!(arena.push_survivor(entity));
            entity
        })
        .collect();

    let mut counts = Vec::new();
    let mut seg = arena.survivors;
    while !seg.is_null() {
        unsafe {
            counts.push((*seg).count);
            seg = (*seg).next;
        }
    }

    assert_eq!(
        counts,
        vec![LOG_SEG_RECORDS, LOG_SEG_RECORDS, n % LOG_SEG_RECORDS],
        "oldest segment first, and none holds more than it has room for"
    );
    assert_eq!(arena.survivor_count(), n);

    let walked: Vec<*mut RcHeader> = arena.walk_survivors(0).collect();
    assert_eq!(walked, survivors, "in the order they were admitted");

    // A walk from inside the second segment starts at that record and not
    // at its segment's first, which is the arithmetic the index space
    // rests on.
    let from = LOG_SEG_RECORDS + 7;
    let tail: Vec<*mut RcHeader> = arena.walk_survivors(from).collect();
    assert_eq!(tail, survivors[from..], "the index names one record");
}

/// A walk resumes on an append behind it, which is what lets the descent
/// walk the chain and grow it at once — but only where it has a segment to
/// resume from. A walk made over an empty chain stays empty, and its caller
/// re-asks at its own index; holding the arena's head field instead was
/// undefined behaviour, the next append's `&mut Arena` retagging the
/// pointer (Miri, 2026-09-13).
#[test]
fn a_walk_resumes_on_an_append_behind_it() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();

    let entity = |arena: &mut Arena| {
        let e = arena.alloc(16) as *mut RcHeader;
        unsafe { e.write(RcHeader::new(MemoryCategory::RequestArena, 0)) };
        e
    };

    let empty = arena.walk_survivors(0);
    let first = entity(&mut arena);
    assert!(arena.push_survivor(first));
    assert_eq!(
        empty.count(),
        0,
        "a walk made over an empty chain has no segment to resume from"
    );
    assert_eq!(
        arena.walk_survivors(0).count(),
        1,
        "and the caller's re-ask finds the record"
    );

    let mut walk = arena.walk_survivors(0);
    assert_eq!(walk.next(), Some(first));
    let second = entity(&mut arena);
    assert!(arena.push_survivor(second));
    assert_eq!(walk.next(), Some(second), "the append reached the walk");
    assert!(walk.next().is_none());
}
