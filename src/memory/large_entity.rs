//! One entity per allocation, for an entity no size class serves
//! (`rfc/model/memory/large-entities.md`).
//!
//! Past what a category's allocator packs in one slot an entity is not
//! packed at all: it keeps its inline layout whole as the sole occupant
//! of a block-aligned allocation whose first line is a block header,
//! entity at `+LINE_SIZE`. Field access is unchanged, because the cells
//! are at the offsets the class assigned them, and the walker visits
//! exactly one slot in such a block.
//!
//! **The split is by size, and it decides who reclaims.** Up to one
//! block payload the allocation is a pooled block, which the region scan
//! already reaches; above it the allocation comes from the system
//! allocator, block-aligned so the address mask still finds the header,
//! and it is entered into the registry below because no region contains
//! it.
//!
//! **Commissioning order, and the zero pass is the load-bearing half.**
//! The header is written, then the entity's first 8 bytes are zeroed,
//! then the kind is published — release under `rc-walk`, because the
//! collector reads every block's kind concurrently. That word is the
//! occupancy test both enumerators apply, so a block whose entity is not
//! yet published reads refcount 0 and is skipped exactly as a free slot
//! is. Without the pass a pooled block recycled from a raw C buffer
//! carries that buffer's bytes, which read as a live refcount with
//! arbitrary category bits, and the collector then traces a class
//! pointer that is the caller's data. `Heap::refill` zeroes an entity
//! block's slots for the same reason.

use std::alloc::Layout;
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
    /// Bytes the system allocator granted, for the matching `dealloc`.
    /// Zero in the pooled form, which goes back to the block pool.
    run_bytes: usize,
}

/// True for the two kinds this module owns. Callers that dispatch on a
/// block kind ask this rather than listing both, because the pair is
/// meant to grow together or not at all.
#[inline]
pub(crate) fn is_large_entity(kind: u32) -> bool {
    kind == BLOCK_KIND_ENTITY_LARGE || kind == BLOCK_KIND_ENTITY_LARGE_RUN
}

/// Allocate one entity of `size` bytes in a block-aligned allocation of
/// its own, and hand back the entity's address.
///
/// **Null on refusal**, which is pool exhaustion or the system allocator
/// saying no, and on a size whose block round-up would overflow. The
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
        let Ok(layout) = Layout::from_size_align(run_bytes, BLOCK_SIZE) else {
            return std::ptr::null_mut();
        };
        let block = unsafe { std::alloc::alloc(layout) };
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
        let entity = block.add(LINE_SIZE);
        (entity as *mut u64).write(0);
        store_block_kind(&raw mut (*header).kind, kind);
        entity
    }
}

/// Give a large entity's memory back. The pooled form returns its block
/// to the pool, which re-stamps the kind on the way in; a run leaves the
/// registry and then goes to the system allocator with the layout it was
/// granted.
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
            let layout = Layout::from_size_align(run_bytes, BLOCK_SIZE)
                .expect("a run's layout was accepted when it was allocated");
            unsafe { std::alloc::dealloc(block, layout) };
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
/// **unmapped**: [`free`] hands it to the system allocator. Three things
/// together make the read sound — the registry entry is removed strictly
/// before the `dealloc`, a free during a collection epoch parks instead
/// of running, and the collector waits for the handshake ack that
/// publishes the epoch before it snapshots. A caller outside those rules
/// is reading memory that may be gone.
///
/// **A visitor must not free a run while walking this list.** The
/// addresses are a snapshot; freeing one leaves every later element
/// naming memory the system allocator has taken back. The retained index
/// tolerates it — former arena memory is never unmapped — and this one
/// does not.
///
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
mod tests {
    use super::*;
    use crate::memory::block_pool::{BLOCK_MASK, test_guard};
    use crate::refcount::MemoryCategory;
    use std::sync::atomic::Ordering;

    /// Up to one block payload the allocation is a pooled block: its
    /// kind says so, the entity sits at `+LINE_SIZE`, its header word
    /// reads zero, and no registry entry is made — the region scan
    /// already reaches it.
    #[test]
    fn a_pooled_large_entity_is_block_aligned_and_reads_dead_until_published() {
        let _g = test_guard();
        let before = snapshot().len();

        let entity = alloc(BLOCK_PAYLOAD);
        assert!(!entity.is_null());
        let block = (entity as usize & !BLOCK_MASK) as *mut u8;
        assert_eq!(
            entity as usize - block as usize,
            LINE_SIZE,
            "the entity starts after the header line"
        );
        assert_eq!(
            unsafe {
                (*(block as *const LargeEntityHeader))
                    .kind
                    .load(Ordering::Relaxed)
            },
            BLOCK_KIND_ENTITY_LARGE
        );
        assert_eq!(
            unsafe { *(entity as *const u64) },
            0,
            "the occupancy word is zeroed before the kind is published"
        );
        assert_eq!(unsafe { occupant(block) }, (entity, BLOCK_PAYLOAD));
        assert_eq!(snapshot().len(), before, "a pooled block needs no registry");

        unsafe { free(block, BLOCK_KIND_ENTITY_LARGE) };
    }

    /// The case that opened the stage, through the real factory: a class
    /// of 10 000 declared properties, which PHP 8.6.0-dev runs at 163 840
    /// bytes an instance. On this layout that is 160 016 bytes, so the
    /// object takes a run of three blocks, and every one of its slots is
    /// where the class put it — the point of keeping the inline layout
    /// whole rather than indirecting the cells.
    #[test]
    fn an_object_of_ten_thousand_properties_is_built_read_and_torn_down() {
        let _g = test_guard();
        let mut builder = crate::class::ClassBuilder::new("WideClass");
        let names: Vec<String> = (0..10_000).map(|i| format!("p{i}")).collect();
        for name in &names {
            // Boxed, so a slot is the 16 bytes the measurement counted; a
            // scalar class is half the size and lands in a two-block run,
            // where the last slot's offset is no longer where this test
            // writes.
            builder = builder.prop(name, true);
        }
        let class = builder.build();
        assert_eq!(
            unsafe { (*class).object_size } as usize,
            16 + 10_000 * 16,
            "the header and ten thousand Box slots"
        );

        let mut arena = crate::memory::arena::Arena::new();
        let mut ctx = crate::memory::context::LLContext { arena: &mut arena };
        unsafe {
            let obj = crate::object::new_constructed(&mut ctx, class, MemoryCategory::GcHeap);
            assert!(!obj.is_null(), "a wide class is instantiable");
            let block = (obj as usize & !BLOCK_MASK) as *mut u8;
            assert_eq!(
                (*(block as *const LargeEntityHeader))
                    .kind
                    .load(Ordering::Relaxed),
                BLOCK_KIND_ENTITY_LARGE_RUN
            );

            // The first and the last slot the class laid out, which is
            // what a lost header line or a short round-up would take out.
            let first = crate::object::Object::prop_at(obj, 16);
            let last = crate::object::Object::prop_at(obj, 16 + 9_999 * 16);
            assert_eq!(
                last as usize + 16 - obj as usize,
                (*class).object_size as usize,
                "the last slot ends where the class says the object does"
            );
            first.write(crate::value::Value::int(-1));
            last.write(crate::value::Value::int(9_999));
            assert_eq!(first.read().as_int(), -1);
            assert_eq!(last.read().as_int(), 9_999);

            assert!(crate::refcount::ll_release(
                obj as *mut crate::refcount::RcHeader
            ));
            crate::object::ll_entity_die(obj as *mut crate::refcount::RcHeader);
            assert!(
                !snapshot().contains(&(block as usize)),
                "and its run left the registry with it"
            );
        }
        arena.reset(|_| {});
    }

    /// A second free of the same pooled block does nothing. It is the one
    /// mistake here that corrupts rather than leaks: `BlockPool::put`
    /// twice threads the block into the free chain twice, and the second
    /// taker gets memory the first is already using. What stops it is
    /// that `put` re-stamps the kind, so the second `ll_free` reads
    /// `BLOCK_KIND_FREE` and falls through the arm that tolerates it.
    #[test]
    fn a_second_free_of_a_pooled_large_entity_does_nothing() {
        let _g = test_guard();
        let entity = alloc(20_000);
        assert!(!entity.is_null());
        let block = (entity as usize & !BLOCK_MASK) as *mut u8;

        unsafe {
            crate::memory::stdapi::ll_free(entity);
            assert_eq!(
                (*(block as *const LargeEntityHeader))
                    .kind
                    .load(Ordering::Relaxed),
                crate::memory::block_pool::BLOCK_KIND_FREE,
                "the pool re-stamps on the way in, which is what makes the \
                 second free legible"
            );
            crate::memory::stdapi::ll_free(entity);
        }

        // The proof that the chain is intact: a block comes back out and
        // is the one that went in.
        let again = alloc(20_000);
        assert_eq!(
            (again as usize & !BLOCK_MASK) as *mut u8,
            block,
            "the freed block is handed out once, not twice"
        );
        unsafe { free(block, BLOCK_KIND_ENTITY_LARGE) };
    }

    /// Exhaustion is reported, not absorbed — and the two halves draw on
    /// different allocators, which is what makes the refusal legible:
    /// with the pool refusing, the pooled half is null while the run half
    /// is served, so a null here can only have come from the pool.
    #[test]
    fn the_pooled_half_reports_pool_exhaustion_and_the_run_half_is_unaffected() {
        let _g = test_guard();
        crate::memory::block_pool::FORCE_OOM.store(true, std::sync::atomic::Ordering::Relaxed);

        let pooled = alloc(BLOCK_PAYLOAD);
        let run = alloc(BLOCK_PAYLOAD + 1);

        crate::memory::block_pool::FORCE_OOM.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(pooled.is_null(), "the pool refused and the refusal carried");
        assert!(
            !run.is_null(),
            "the run comes from the system allocator, which was not refusing"
        );

        let block = (run as usize & !BLOCK_MASK) as *mut u8;
        unsafe { free(block, BLOCK_KIND_ENTITY_LARGE_RUN) };
    }

    /// One byte more and the allocation is a run: outside every region,
    /// so the registry is what makes it findable, and the entry goes
    /// when the memory does.
    #[test]
    fn a_run_is_registered_while_it_lives_and_gone_after_it_is_freed() {
        let _g = test_guard();
        let size = BLOCK_PAYLOAD + 1;

        let entity = alloc(size);
        assert!(!entity.is_null());
        let block = (entity as usize & !BLOCK_MASK) as *mut u8;
        assert_eq!(
            unsafe {
                (*(block as *const LargeEntityHeader))
                    .kind
                    .load(Ordering::Relaxed)
            },
            BLOCK_KIND_ENTITY_LARGE_RUN
        );
        assert_eq!(
            entity as usize - block as usize,
            LINE_SIZE,
            "the entity starts after the header line here too — the block
             round-up leaves slack at the tail, so no write in this test
             would fault on a lost line"
        );
        assert!(
            snapshot().contains(&(block as usize)),
            "a run is enumerated from the registry or not at all"
        );
        assert_eq!(unsafe { occupant(block) }.1, size);

        // The last byte the entity claims is inside the allocation: a
        // round-up that lost the header line would fault here.
        unsafe { entity.add(size - 1).write(0xab) };

        unsafe { free(block, BLOCK_KIND_ENTITY_LARGE_RUN) };
        assert!(
            !snapshot().contains(&(block as usize)),
            "and the entry goes before the memory"
        );
    }
}
