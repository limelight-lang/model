use super::*;
use crate::refcount::MemoryCategory;

/// Size, offsets and flag-bit values are ABI: compiled PHP reads them
/// as constants, so they are pinned by value and not only by name.
mod the_layout_generated_code_depends_on {
    use super::*;

    #[test]
    fn box_is_16_bytes_with_fixed_offsets() {
        assert_eq!(size_of::<Value>(), 16);
        assert_eq!(core::mem::offset_of!(Value, payload), 0);
        assert_eq!(core::mem::offset_of!(Value, tag), 8);
        assert_eq!(core::mem::offset_of!(Value, flags), 9);
    }

    /// The flags byte's bit assignments are ABI (generated code tests
    /// them by constant), so the values are pinned, not just the names.
    #[test]
    fn flag_bits_are_pinned_and_undef_is_not_null() {
        assert_eq!(VALUE_REFCOUNTED, 1);
        assert_eq!(VALUE_UNDEF, 2);
        assert_eq!(VALUE_WRITING, 4);

        let u = Value::undef();
        assert!(u.is_undef());
        assert!(!u.is_refcounted(), "undef is never traced or counted");
        assert!(
            !Value::null().is_undef(),
            "an all-zero Box is null, not undef"
        );
    }
}

/// Which payload the tag admits, when truth may read the payload at
/// all, and which boxes carry a reference the runtime must count.
mod what_a_box_means {
    use super::*;

    #[test]
    fn scalar_round_trips() {
        assert_eq!(Value::int(-42).as_int(), -42);
        assert_eq!(Value::float(2.5).as_float(), 2.5);
        assert_eq!(Value::int(i64::MIN).as_int(), i64::MIN, "full 64-bit ints");
    }

    #[test]
    fn truth_never_reads_payload_for_bool_tags() {
        assert_eq!(Value::null().is_truthy_tag(), Some(false));
        assert_eq!(Value::bool(false).is_truthy_tag(), Some(false));
        assert_eq!(Value::bool(true).is_truthy_tag(), Some(true));
        assert_eq!(Value::int(0).is_truthy_tag(), None, "ints decode payload");
    }

    #[test]
    fn entity_boxes_count_scalars_do_not() {
        let mut e = RcHeader::new(MemoryCategory::GcHeap, 0);
        let v = Value::entity(Tag::Object, &mut e);
        assert!(v.is_refcounted());
        unsafe { value_retain(&v) };
        assert_eq!(e.refcount, 2);
        assert!(!unsafe { value_release(&v) });
        assert_eq!(e.refcount, 1);

        let i = Value::int(7);
        assert!(!i.is_refcounted());
        assert!(!unsafe { value_release(&i) }, "scalars are no-ops");
    }
}
