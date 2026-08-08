//! Object indexes for retained former-arena blocks.
//!
//! A retained block was filled by an arena's bump allocator, so its
//! occupants have mixed sizes and no uniform stride: the walk cannot
//! locate them by dividing an offset by a size class the way it does in
//! an entity block. Without an inventory they are unwalkable, which
//! makes them root sources and leaves a ring living entirely among
//! promoted survivors uncollectable
//! (`rfc/model/gc/retained-block-walk.md`).
//!
//! The inventory already exists. `promote`'s reset fixpoint collects
//! every survivor into a vector; this module keeps it, one sorted array
//! of addresses per retained block, and hands it to the two enumerators
//! that need it — `heap::for_each_entity_slot` for the synchronous walk
//! and `heap::snapshot_entity_blocks` for the collector's epoch.
//!
//! # What this module does not know
//!
//! What lives at those addresses. It stores block addresses and arrays
//! of addresses; entities, classes, refcounts and verdicts belong to
//! the layers above. It reads one word of one of them, in exactly two
//! places and for one purpose: the refcount word is the occupancy test,
//! and the live-occupant count below is what decides when a block has
//! emptied. Everything else about an address stays the readers'.
//!
//! # The one requirement on an address
//!
//! Every address in a registered index stays **readable** for as long as
//! the index is registered. Both enumerators read its refcount word
//! without first testing that the block still exists — they can, because
//! a retained block leaves circulation only once its last survivor is
//! gone — so an address that stops being mapped is a wild read on
//! whichever thread walks next, and both walks are process-global. The
//! index is therefore dropped **before** the block is handed to the
//! pool, never after.
//!
//! # Why the arrays never change under a reader
//!
//! Nothing allocates into a dead arena, so a retained block's
//! population only shrinks, and it shrinks by entities dying rather
//! than by slots being reissued: a retained block leaves circulation
//! only when all of its survivors are gone. An index therefore needs no
//! version, no growth path and no lock beyond the one guarding the map
//! itself; readers clone the `Arc` and walk it outside the lock.
//!
//! # Blocks retained for bytes rather than for occupants
//!
//! A survivor whose out-of-line payload could not be carried out of the
//! dying arena retains the block those bytes lie in (`promote`'s refused
//! carry). The block is then held by two populations at once, and it
//! goes home when both are empty: its live occupants, counted here since
//! the index was built, and the payloads it was pinned for, counted by
//! [`pin`] and spent by [`payload_freed`].
//!
//! The payload's death event is the owning entity's own free. A promoted
//! string or table frees its body through
//! `buffer_arena::buffer_free_longlived_payload`, which reads the block
//! kind under the pointer and finds `BLOCK_KIND_RETAINED`: the bytes are
//! left where they are, because former arena memory has no free list to
//! return them to, and the block is what is reclaimed instead.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// What is known about one retained block.
struct Index {
    /// Its occupants, sorted ascending.
    occupants: Arc<[usize]>,
    /// How many of them were alive when the index was registered, less
    /// one for each that has died since. At zero the block is empty and
    /// goes home.
    live: usize,
    /// How many payloads the block is held for beyond its occupants:
    /// bytes a reset could not carry out, each freed by the entity that
    /// owns it (module doc). At zero, with `live` at zero too, the block
    /// goes home.
    payloads: usize,
}

/// Block address (the block header, 64 KiB-aligned) → what is known
/// about it.
type Registry = BTreeMap<usize, Index>;

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record `occupants` as the object index of retained block `block`,
/// and count how many of them are alive.
///
/// `occupants` is sorted here rather than at the call site, because the
/// census's lookup is a binary search over it and the reset builds it
/// in discovery order. A block registered twice keeps the newer index
/// and stays pinned if it was: one reset produces one index per block it
/// retains, and the pin comes from the same reset a few lines earlier.
///
/// **An occupant already dead when the index is built is not counted.**
/// It has had its one death and will never reach [`occupant_freed`], so
/// counting it would hold the block for a survivor that no longer
/// exists — which is exactly the case a heap box behind `&` produces:
/// the element it made an escapee is promoted, and the box's logged
/// release kills it in the same reset, before the index exists.
///
/// **True when the block is empty already**, which is that same case
/// taken to its end: every occupant died inside the reset, so nobody is
/// left to report the last death and the caller must free the block
/// itself. The index is registered either way, because the free path
/// asks [`occupant_freed`] and that answers off the index.
///
/// # Safety
/// Every address in `occupants` must be readable, as the module doc
/// requires of anything registered here.
#[must_use = "true means the block is empty and the caller owes it to the pool"]
pub(crate) unsafe fn register(block: usize, mut occupants: Vec<usize>) -> bool {
    occupants.sort_unstable();
    let live = occupants
        .iter()
        .filter(|&&address| unsafe { is_occupied(address) })
        .count();
    let mut map = registry().lock().expect("retained index registry poisoned");
    let payloads = map.get(&block).map_or(0, |index| index.payloads);
    map.insert(
        block,
        Index {
            occupants: occupants.into(),
            live,
            payloads,
        },
    );
    live == 0 && payloads == 0
}

/// One more payload the block is held for: the reset could not carry
/// these bytes out, so they outlive the arena and the block waits for
/// them as it waits for a live occupant (module doc). Counted rather than
/// flagged, because one block can hold the payloads of several survivors
/// and each is freed on its own.
pub(crate) fn pin(block: usize) {
    let mut map = registry().lock().expect("retained index registry poisoned");
    map.entry(block)
        .or_insert_with(|| Index {
            occupants: Vec::new().into(),
            live: 0,
            payloads: 0,
        })
        .payloads += 1;
}

/// One occupant of retained `block` has been freed. **True** when nothing
/// holds the block afterwards, in which case the index is already gone
/// and the caller owes the block to the pool.
///
/// False for a block nobody registered, and false while a payload the
/// block was pinned for is still alive.
///
/// **A count already at zero answers true rather than underflowing**,
/// and that is a case rather than a mistake: every occupant of the block
/// had died before the index was built, so the reset asks this question
/// once on the block's own behalf ([`register`] told it to). Double-free
/// protection is not this counter's job — `ll_free` asserts on the
/// refcount word in test builds, one layer down.
#[must_use = "true means the block is empty and the caller owes it to the pool"]
pub(crate) fn occupant_freed(block: usize) -> bool {
    let mut map = registry().lock().expect("retained index registry poisoned");
    let Some(index) = map.get_mut(&block) else {
        return false;
    };
    index.live = index.live.saturating_sub(1);
    empty_now(&mut map, block)
}

/// One payload the block was pinned for has been freed — the death event
/// the bytes were said to lack, which is the owning entity's own free
/// reaching `buffer_arena::buffer_free_longlived_payload` and finding a
/// retained block under the pointer. **True** when nothing holds the
/// block afterwards, with the same duty on the caller as
/// [`occupant_freed`].
///
/// False for a block nobody pinned, which is the ordinary case: most
/// long-lived payloads never see a refused carry.
#[must_use = "true means the block is empty and the caller owes it to the pool"]
pub(crate) fn payload_freed(block: usize) -> bool {
    let mut map = registry().lock().expect("retained index registry poisoned");
    let Some(index) = map.get_mut(&block) else {
        return false;
    };
    if index.payloads == 0 {
        return false;
    }
    index.payloads -= 1;
    empty_now(&mut map, block)
}

/// Whether `block` is held by nothing any more, dropping its index when
/// it is. The index goes **before** the block does, because both
/// enumerators dereference a registered address without testing that its
/// block still exists (module doc).
fn empty_now(map: &mut Registry, block: usize) -> bool {
    let Some(index) = map.get(&block) else {
        return false;
    };
    if index.live > 0 || index.payloads > 0 {
        return false;
    }
    map.remove(&block);
    true
}

/// Hand a retained block that nothing holds any more back to the pool.
/// The kind is stamped first, so a block reaching the pool never carries
/// the retained kind into its next life.
///
/// # Safety
/// `block` is a retained block whose index is gone —
/// [`occupant_freed`] or [`payload_freed`] has just answered true for it.
pub(crate) unsafe fn give_block_back(block: usize) {
    unsafe {
        crate::memory::block_pool::store_block_kind(
            &raw mut (*(block as *mut crate::memory::block_pool::BlockHeader)).kind,
            crate::memory::block_pool::BLOCK_KIND_FREE,
        )
    };
    crate::memory::block_pool::BlockPool::global()
        .put(block as *mut crate::memory::block_pool::BlockHeader);
}

/// The occupancy test both enumerators apply, and the only thing this
/// module reads through an address: a slot whose refcount word is zero
/// holds no live entity.
///
/// # Safety
/// `address` must be readable.
unsafe fn is_occupied(address: usize) -> bool {
    let header = unsafe { *(address as *const u64) };
    header & 0xffff_ffff != 0
}

/// Every retained block and its index, as `(block address, occupants)`.
///
/// The `Arc`s are cloned under the lock and read outside it, so a
/// reader never holds the registry across a walk — which matters
/// because the walk runs user-visible work (the synchronous collection
/// runs destructors) and the reset takes the same lock.
pub(crate) fn snapshot() -> Vec<(usize, Arc<[usize]>)> {
    registry()
        .lock()
        .expect("retained index registry poisoned")
        .iter()
        .map(|(&block, index)| (block, index.occupants.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Take an index out of the process-global registry, which a test
    /// that registered one owes whether or not it emptied it.
    fn drop_index(block: usize) {
        registry()
            .lock()
            .expect("retained index registry poisoned")
            .remove(&block);
    }

    /// A block address and occupants a walk may dereference, which is
    /// what the module doc requires of anything registered here.
    ///
    /// The registry is process-global, so an index left in it is read by
    /// every later walk in the process. The tests below hold the block
    /// pool's test guard, which is what serializes them against the walks
    /// that take it; the cells are **leaked** on top of that, because a
    /// test that panics before it empties its index leaves that index
    /// registered for the rest of the run and no guard covers that.
    /// Freeing the cells would make such an entry a use-after-free rather
    /// than one that reads refcount 0 and is skipped.
    ///
    /// The block address is derived from the cells so that it names the
    /// range they lie in. A constant would be a guess about an address
    /// space the process is also carving regions out of.
    ///
    /// The cells come back as raw pointers beside their addresses, and a
    /// test that occupies one writes through **the pointer its address
    /// was taken from**. Neither half of that is optional: an address
    /// that has been through `usize` carries no provenance to write
    /// with, and writing through the leaked slice's reference instead
    /// pops the exposed raw tags off the borrow stack, so the read the
    /// registry itself performs becomes the violation. Miri rejects
    /// both mistakes, one per run.
    fn walkable_index(n: usize) -> (usize, Vec<usize>, Vec<*mut u64>) {
        let cells: &'static mut [u64] = Box::leak(vec![0u64; n].into_boxed_slice());
        let base = cells.as_mut_ptr();
        let pointers: Vec<*mut u64> = (0..n).map(|i| unsafe { base.add(i) }).collect();
        let addresses: Vec<usize> = pointers.iter().map(|&p| p as usize).collect();
        let block = addresses[0] & !crate::memory::block_pool::BLOCK_MASK;
        (block, addresses, pointers)
    }

    /// Registration sorts, because the census binary-searches the index
    /// and the reset discovers survivors in trace order.
    #[test]
    fn an_index_is_stored_sorted_whatever_order_it_arrives_in() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, _live) = walkable_index(3);
        let _empty = unsafe { register(block, vec![cells[2], cells[0], cells[1]]) };
        let found = snapshot()
            .into_iter()
            .find(|&(b, _)| b == block)
            .expect("registered block is in the snapshot");
        let mut ascending = cells.clone();
        ascending.sort_unstable();
        assert_eq!(&*found.1, &ascending[..]);
        drop_index(block);
    }

    /// The last live occupant's death empties the block, and the index
    /// is gone before the caller is told to hand the block over — the
    /// order the enumerators' readable-address contract requires.
    #[test]
    fn the_last_live_occupant_empties_the_block() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, live) = walkable_index(2);
        unsafe {
            live[0].write(1);
            live[1].write(1);
        }
        let _empty = unsafe { register(block, cells.clone()) };
        assert!(snapshot().iter().any(|&(b, _)| b == block));
        assert!(!occupant_freed(block), "one of two occupants emptied it");
        assert!(snapshot().iter().any(|&(b, _)| b == block));
        assert!(occupant_freed(block), "the second death left it occupied");
        assert!(!snapshot().iter().any(|&(b, _)| b == block));
        unsafe {
            live[0].write(0);
            live[1].write(0);
        }
    }

    /// An occupant already dead when the index is built is not counted,
    /// or the block would wait forever for a death that has happened.
    #[test]
    fn an_occupant_dead_at_registration_holds_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, live) = walkable_index(2);
        let _empty = unsafe {
            live[0].write(1);
            register(block, cells.clone())
        };
        assert!(occupant_freed(block), "the dead occupant was counted live");
        assert!(!snapshot().iter().any(|&(b, _)| b == block));
        unsafe { live[0].write(0) };
    }

    /// A block retained for a payload it could not carry out outlives
    /// its occupants: their deaths say nothing about bytes they do not
    /// own.
    #[test]
    fn a_pinned_block_outlives_its_occupants() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, live) = walkable_index(1);
        pin(block);
        let _empty = unsafe {
            live[0].write(1);
            register(block, cells.clone())
        };
        assert!(!occupant_freed(block), "a pinned block was handed back");
        assert!(
            snapshot().iter().any(|&(b, _)| b == block),
            "registration cleared the pin set before it"
        );
        unsafe { live[0].write(0) };
        drop_index(block);
    }

    /// The payload's own free is the event the block was waiting for, so
    /// a block held for bytes alone goes home when they are freed. Before
    /// this the pin was permanent and the block was out of circulation
    /// for the life of the process; the test was seen failing on
    /// `payload_freed` answering false.
    #[test]
    fn a_freed_payload_empties_the_block_it_pinned() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, live) = walkable_index(1);
        pin(block);
        let _empty = unsafe {
            live[0].write(1);
            register(block, cells.clone())
        };

        assert!(!occupant_freed(block), "the payload still holds it");
        assert!(payload_freed(block), "the last holder of the block died");
        assert!(
            !snapshot().iter().any(|&(b, _)| b == block),
            "the index outlived the block it describes"
        );
        unsafe { live[0].write(0) };
    }

    /// One block can hold the payloads of several survivors, so the pin
    /// is a count and every payload has to report. Seen failing with the
    /// count as a flag: the first free released a block the second
    /// payload was still living in.
    #[test]
    fn a_block_pinned_for_two_payloads_waits_for_both() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, _live) = walkable_index(1);
        pin(block);
        pin(block);
        let _empty = unsafe { register(block, Vec::new()) };

        assert!(!payload_freed(block), "one payload still lives there");
        assert!(payload_freed(block), "both are gone now");
        assert!(!payload_freed(block), "an unpinned block reports nothing");
        // The registry is process-global and a leaked cell's block
        // address can come up again in another test, so nothing is left
        // behind even on the paths where the assertions above hold.
        drop_index(block);
        let _ = cells;
    }

    /// The synchronous enumerator walks a registered index without
    /// checking that the block exists, so a registered address is
    /// dereferenced by whichever thread walks next. A zeroed cell reads
    /// refcount 0 and is skipped, which is the contract; a fabricated
    /// address is a wild read, which is what this pins against.
    #[test]
    fn a_registered_index_is_safe_for_the_enumerator_to_read() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, _live) = walkable_index(4);
        let _empty = unsafe { register(block, cells.clone()) };
        let mut seen = 0usize;
        unsafe {
            crate::memory::heap::for_each_entity_slot(|slot| {
                if cells.contains(&(slot as usize)) {
                    seen += 1;
                }
            })
        };
        drop_index(block);
        assert_eq!(seen, 0, "zeroed cells read refcount 0 and are skipped");
    }
}
