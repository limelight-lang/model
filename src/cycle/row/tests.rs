use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::block_pool::{BLOCK_MASK, LINE_SIZE, load_block_kind, test_guard};
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, RcHeader, ll_release};
use crate::test_support::store_prop;

/// The row a slot's own address derives, by the division the crate
/// already performs on this shape (`heap::describe_slot`) rather than by
/// the reciprocal multiply under test. That the two agree for every size
/// class and every slot is
/// `heap::tests::the_block_under_the_slots::`
/// `the_reciprocal_multiply_is_the_division_over_a_whole_block`; here
/// the division
/// is the independent witness that the dispatch reached the entity's own
/// row and not merely a row.
///
/// `object_size` is the class's, because a slot carries no record of the
/// class it was cut for.
fn row_by_division(entity: *mut Object, object_size: usize) -> RowKey {
    let address = entity as usize;
    let block = address & !BLOCK_MASK;
    let stride =
        crate::memory::heap::SIZE_CLASSES[crate::memory::heap::size_class_index(object_size)
            .expect("the fixture's class fits a size class")];
    RowKey {
        block,
        index: ((address - block - LINE_SIZE) / stride) as u32,
        population: Population::Slotted,
    }
}

/// The kind stamped on the block holding `address`, read the way its owning
/// thread reads it. A fixture asserts on this before asking
/// `resolve_edge_target` anything: a test of the retained arm that quietly
/// built an arena block would pass on the wrong branch.
unsafe fn block_kind(address: usize) -> u32 {
    let header = crate::memory::block_pool::BlockHeader::of_ptr(address as *const u8);
    unsafe { load_block_kind(&raw const (*header).kind) }
}

mod the_entity_behind_a_row;
mod the_row_each_population_resolves_to;
