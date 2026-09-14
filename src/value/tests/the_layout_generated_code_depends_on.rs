//! Size, offsets, the tag word's flag bits and the sample boxes are ABI:
//! compiled PHP reads them as constants and a collector reads the +8
//! word alone, so they are pinned by value and not only by name.

use super::*;

#[test]
fn box_is_16_bytes_with_fixed_offsets() {
    assert_eq!(size_of::<Value>(), 16);
    assert_eq!(core::mem::offset_of!(Value, w0), 0);
    assert_eq!(core::mem::offset_of!(Value, w8), DISCRIMINATING_WORD_OFFSET);
    assert_eq!(DISCRIMINATING_WORD_OFFSET, 8);
}

/// The tag word's flag bits are ABI (generated code tests them by
/// constant), so the values are pinned, not just the names.
#[test]
fn flag_bits_are_pinned_and_undef_is_not_null() {
    assert_eq!(TAG_WORD_BIT, 1);
    assert_eq!(TAG_WORD_UNDEF, 2);

    let u = Value::undef();
    assert!(u.is_undef());
    assert!(!u.is_pointer(), "undef is never traced or counted");
    assert!(
        !Value::null().is_undef(),
        "an all-zero box is null, not undef"
    );
}

/// The two machine words of a box, as a slot holds them and a collector
/// reads them.
fn words(v: Value) -> [u64; 2] {
    unsafe { core::mem::transmute::<Value, [u64; 2]>(v) }
}

/// The sample boxes of `rfc/model/values.md`, "ValueBox Layout", byte for
/// byte: +0 is the immediate value or, on the pointer arm, the tag word;
/// +8 is the counted pointer or the tag word with bit 0 set. Pinned by
/// value rather than through the accessors because a collector on another
/// thread decides from the +8 word alone, and a box the constructors built
/// any other way is one it would misread.
#[test]
fn the_sample_boxes_of_the_layout_byte_for_byte() {
    let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
    let p = &raw mut e;
    assert_eq!(words(Value::int(42)), [42, 0x0301]);
    assert_eq!(words(Value::float(2.5)), [2.5f64.to_bits(), 0x0401]);
    assert_eq!(words(Value::bool(false)), [0, 0x0101]);
    assert_eq!(words(Value::bool(true)), [0, 0x0201]);
    assert_eq!(
        words(Value::null()),
        [0, 0],
        "the barrier's null is all-zero"
    );
    assert_eq!(
        words(Value::undef()),
        [0, 0x0003],
        "a Null tag word with the undef bit"
    );
    assert_eq!(words(Value::entity(Tag::String, p)), [0x0501, p as u64]);
    assert_eq!(words(Value::entity(Tag::Object, p)), [0x0701, p as u64]);
    assert_eq!(words(Value::entity(Tag::Reference, p)), [0x0901, p as u64]);
}

/// `tag()` lists the codes by hand, so a tag added to the enum and not to
/// the decode would abort at its first read; every tag goes through the
/// decode here, on the arm it belongs to.
#[test]
fn every_tag_decodes_to_itself() {
    let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
    let p = &raw mut e;
    let immediate = [Tag::Null, Tag::False, Tag::True, Tag::Int, Tag::Float];
    let pointer = [
        Tag::String,
        Tag::Array,
        Tag::Object,
        Tag::Resource,
        Tag::Reference,
    ];
    for tag in immediate {
        assert_eq!(
            Value::from_words([0, 0x0001 | (tag as u64) << 8]).tag(),
            tag
        );
    }

    for tag in pointer {
        assert_eq!(Value::entity(tag, p).tag(), tag);
    }
}
