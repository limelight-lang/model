//! An entity block's slots are read as headers by a walker that owns
//! none of them, so the free-list link is written at bytes 8-15 and
//! commissioning zeroes every slot's first word, whatever the block
//! held before. The kind goes through one published store the
//! collector's region scan may read concurrently, and a raw heap
//! never adopts an entity block, or C buffers would land where the
//! walker reads headers.

use super::*;

/// The free-list link lives in slot bytes 8–15, so a free leaves the
/// first 8 bytes exactly as the dying occupant left them — in an
/// entity block that is the final refcount-0 header, which is how a
/// trace tells a free slot from a live entity. Fails with the link at
/// bytes 0–7, where the push clobbered them.
#[test]
fn free_leaves_the_slots_first_8_bytes_untouched() {
    let _g = crate::memory::block_pool::test_guard();
    let mut heap = Heap::new();
    let p = heap.alloc(64);
    unsafe { std::ptr::write_bytes(p, 0xAA, 64) };
    unsafe { heap.free(p) };
    for i in 0..8 {
        assert_eq!(
            unsafe { *p.add(i) },
            0xAA,
            "byte {i} of a freed slot was clobbered by the free-list link"
        );
    }

    let again = heap.alloc(64);
    assert_eq!(again, p, "the slot still threads the free list");
    unsafe { heap.free(again) };
}

/// Commissioning an entity block zeroes every slot's first 8 bytes,
/// whatever the block held before — the commissioning rule that closes
/// the carve-to-first-store window. The raw population deliberately skips
/// the pass, which is also the control proving the scribble survives
/// the pool round-trip (i.e. that this test can fail).
#[test]
fn entity_commissioning_zeroes_slot_headers_of_a_recycled_block() {
    let _g = crate::memory::block_pool::test_guard();
    let pool = BlockPool::global();
    let class = 3072usize; // uncommon class: no adoptable leftovers
    let slots = BLOCK_PAYLOAD / class;

    let scribble = |block: *mut BlockHeader| unsafe {
        std::ptr::write_bytes(BlockHeader::payload_start(block), 0xEE, BLOCK_PAYLOAD);
    };

    let slot_header = |p: *mut u8, s: usize| unsafe {
        ((p as usize & !BLOCK_MASK) as *mut u8)
            .add(LINE_SIZE + s * class)
            .cast::<u64>()
            .read()
    };

    // Control: the raw population keeps the garbage (thread-cache
    // LIFO hands the scribbled block straight back to refill).
    let block = pool.get();
    scribble(block);
    pool.put(block);
    let mut raw = Heap::new();
    let p = raw.alloc(class);
    assert_eq!(p as usize & !BLOCK_MASK, block as usize, "LIFO reuse");
    assert_eq!(
        slot_header(p, 1),
        0xEEEE_EEEE_EEEE_EEEE,
        "control: without the pass the scribble survives commissioning"
    );
    unsafe { raw.free(p) };
    drop(raw);

    // The entity population must not: every slot header reads 0.
    let block = pool.get();
    scribble(block);
    pool.put(block);
    let mut entity = Heap::new_entity();
    let p = entity.alloc(class);
    assert_eq!(p as usize & !BLOCK_MASK, block as usize, "LIFO reuse");
    for s in 1..slots {
        assert_eq!(
            slot_header(p, s),
            0,
            "slot {s} header must be zeroed at entity commissioning"
        );
    }

    unsafe { entity.free(p) };
}

/// A block never crosses populations through abandonment: a raw heap
/// must not adopt an entity block, or C buffers land where the walker
/// reads headers.
#[test]
fn adoption_never_crosses_block_populations() {
    let _g = crate::memory::block_pool::test_guard();
    let class = 3072usize; // uncommon class: adoption is deterministic

    let mut donor = Heap::new_entity();
    let held = donor.alloc(class);
    assert!(!held.is_null());
    let entity_block = held as usize & !BLOCK_MASK;
    donor.abandon_all(); // still holds `held`: goes to the abandoned list

    // A raw heap of the same class must refill fresh, not adopt it.
    let mut raw = Heap::new();
    let p = raw.alloc(class);
    assert_ne!(
        p as usize & !BLOCK_MASK,
        entity_block,
        "a raw heap must never serve from an entity block"
    );
    unsafe {
        assert_eq!(
            (*HeapBlockHeader::of_ptr(p)).kind.load(Ordering::Relaxed),
            BLOCK_KIND_HEAP
        );
    }

    unsafe { raw.free(p) };

    // An entity heap is the legitimate adopter; freeing both slots
    // sends the block home instead of leaving it stranded abandoned.
    let mut adopter = Heap::new_entity();
    let q = adopter.alloc(class);
    assert_eq!(
        q as usize & !BLOCK_MASK,
        entity_block,
        "the entity heap adopts the abandoned entity block"
    );
    unsafe {
        adopter.free(q);
        adopter.free(held);
    }
}

/// End to end over the C ABI: the factory's population is disjoint
/// from `ll_malloc`'s, and `ll_free` routes each kind to its heap.
#[test]
fn entity_and_raw_allocations_are_segregated_end_to_end() {
    let _g = crate::memory::block_pool::test_guard();
    let e = unsafe { entity_alloc(40) };
    let r = unsafe { crate::memory::stdapi::ll_malloc(40) };
    assert!(!e.is_null() && !r.is_null());
    unsafe {
        assert_eq!(
            (*HeapBlockHeader::of_ptr(e)).kind.load(Ordering::Relaxed),
            BLOCK_KIND_ENTITY
        );
        assert_eq!(
            (*HeapBlockHeader::of_ptr(r)).kind.load(Ordering::Relaxed),
            BLOCK_KIND_HEAP
        );
    }

    assert_ne!(
        e as usize & !BLOCK_MASK,
        r as usize & !BLOCK_MASK,
        "the two populations never share a block"
    );
    // Both go home through the one size-less free, the entity slot through the
    // unpublished form of it: nothing was published into it, so nothing took
    // the mark of the free that may have put it on the free list
    // (`memory::stdapi::free_unpublished`).
    unsafe {
        crate::memory::stdapi::free_unpublished(e);
        crate::memory::stdapi::ll_free(r);
    }
}
