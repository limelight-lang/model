//! A slot with no marker of its own — a defaultless `?T` pointer, a
//! scalar, a bool — carries one bit in the byte block at the tail,
//! numbered in declaration order, and `init_bit` is absolute, so the
//! ninth crosses into a second byte. A subclass gets a block of its
//! own because the parent's bit positions are compiled into parent
//! code.

use super::*;

/// Bitmap-tracked raw slots (a defaultless `?T` pointer, scalar,
/// bool) get one init bit each in the byte block at the layout tail,
/// numbered in declaration order; slots with a marker of their own
/// (a default, or a non-nullable pointer's `NULL`) carry the sentinel.
#[test]
fn bitmap_tracked_slots_get_bits_in_a_tail_byte_block() {
    let _g = crate::memory::block_pool::test_guard();
    let cls_ptr = ClassBuilder::new("RawUninit")
        .prop_scalar_without_default("n") // decl 0 → bit 0
        .prop_nullable_pointer_without_default("p") // decl 1 → bit 1
        .prop("defaulted", false) // decl 2, untracked
        .prop_bool_without_default("b") // decl 3 → bit 2
        .prop_pointer("q") // decl 4, non-nullable: NULL is its marker
        .build();
    let cls = unsafe { &*cls_ptr };

    // Physical: the pointer run p,q at 16,24; then n@32, defaulted@40,
    // b@48; the byte block is the tail byte 49.
    assert_eq!(cls.find_prop(intern_str("p")).unwrap().offset, 16);
    assert_eq!(cls.find_prop(intern_str("q")).unwrap().offset, 24);
    assert_eq!(cls.find_prop(intern_str("n")).unwrap().offset, 32);
    assert_eq!(cls.find_prop(intern_str("b")).unwrap().offset, 48);
    assert_eq!(cls.layout_end, 50, "one bitmap byte after the bool");
    assert_eq!(cls.object_size, 56);

    // Bits in declaration order, as absolute positions in byte 49.
    assert_eq!(cls.find_prop(intern_str("n")).unwrap().init_bit, 49 * 8);
    assert_eq!(cls.find_prop(intern_str("p")).unwrap().init_bit, 49 * 8 + 1);
    assert_eq!(cls.find_prop(intern_str("b")).unwrap().init_bit, 49 * 8 + 2);
    assert_eq!(
        cls.find_prop(intern_str("defaulted")).unwrap().init_bit,
        NO_INIT_BIT
    );
    assert_eq!(
        cls.find_prop(intern_str("q")).unwrap().init_bit,
        NO_INIT_BIT
    );

    // The nullable pointer is an ordinary member of the pointer trace
    // run — the bit is metadata beside the slot, not a representation.
    assert_eq!(
        cls.ptr_runs(),
        &[Run {
            offset: 16,
            count: 2
        }]
    );
}

/// Nine tracked slots need two bitmap bytes; `init_bit` is an
/// absolute bit position, so the ninth lands in the second byte.
#[test]
fn a_ninth_tracked_slot_crosses_into_a_second_bitmap_byte() {
    let _g = crate::memory::block_pool::test_guard();
    let mut builder = ClassBuilder::new("WideBitmap");
    for i in 0..9 {
        builder = builder.prop_scalar_without_default(&format!("s{i}"));
    }

    let cls = unsafe { &*builder.build() };

    // Scalars at 16..88, block bytes 88-89.
    assert_eq!(cls.find_prop(intern_str("s0")).unwrap().init_bit, 88 * 8);
    assert_eq!(
        cls.find_prop(intern_str("s8")).unwrap().init_bit,
        88 * 8 + 8,
        "byte 89, bit 0"
    );
    assert_eq!(cls.layout_end, 90, "two bitmap bytes");
}

/// A subclass's own tracked slots get their own byte block: the
/// parent's bits do not move (parent offsets are compiled into parent
/// code), and a small block lands in the parent's tail padding.
#[test]
fn subclass_appends_its_own_byte_block() {
    let _g = crate::memory::block_pool::test_guard();
    let parent_ptr = ClassBuilder::new("RawUninitParent")
        .prop_scalar_without_default("n") // @16, bit in byte 25
        .prop_bool("flag") // @24: leaves the layout mid-word
        .build();
    let sub_ptr = ClassBuilder::new("RawUninitSub")
        .parent(parent_ptr)
        .prop_bool_without_default("b")
        .build();
    let (parent, sub) = unsafe { (&*parent_ptr, &*sub_ptr) };

    assert_eq!(parent.find_prop(intern_str("n")).unwrap().init_bit, 25 * 8);
    assert_eq!(parent.layout_end, 26);
    assert_eq!(parent.object_size, 32);

    // The subclass resumes at 26: its bool, then its own bitmap byte.
    assert_eq!(
        sub.find_prop(intern_str("n")).unwrap().init_bit,
        25 * 8,
        "parent bit unmoved"
    );
    assert_eq!(sub.find_prop(intern_str("b")).unwrap().offset, 26);
    assert_eq!(sub.find_prop(intern_str("b")).unwrap().init_bit, 27 * 8);
    assert_eq!(sub.layout_end, 28);
    assert_eq!(
        sub.object_size, 32,
        "slot and block fit in the parent's tail padding"
    );
}
