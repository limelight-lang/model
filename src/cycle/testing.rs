//! What the collector's tests read out of a shadow row, and the offset
//! the graphs they build stand on.
//!
//! Two test trees ask the same two questions — the mark's and the
//! scan's — and a second copy of the row lookup would be a second
//! opinion about where a row is. Test builds only.

use crate::cycle::row::{Edge, Row, edge_to};
use crate::cycle::shadow::{self, Colour, RowArray};
use crate::refcount::RcHeader;

/// The offset of a class's `index`-th declared property. The collector's
/// tests build their graphs out of one-Value properties, which is the
/// layout `ClassBuilder` gives a boxed slot: the header and the class
/// word take the first sixteen bytes, and each property takes sixteen
/// after them.
pub(crate) fn prop_offset(index: u32) -> u32 {
    16 + 16 * index
}

/// The row word the trace left for `entity`, read the way the scan
/// reads it — through the block's own shadow pointer. A meeting would
/// answer too, and would be the wrong instrument: it initialises a row
/// the trace never reached, so a test built on it cannot tell an
/// untouched row from a met one.
///
/// # Safety
/// `entity` is a live entity of the GC heap whose block this collection
/// has touched.
pub(crate) unsafe fn row_word(entity: *mut RcHeader) -> u32 {
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

/// The colour the trace left for `entity`: what the mark met, or the
/// verdict the scan wrote over it.
///
/// # Safety
/// As [`row_word`].
pub(crate) unsafe fn row_colour(entity: *mut RcHeader) -> Colour {
    shadow::colour(unsafe { row_word(entity) })
}
