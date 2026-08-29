use super::*;
use crate::class::ClassBuilder;
use crate::cycle::row::Row;
use crate::cycle::shadow::{Colour, RowArray};
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockPool, FORCE_OOM, test_guard};
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, ll_release, ll_retain};
use crate::test_support::store_prop;
use std::sync::atomic::Ordering;

/// The offset of a class's `index`-th declared property. The tests here
/// build their graphs out of one-Value properties, which is the layout
/// `ClassBuilder` gives a boxed slot: the header and the class word take
/// the first sixteen bytes, and each property takes sixteen after them.
fn prop_offset(index: u32) -> u32 {
    16 + 16 * index
}

/// The row word the trace left for `entity`, read the way the scan will
/// read it — through the block's own shadow pointer. A second meeting
/// would answer too, and would be the wrong instrument: it initialises a
/// row the trace never reached, so a test built on it cannot tell an
/// untouched row from a met one.
unsafe fn row_word(entity: *mut RcHeader) -> u32 {
    let Edge::Interior(Row {
        block,
        index,
        population: _,
    }) = (unsafe { edge_to(entity) })
    else {
        panic!("the fixture's entity is not a GC-heap entity");
    };

    let array = unsafe { crate::memory::heap::block_shadow(block as *mut u8) } as *mut RowArray;
    assert!(!array.is_null(), "the trace touched this entity's block");
    unsafe { *shadow::row(array, index) }
}

/// The working count the trace left for `entity`, with its colour
/// asserted met: a count read off an untouched row is whatever the
/// previous tenant of that memory left there.
unsafe fn working_count(entity: *mut Object) -> u32 {
    let word = unsafe { row_word(entity as *mut RcHeader) };
    assert_eq!(
        shadow::colour(word),
        Colour::Met,
        "the trace met this entity"
    );
    shadow::count(word)
}

/// Every live entity's header word in the process, folded.
///
/// The instrument for "the trace writes into no entity", and it is the
/// counted state that a trial deletion gone wrong would land in: a mark
/// that subtracted from the refcount instead of the row moves exactly
/// these bytes. Folded rather than compared entity by entity because the
/// population is the whole heap and the question is a yes or a no.
///
/// # Safety
/// A quiescent mutator, as `heap::for_each_entity_slot`.
unsafe fn every_header_folded() -> u64 {
    let mut folded = FNV_OFFSET;
    unsafe {
        crate::memory::heap::for_each_entity_slot(|entity| {
            let (refcount, flags) = crate::refcount::header_pair(entity);
            folded = fold(folded, entity as u64);
            folded = fold(folded, u64::from(refcount) << 32 | u64::from(flags));
        })
    };

    folded
}

/// The whole of each named object's bytes, folded — its cells included,
/// which the header fold above does not cover.
///
/// # Safety
/// Each pair is a live object and the size its class declares.
unsafe fn object_bytes_folded(objects: &[(*mut Object, usize)]) -> u64 {
    let mut folded = FNV_OFFSET;
    for &(object, size) in objects {
        for i in 0..size {
            folded = fold(
                folded,
                u64::from(unsafe { (object as *const u8).add(i).read() }),
            );
        }
    }

    folded
}

/// FNV-1a's basis and its multiply. The fold has no cryptographic duty:
/// what it stands against is a stray write, not an adversary.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fold(state: u64, word: u64) -> u64 {
    let mut folded = state;
    for byte in word.to_le_bytes() {
        folded = (folded ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
    }

    folded
}

mod an_aborted_mark_writes_nothing;
mod the_descent_carries_its_own_stack;
mod what_the_trace_subtracts;
