//! One entity per allocation, for an entity no size class serves
//! (`rfc/model/memory/large-entities.md`; `docs/memory-manager.md`,
//! "Entity blocks: the second population").
//!
//! **Commissioning order, of which the zero pass is the half that
//! decides.**
//! The header is written, then the entity's first 8 bytes are zeroed,
//! then the kind is published — a release store, because a collector
//! reads every block's kind concurrently. That word is the
//! occupancy test both enumerators apply, so a block whose entity is not
//! yet published reads refcount 0 and is skipped exactly as a free slot
//! is. Without the pass a pooled block recycled from a raw C buffer
//! carries that buffer's bytes, which read as a live refcount with
//! arbitrary category bits, and the collector then traces a class
//! pointer that is the caller's data. `Heap::refill` zeroes an entity
//! block's slots for the same reason.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicU32;
use std::sync::{Mutex, OnceLock};

use crate::memory::block_pool::{
    BLOCK_KIND_ENTITY_LARGE, BLOCK_KIND_ENTITY_LARGE_RUN, BLOCK_PAYLOAD, BLOCK_SIZE, BlockHeader,
    BlockPool, LINE_SIZE, store_block_kind,
};

/// The first line of a large-entity block. `kind` at offset 0 is the
/// rule every block-aligned allocation obeys, so the address mask plus
/// one load routes a free without a caller-provided size.
#[repr(C)]
pub(crate) struct LargeEntityHeader {
    pub(crate) kind: AtomicU32,
    _pad: u32,
    /// The entity's size in bytes — what a heap block records as a size
    /// class, for a population whose class is one entity wide.
    pub(crate) size: usize,
    /// Bytes the operating system mapped, which [`free`] unmaps.
    /// Zero in the pooled form, which goes back to the block pool.
    run_bytes: usize,
    /// The collector's shadow row for the one entity this block holds,
    /// in the header's free tail rather than in an array of one
    /// (`rfc/model/gc/rc-cycle.md`, "Where the shadow count lives").
    ///
    /// Zero — [`Color::Untouched`](crate::cycle::shadow::Color) — from
    /// commissioning until a collection meets the entity, and nulled
    /// again by that collection's sweep, so a block whose life outlasts
    /// a collection carries no row from it. Plain rather than atomic
    /// like every other field here: the commissioning write is published
    /// by the kind's release store, and the trace token is what keeps
    /// two collectors off it.
    row: u32,
}

/// True for the two kinds this module owns. Callers that dispatch on a
/// block kind ask this rather than listing both, because the pair is
/// meant to grow together or not at all.
#[inline]
pub(crate) fn is_large_entity(kind: u32) -> bool {
    kind == BLOCK_KIND_ENTITY_LARGE || kind == BLOCK_KIND_ENTITY_LARGE_RUN
}

/// The shadow row of the entity `block` holds, which is one word of the
/// block's own header (`LargeEntityHeader::row`).
///
/// The collection writes through it while it holds the trace token, and
/// its sweep zeroes it; between collections it reads zero, which is the
/// untouched colour.
///
/// # Safety
/// `block` must be the header of a live large-entity block, of either
/// kind.
pub(crate) unsafe fn shadow_row(block: *mut u8) -> *mut u32 {
    unsafe { &raw mut (*(block as *mut LargeEntityHeader)).row }
}

/// Allocate one entity of `size` bytes in a block-aligned allocation of
/// its own, and hand back the entity's address.
///
/// **Null on refusal**, which is pool exhaustion or the operating system
/// refusing a mapping, and on a size whose block round-up would overflow. The
/// caller publishes an `RcHeader` into the first 8 bytes, which this
/// function leaves zeroed.
///
/// The memory is **not** zeroed beyond that word: a factory writes every
/// field it declares, exactly as it does in a size-class slot.
pub(crate) fn alloc(size: usize) -> *mut u8 {
    if size <= BLOCK_PAYLOAD {
        let block = BlockPool::global().get();
        if block.is_null() {
            return std::ptr::null_mut();
        }

        unsafe { commission(block as *mut u8, size, 0, BLOCK_KIND_ENTITY_LARGE) }
    } else {
        // `size` reaches here from a class layout, which the compiler
        // controls, but the round-up is guarded all the same: wrapping
        // would hand back a run smaller than the entity it must hold.
        let Some(run_bytes) = size
            .checked_add(LINE_SIZE)
            .and_then(|n| n.checked_add(BLOCK_SIZE - 1))
            .map(|n| n & !(BLOCK_SIZE - 1))
        else {
            return std::ptr::null_mut();
        };

        if run_bytes > isize::MAX as usize {
            return std::ptr::null_mut();
        }

        let block = crate::memory::os::map_aligned(run_bytes, BLOCK_SIZE);
        if block.is_null() {
            return std::ptr::null_mut();
        }

        let entity = unsafe { commission(block, size, run_bytes, BLOCK_KIND_ENTITY_LARGE_RUN) };
        // After the commissioning, never before: registration is what
        // makes the run reachable to an enumerator, and what it must
        // find there is a header word, zeroed.
        runs().lock().unwrap().insert(block as usize);
        entity
    }
}

/// Header, then the occupancy word, then the kind. See the module doc:
/// the middle step is what makes the last one safe.
///
/// # Safety
/// `block` is a fresh block-aligned allocation of at least
/// `LINE_SIZE + size` bytes that nothing else refers to.
unsafe fn commission(block: *mut u8, size: usize, run_bytes: usize, kind: u32) -> *mut u8 {
    let header = block as *mut LargeEntityHeader;
    unsafe {
        // Field by field rather than one struct store, because a struct
        // store covers `kind` too: a pooled block is inside a carved
        // region, where the collector loads every block's kind with an
        // acquire, and a plain store to that word is a data race however
        // little the value changes. `kind` is written once, below,
        // through the only function allowed to write it.
        (&raw mut (*header)._pad).write(0);
        (&raw mut (*header).size).write(size);
        (&raw mut (*header).run_bytes).write(run_bytes);
        (&raw mut (*header).row).write(0);
        let entity = block.add(LINE_SIZE);
        (entity as *mut u64).write(0);
        store_block_kind(&raw const (*header).kind, kind);
        entity
    }
}

/// Give a large entity's memory back. The pooled form returns its block
/// to the pool, which re-stamps the kind on the way in; a run leaves the
/// registry and is then unmapped, at the size its commissioning recorded.
///
/// # Safety
/// `block` is the block header of a live large-entity allocation whose
/// entity is dead, and `kind` is the kind read from it.
pub(crate) unsafe fn free(block: *mut u8, kind: u32) {
    match kind {
        BLOCK_KIND_ENTITY_LARGE => BlockPool::global().put(block as *mut BlockHeader),
        BLOCK_KIND_ENTITY_LARGE_RUN => {
            // The entry goes before the memory does, because both
            // enumerators dereference a registered address without
            // testing that its block still exists — the rule
            // `memory/retained.rs` states for its own index.
            runs().lock().unwrap().remove(&(block as usize));
            let run_bytes = unsafe { (*(block as *const LargeEntityHeader)).run_bytes };
            crate::memory::os::unmap(block, run_bytes);
        }
        _ => debug_assert!(false, "not a large-entity block: kind {kind}"),
    }
}

/// The entity a large-entity block holds, and how big it is.
///
/// # Safety
/// `block` is the block header of a live large-entity allocation.
#[inline]
pub(crate) unsafe fn occupant(block: *mut u8) -> (*mut u8, usize) {
    let size = unsafe { (*(block as *const LargeEntityHeader)).size };
    (unsafe { block.add(LINE_SIZE) }, size)
}

/// Every OS-direct run alive at this moment, cloned out under the lock so
/// the caller walks the addresses without holding it — the contract
/// `memory/retained.rs` keeps for its own index, and for the same reason:
/// a visitor runs arbitrary code and must not do it under a lock the
/// allocator takes.
///
/// **A returned address may be dereferenced, and the reason is worth
/// stating here rather than at the three sites that rely on it.** Unlike
/// every other address either enumerator holds, a run's memory can be
/// **unmapped**: [`free`] returns it to the operating system. Three things
/// together make the read sound — the registry entry is removed strictly
/// before the unmap, a free during a collection withholds instead of
/// running, and a collection does not begin reading a thread's blocks
/// until that thread has entered it. The owner-side withholding is S36.2's;
/// S38.1/S38.3 must make the claim and withholding owner-addressable before a
/// worker may rely on all three (`PLAN.md`).
///
/// **A visitor must not free a run while walking this list.** The
/// addresses are a snapshot, so a run freed during the walk leaves
/// whichever element names it pointing at memory the operating system
/// has taken back. The retained index tolerates it — former arena memory
/// is never unmapped — and this one does not.
pub(crate) fn snapshot() -> Vec<usize> {
    runs().lock().unwrap().iter().copied().collect()
}

/// The registry of OS-direct runs: one address per run, and nothing
/// else, because a run's occupant index has length one and is computed
/// (`block + LINE_SIZE`).
///
/// A table of its own rather than an arm of `memory/retained.rs`: a
/// retained block dies when two counters reach zero and goes back to the
/// **pool**, while a run dies with its single entity and must reach
/// `dealloc`. Sharing the table would put that branch on the entry, in
/// the reclamation path itself, where the wrong arm either unmaps a
/// pooled block or leaks a run.
fn runs() -> &'static Mutex<BTreeSet<usize>> {
    static RUNS: OnceLock<Mutex<BTreeSet<usize>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

#[cfg(test)]
mod tests;
