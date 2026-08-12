//! Object indexes for retained former-arena blocks.
//!
//! Why such a block cannot be walked without one, and what goes
//! uncollected then: `rfc/model/gc/retained-block-walk.md`, and
//! `docs/memory-manager.md`, "Arena reset: the settling loop".
//!
//! The inventory already exists: `promote`'s reset fixpoint collects every
//! survivor into a vector. This module keeps it as one sorted array of
//! addresses per retained block and hands it to the two enumerators that
//! need it, `heap::for_each_entity_slot` for the synchronous walk and
//! `heap::snapshot_entity_blocks` for the collector's epoch.
//!
//! # What this module does not know
//!
//! What lives at those addresses. It stores block addresses and arrays of
//! addresses; entities, classes, refcounts and verdicts belong to the
//! layers above. It reads one word of one of them, in two places and for
//! one purpose: the refcount word is the occupancy test, and the
//! live-occupant count is what decides when a block has emptied.
//!
//! # The one requirement on an address
//!
//! Every address in a registered index stays **readable** for as long as
//! the index is registered. Both enumerators read its refcount word
//! without first testing that the block still exists, which they may
//! because a retained block leaves circulation only once its last survivor
//! is gone. An address that stops being mapped is a wild read on whichever
//! thread walks next, and both walks are process-global, so the index is
//! dropped **before** the block is handed to the pool.
//!
//! An index needs no version and no lock beyond the one guarding the
//! registry map: nothing allocates into a dead arena, so a block's
//! population only shrinks, and readers walk a cloned `Arc` outside it.
//!
//! A block may be retained for **bytes** rather than for occupants, when a
//! survivor's out-of-line payload could not be carried out of the dying
//! arena. It is then held by two populations and goes home when both are
//! empty: occupants counted here, payloads counted by [`pin`] and spent by
//! [`payload_freed`] (`dev/DECISIONS.md`, "a pinned block goes home when
//! its last payload is freed").

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
mod tests;
