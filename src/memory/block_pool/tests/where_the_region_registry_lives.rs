//! The region registry holds every carved base in carve order across
//! more than one chunk, and its chunks come from the operating system
//! rather than from a `Vec` that would abort when it could not grow.

use super::*;

/// Carve order is what makes a registry index a stable handle, and the
/// chain has to preserve it across a chunk boundary — the one place an
/// append can reverse it, because a new chunk is linked at the tail
/// while iteration starts at the head.
///
/// The bases here are fabricated: the registry stores whatever it is
/// given and never dereferences it, so crossing the boundary needs
/// `REGISTRY_CHUNK_ENTRIES + 1` pushes rather than 16 GB of regions.
#[test]
fn the_chain_keeps_carve_order_across_a_chunk_boundary() {
    // The guard, though this test allocates nothing from the pool: the
    // map refusal the tests below arm is one process-wide switch, and a
    // sibling's arming would otherwise land on this test's `grow`.
    let _g = test_guard();
    let pool = a_pool_with_nothing_in_it();

    // The two chunks this maps are never unmapped: the registry has no
    // destructor, because in the pool it lives as long as the process.
    let pushes = REGISTRY_CHUNK_ENTRIES + 1;
    for i in 0..pushes {
        assert!(
            pool.register_region(base_for(i)),
            "the operating system refused a registry chunk at push {i}"
        );
    }

    let mut bases = Vec::new();
    pool.for_each_region(|base| bases.push(base as usize));

    assert_eq!(bases.len(), pushes, "every base is recorded");
    for (i, &base) in bases.iter().enumerate() {
        assert_eq!(base, base_for(i), "base {i} is out of carve order");
    }
}

/// A second chunk is mapped only once the first is full: the chunk is a
/// whole block, so mapping one per region would cost as much as the
/// regions do.
#[test]
fn one_chunk_serves_until_it_is_full() {
    let _g = test_guard();
    let pool = a_pool_with_nothing_in_it();

    for i in 0..REGISTRY_CHUNK_ENTRIES {
        assert!(pool.register_region(base_for(i)));
    }

    let head = pool.registry_head.load(Ordering::Relaxed);
    assert_eq!(
        head,
        pool.registry_tail.lock().unwrap().0,
        "one chunk holds a full load"
    );

    assert!(pool.register_region(base_for(REGISTRY_CHUNK_ENTRIES)));
    assert_ne!(
        head,
        pool.registry_tail.lock().unwrap().0,
        "the overflow maps a second"
    );
}

/// A recognisable, non-zero, block-aligned stand-in for a region base.
fn base_for(i: usize) -> usize {
    (i + 1) * BLOCK_SIZE
}

/// A pool of its own, so a refusal can be forced without the global
/// pool's free list answering the carve before the mapping is reached.
fn a_pool_with_nothing_in_it() -> BlockPool {
    BlockPool {
        free: Mutex::new(FreeList {
            head: std::ptr::null_mut(),
        }),
        regions_carved: AtomicUsize::new(0),
        blocks_out: AtomicUsize::new(0),
        registry_head: AtomicPtr::new(std::ptr::null_mut()),
        registry_tail: Mutex::new(RegistryTail(std::ptr::null_mut())),
    }
}

/// The refusal `carve_region` was built to report: the operating system
/// says no to the region, and the answer is false rather than a dead
/// process. This is the branch the old `std::alloc` path could not take
/// on the way to `handle_alloc_error`.
#[test]
fn a_refused_region_is_reported() {
    let _g = test_guard();
    let pool = a_pool_with_nothing_in_it();

    let _refusing = crate::memory::os::fault::Refusing::after(0);
    let carved = pool.carve_region();

    assert!(!carved, "a refused region must be reported, not aborted");
    assert_eq!(pool.regions_carved(), 0, "nothing was carved");
}

/// The region is mapped and the registry's chunk is not. The region goes
/// back rather than being handed out: blocks the census cannot map to a
/// region are invisible to it, which is worse than reporting exhaustion
/// one region early.
#[test]
fn a_region_whose_chunk_is_refused_goes_back() {
    let _g = test_guard();
    let pool = a_pool_with_nothing_in_it();

    // One grant for the region, then the refusal lands on the registry's
    // first chunk — the only other mapping this path makes.
    let _refusing = crate::memory::os::fault::Refusing::after(1);
    let carved = pool.carve_region();

    assert!(!carved, "a region the registry cannot record is refused");
    assert_eq!(pool.regions_carved(), 0);
    assert!(
        pool.free.lock().unwrap().head.is_null(),
        "no block of an unrecorded region may reach the free list"
    );
}

/// The registry reports a refused chunk and stays exactly as it was, so
/// the caller's own undo has something consistent to return to.
#[test]
fn a_refused_chunk_leaves_the_registry_untouched() {
    let _g = test_guard();
    let pool = a_pool_with_nothing_in_it();

    let _refusing = crate::memory::os::fault::Refusing::after(0);
    let pushed = pool.register_region(base_for(0));

    assert!(!pushed, "a refused chunk is reported");
    assert!(
        pool.registry_head.load(Ordering::Relaxed).is_null(),
        "no chunk was linked"
    );
    let mut recorded = 0;
    pool.for_each_region(|_| recorded += 1);
    assert_eq!(recorded, 0, "no base was recorded");
}

/// The refusal reaching `get`, which is where the contract lives: a
/// caller reads null and the optimistic `blocks_out` bump is undone.
///
/// `FORCE_OOM` cannot exercise this — it short-circuits at the top of
/// `take_block`, before the refill loop this branch belongs to.
#[test]
fn a_refused_carve_reaches_get_as_null() {
    let _g = test_guard();
    drain_thread_cache();

    let pool = a_pool_with_nothing_in_it();
    let out_before = pool.blocks_out();

    let _refusing = crate::memory::os::fault::Refusing::after(0);
    let block = pool.get();

    assert!(
        block.is_null(),
        "an empty pool that cannot carve answers null"
    );
    assert_eq!(
        pool.blocks_out(),
        out_before,
        "the optimistic count is undone on the refusal"
    );
}

/// A carve refused **after** the free list gave what it had: the caller
/// still gets a block, and the rest of the short batch reaches the thread
/// cache rather than being discarded.
#[test]
fn a_short_batch_keeps_every_block_it_got() {
    let _g = test_guard();
    drain_thread_cache();

    let pool = a_pool_with_nothing_in_it();
    assert!(pool.carve_region(), "the fixture needs one region");

    // Two blocks left on the free list: one for the caller, one spare,
    // and a refused carve where the batch would have asked for more.
    let mut held = Vec::new();
    while let Some(block) = pool.pop_global() {
        held.push(block);
    }

    for block in held.drain(..2) {
        pool.push_global(block);
    }

    let block = {
        let _refusing = crate::memory::os::fault::Refusing::after(0);
        pool.get()
    };

    assert!(!block.is_null(), "the free list had a block to give");
    assert_eq!(
        thread_cache_len(),
        1,
        "the batch's remainder belongs in the cache, not on the floor"
    );

    // The cached spare is this pool's, out of a region the global pool
    // does not know: take it back before another test's exit hands it
    // there.
    while thread_cache_len() > 0 {
        let _ = pool.get();
    }
}
