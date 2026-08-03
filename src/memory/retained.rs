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

    /// Registration sorts, because the census binary-searches the index
    /// and the reset discovers survivors in trace order.
    #[test]
    fn an_index_is_stored_sorted_whatever_order_it_arrives_in() {
        let block = 0x4000_0000usize;
        register(block, vec![0x4000_3000, 0x4000_1000, 0x4000_2000]);
        let found = snapshot()
            .into_iter()
            .find(|&(b, _)| b == block)
            .expect("registered block is in the snapshot");
        assert_eq!(&*found.1, &[0x4000_1000, 0x4000_2000, 0x4000_3000]);
        release(block);
    }

    /// Release is what a returned block owes; after it the block is
    /// invisible to every enumerator again.
    #[test]
    fn a_released_block_leaves_the_snapshot() {
        let block = 0x5000_0000usize;
        register(block, vec![0x5000_1000]);
        assert!(snapshot().iter().any(|&(b, _)| b == block));
        release(block);
        assert!(!snapshot().iter().any(|&(b, _)| b == block));
    }
}
