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
//! the layers above. The occupancy test that makes a stale entry
//! harmless — a survivor that later dies reads refcount 0 — is applied
//! by the readers, not here.
//!
//! # The one requirement on an address
//!
//! Every address in a registered index stays **readable** for as long as
//! the index is registered. Both enumerators read its refcount word
//! without first testing that the block still exists — they can, because
//! a retained block leaves circulation only once its last survivor is
//! gone — so an address that stops being mapped is a wild read on
//! whichever thread walks next, and both walks are process-global.
//!
//! # Why the arrays never change under a reader
//!
//! Nothing allocates into a dead arena, so a retained block's
//! population only shrinks, and it shrinks by entities dying rather
//! than by slots being reissued: a retained block leaves circulation
//! only when all of its survivors are gone. An index therefore needs no
//! version, no growth path and no lock beyond the one guarding the map
//! itself; readers clone the `Arc` and walk it outside the lock.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Block address (the block header, 64 KiB-aligned) → its occupants,
/// sorted ascending.
type Registry = BTreeMap<usize, Arc<[usize]>>;

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record `occupants` as the object index of retained block `block`.
///
/// `occupants` is sorted here rather than at the call site, because the
/// census's lookup is a binary search over it and the reset builds it
/// in discovery order. A block registered twice keeps the newer index:
/// one reset produces one index per block it retains.
pub(crate) fn register(block: usize, mut occupants: Vec<usize>) {
    occupants.sort_unstable();
    registry()
        .lock()
        .expect("retained index registry poisoned")
        .insert(block, occupants.into());
}

/// Drop the index of `block`, which is what returning a fully emptied
/// retained block to the pool must do.
///
/// Nothing calls this yet: a retained block stays out of circulation
/// while its survivors live and today never comes back
/// (`rfc/model/memory/arena-reset.md`, Retention; `PLAN.md` carries the
/// return mechanism as a small future item). The index therefore lives
/// exactly as long as the retention it describes. This exists so that
/// hook is one call rather than a search.
#[allow(dead_code)]
pub(crate) fn release(block: usize) {
    registry()
        .lock()
        .expect("retained index registry poisoned")
        .remove(&block);
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
        .map(|(&block, index)| (block, index.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block address and occupants a walk may dereference, which is
    /// what the module doc requires of anything registered here.
    ///
    /// The registry is process-global, so an index left in it is read by
    /// every later walk in the process. The tests below hold the block
    /// pool's test guard, which is what serializes them against the walks
    /// that take it; the cells are **leaked** on top of that, because a
    /// test that panics between `register` and `release` leaves its index
    /// registered for the rest of the run and no guard covers that.
    /// Freeing the cells would make such an entry a use-after-free rather
    /// than one that reads refcount 0 and is skipped.
    ///
    /// The block address is derived from the cells so that it names the
    /// range they lie in. A constant would be a guess about an address
    /// space the process is also carving regions out of.
    fn walkable_index(n: usize) -> (usize, Vec<usize>) {
        let cells: &'static mut [u64] = Box::leak(vec![0u64; n].into_boxed_slice());
        let addresses: Vec<usize> = cells.iter().map(|c| c as *const u64 as usize).collect();
        let block = addresses[0] & !crate::memory::block_pool::BLOCK_MASK;
        (block, addresses)
    }

    /// Registration sorts, because the census binary-searches the index
    /// and the reset discovers survivors in trace order.
    #[test]
    fn an_index_is_stored_sorted_whatever_order_it_arrives_in() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells) = walkable_index(3);
        register(block, vec![cells[2], cells[0], cells[1]]);
        let found = snapshot()
            .into_iter()
            .find(|&(b, _)| b == block)
            .expect("registered block is in the snapshot");
        let mut ascending = cells.clone();
        ascending.sort_unstable();
        assert_eq!(&*found.1, &ascending[..]);
        release(block);
    }

    /// Release is what a returned block owes; after it the block is
    /// invisible to every enumerator again.
    #[test]
    fn a_released_block_leaves_the_snapshot() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells) = walkable_index(1);
        register(block, cells);
        assert!(snapshot().iter().any(|&(b, _)| b == block));
        release(block);
        assert!(!snapshot().iter().any(|&(b, _)| b == block));
    }

    /// The synchronous enumerator walks a registered index without
    /// checking that the block exists, so a registered address is
    /// dereferenced by whichever thread walks next. A zeroed cell reads
    /// refcount 0 and is skipped, which is the contract; a fabricated
    /// address is a wild read, which is what this pins against.
    #[test]
    fn a_registered_index_is_safe_for_the_enumerator_to_read() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells) = walkable_index(4);
        register(block, cells.clone());
        let mut seen = 0usize;
        unsafe {
            crate::memory::heap::for_each_entity_slot(|slot| {
                if cells.contains(&(slot as usize)) {
                    seen += 1;
                }
            })
        };
        release(block);
        assert_eq!(seen, 0, "zeroed cells read refcount 0 and are skipped");
    }
}
