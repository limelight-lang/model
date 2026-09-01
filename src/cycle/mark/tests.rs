use super::*;
use crate::class::ClassBuilder;
use crate::cycle::shadow::Color;
use crate::cycle::testing::row_word;
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockPool, force_oom, test_guard};
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, ll_release, ll_retain};
use crate::test_support::{prop_offset, store_prop};

/// The working count the trace left for `entity`, with its colour
/// asserted met: a count read off an untouched row is whatever the
/// previous tenant of that memory left there.
unsafe fn working_count(entity: *mut Object) -> u32 {
    let word = unsafe { row_word(entity as *mut RcHeader) };
    assert_eq!(
        shadow::color(word),
        Color::Unclassified,
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
mod what_the_trace_subtracts;
