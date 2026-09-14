//! The reader of a sixteen-byte box cell decides by the +8 word alone:
//! a non-zero word with bit 0 clear is the counted child, and anything
//! else — zero, a tag word, a container's null — is no child, whatever
//! +0 holds (`rfc/model/values.md`, "ValueBox Layout").

use super::*;

/// A box cell as two words, aligned as a slot is.
#[repr(C, align(8))]
struct Slot([u64; 2]);

fn child_of(slot: &Slot) -> Option<Cell> {
    unsafe { counted_box_cell::<PlainCells>(slot.0.as_ptr() as *const u8) }
}

#[test]
fn the_box_reader_decides_by_the_discriminating_word_alone() {
    let mut h = RcHeader::new(MemoryCategory::GcHeap, 0);
    let p = &raw mut h;

    let pointer = Slot([0x0701, p as u64]);
    let cell = child_of(&pointer).expect("a pointer-arm box holds a counted child");
    assert_eq!(cell.child, p);
    assert_eq!(cell.shape, CellShape::Box);
    assert_eq!(cell.addr, pointer.0.as_ptr() as usize);

    let int = Slot([42, 0x0301]);
    assert!(child_of(&int).is_none(), "an immediate box holds no child");

    // +0 spells a pointer arm's tag word and +8 an immediate one's: the
    // integer 1793, and the +8 word says so.
    let odd_looking_int = Slot([0x0701, 0x0301]);
    assert!(child_of(&odd_looking_int).is_none());

    let null = Slot([0, 0]);
    assert!(child_of(&null).is_none());

    let container_null = Slot([0, 0x0001 | 5 << 32]);
    assert!(child_of(&container_null).is_none());

    let undef = Slot([0, 0x0003]);
    assert!(child_of(&undef).is_none());
}
