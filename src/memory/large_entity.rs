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

use std::sync::Mutex;
use std::sync::atomic::AtomicU32;

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
    /// Registry link toward the head, null in the head itself. Who may
    /// read or write the pair, and when it is meaningful, is [`Runs`].
    prev: *mut LargeEntityHeader,
    /// Registry link toward the tail, null in the last run.
    next: *mut LargeEntityHeader,
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
        // After the commissioning, never before: `commission` nulls the
        // link words, so a run linked ahead of it would have its `next`
        // overwritten and orphan every run behind it. Registration is
        // also what makes the run reachable to an enumerator, and what
        // it must find there is a header word, zeroed.
        unsafe { link(block as *mut LargeEntityHeader) };
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
        // Null here and nowhere else: `link` writes the new head's
        // `next` and the old head's `prev`, and takes the new head's own
        // `prev` from this write. A pooled block is never linked and
        // carries the pair null for as long as it is a large entity.
        (&raw mut (*header).prev).write(std::ptr::null_mut());
        (&raw mut (*header).next).write(std::ptr::null_mut());
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
    // A block returned under a standing `DEAD_IN_PLACE` goes to the pool or
    // to the operating system while a sweep is still owed the mark, and the
    // sweep would then hand the same memory back a second time. The clear
    // belongs to whoever returns it (`crate::refcount::clear_dead_in_place`),
    // and this is where a path that forgot it shows up.
    debug_assert_ne!(
        unsafe {
            let (entity, _) = occupant(block);
            crate::refcount::slot_state(entity as *const crate::refcount::RcHeader)
        },
        crate::refcount::SlotState::DeadInPlace,
        "a large entity's memory is returned with its dead-in-place mark cleared"
    );

    match kind {
        BLOCK_KIND_ENTITY_LARGE => BlockPool::global().put(block as *mut BlockHeader),
        BLOCK_KIND_ENTITY_LARGE_RUN => {
            // The entry goes before the memory does, because both
            // enumerators dereference a registered address without
            // testing that its block still exists — the rule
            // `memory/retained.rs` states for the addresses it publishes.
            unsafe { unlink(block as *mut LargeEntityHeader) };
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
/// the caller walks the addresses without holding it: a visitor runs
/// arbitrary code and must not do it under a lock the allocator takes.
///
/// **It knowingly breaks that rule for itself**, as `BlockPool::regions`
/// does: the `Vec` grows under the registry mutex, one push per live run.
/// Nothing it calls reaches [`link`], [`unlink`] or `snapshot` again, so
/// the re-entry the rule guards against cannot happen; an allocation
/// failure aborts, which is the residue, and the visiting form that would
/// remove the `Vec` is refused for the re-entry it opens instead
/// (`dev/DECISIONS.md`, "the registry of OS-direct runs is threaded through
/// the runs").
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
/// has taken back. A retained block's survivor list tolerates it — former
/// arena memory is never unmapped — and this one does not.
///
/// The order is reverse registration, the newest run first, because that
/// is where [`link`] puts one. No caller may depend on it: what the list
/// answers is membership.
pub(crate) fn snapshot() -> Vec<usize> {
    let runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    let mut run = runs.head;
    while !run.is_null() {
        out.push(run as usize);
        run = unsafe { (*run).next };
    }
    out
}

/// The registry of OS-direct runs: the head of a list whose nodes are the
/// runs themselves, linked through [`LargeEntityHeader`]'s `prev` and
/// `next`. One address per run and nothing else, because a run's occupant
/// index has length one and is computed (`block + LINE_SIZE`).
///
/// **The list is exactly the runs between [`alloc`]'s return and the unmap
/// in [`free`]**, and the mutex is what publishes it: a run is linked
/// under the lock strictly after its kind's release store and unlinked
/// under the lock strictly before its mapping goes back to the operating
/// system. A reader that holds the lock therefore sees only addresses it
/// may dereference.
///
/// The index of a run lives in the run, because a table beside them would
/// be a second lifetime to keep: `free` runs inside a collection's close,
/// where an allocation is refused, and a table there both takes memory to
/// insert and gives memory back to remove. Doubly linked for the same
/// path — `free` removes an arbitrary run, and a single link would walk
/// the live runs under a process-global lock for every dead one.
///
/// A structure of its own rather than an arm of `memory/retained.rs`,
/// which answers a different question. What a retained block publishes in
/// its own header says whether that one block is still held, and the
/// module names no set: every reader arrives with an address. A run has to
/// be **enumerated**, having no region to be scanned out of, so the words
/// that find it cannot be per-block state alone. The ends of life diverge
/// with the question: a retained block goes back to the pool through
/// `retained::release_emptied`, while a run's memory returns to the
/// operating system and must never reach the pool.
///
/// Before [`link`], and in the pooled half which is never linked, both
/// words read null — that is what [`commission`] writes. After [`unlink`]
/// they still name the departed neighbours, and the mapping carrying them
/// goes back to the operating system before anything can read them. So
/// neither word is self-describing: a null `prev` means "the head" only
/// for a run the list holds, every other block carrying one too, which is
/// why [`unlink`] asserts membership before it believes the head arm.
struct Runs {
    head: *mut LargeEntityHeader,
}

// SAFETY: `head` is a raw pointer with no thread affinity, and it and
// every `prev`/`next` reachable from it are read and written only with
// this mutex held. A run is linked after its mapping exists and unlinked
// before that mapping goes back to the operating system, so no pointer
// reachable from `head` names an unmapped page.
unsafe impl Send for Runs {}

/// `Mutex::new` is `const`, so no lazy cell stands between a free and the
/// list. That the lock itself takes no memory is what
/// `tests::what_the_registry_asks_the_allocator` measures.
///
/// **Every taker recovers a poisoned lock rather than propagating it, and
/// what makes that sound is that no section stores before its last panic
/// site**: [`link`] and [`unlink`] assert and then write raw pointers
/// only, and [`snapshot`] allocates under the lock but mutates nothing, so
/// a panic here cannot leave a half-linked list. Propagating instead would
/// abort — [`unlink`] is reached from `stdapi::ll_free`, whose C-ABI
/// callers `object::ll_entity_die` and `stdapi::ll_c_free` turn an unwind
/// into one — and a failed debug assertion in one test would take the
/// process. A section added here that stores and can then panic breaks
/// this and propagates instead.
static RUNS: Mutex<Runs> = Mutex::new(Runs {
    head: std::ptr::null_mut(),
});

/// Put `run` at the head of the registry, which is what makes it visible
/// to an enumerator.
///
/// # Safety
/// `run` is a commissioned run block that is not in the list, its kind is
/// already published, and its `prev` reads null: this writes the new
/// head's `next` and the old head's `prev`, and takes the new head's own
/// `prev` from [`commission`].
unsafe fn link(run: *mut LargeEntityHeader) {
    let mut runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        // A second link of the run already at the head writes `next = run`
        // and leaves [`snapshot`] walking a self-loop under this mutex,
        // which hangs every allocation and free of a run behind it with no
        // fault and no output.
        debug_assert!(
            !std::ptr::eq(runs.head, run),
            "a run entered the registry a second time"
        );
        debug_assert!(
            (*run).prev.is_null(),
            "a run entered the registry with `prev` already set"
        );
        (*run).next = runs.head;
        if !runs.head.is_null() {
            (*runs.head).prev = run;
        }
    }
    runs.head = run;
}

/// Take `run` out of the registry. Its own link words are left as they
/// are: the caller unmaps the memory holding them before returning.
///
/// # Safety
/// `run` is in the list, and the caller is the thread freeing it.
unsafe fn unlink(run: *mut LargeEntityHeader) {
    let mut runs = RUNS.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let prev = (*run).prev;
        let next = (*run).next;
        // A null `prev` means the head here, and `commission` gives every
        // block one — so an unlink of something never linked would take
        // the head arm and empty the whole registry.
        debug_assert!(
            !prev.is_null() || std::ptr::eq(runs.head, run),
            "unlink of a run the registry does not hold"
        );
        if prev.is_null() {
            runs.head = next;
        } else {
            (*prev).next = next;
        }
        if !next.is_null() {
            (*next).prev = prev;
        }
    }
}

#[cfg(test)]
mod tests;
