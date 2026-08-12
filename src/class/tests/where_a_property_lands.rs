//! An offset comes from the slot's kind and from the run it is
//! grouped into, so declaration order survives the physical
//! regrouping. A subclass appends after the parent's layout and
//! falls into its tail padding, the JDK-15 rule, which is why
//! `layout_end` and `object_size` are two numbers.

use super::*;

#[test]
fn property_offsets_inherit_and_append() {
    let _g = crate::memory::block_pool::test_guard();
    // `parent` takes the raw descriptor pointer: handing it a `&Class`
    // would narrow provenance to the fixed fields, and `build` reads
    // the parent's trailing vtable.
    let animal_ptr = base();
    let dog_ptr = ClassBuilder::new("Dog")
        .parent(animal_ptr)
        .prop("breed", true)
        .build();
    let (animal, dog) = unsafe { (&*animal_ptr, &*dog_ptr) };

    // name is a Boxed slot (the `refcounted` shim), age a Scalar: the
    // Box run is placed first, the scalar after it, and object_size is
    // the exact byte count, not a uniform per-slot size.
    assert_eq!(animal.find_prop(intern_str("name")).unwrap().offset, 16);
    assert_eq!(animal.find_prop(intern_str("age")).unwrap().offset, 32);
    assert_eq!(animal.layout_end, 40);
    assert_eq!(animal.object_size, 40);

    // Inherited offsets are unchanged; Dog's own box slot resumes at
    // the parent's layout_end (40).
    assert_eq!(dog.find_prop(intern_str("name")).unwrap().offset, 16);
    assert_eq!(dog.find_prop(intern_str("age")).unwrap().offset, 32);
    assert_eq!(dog.find_prop(intern_str("breed")).unwrap().offset, 40);
    assert_eq!(dog.object_size, 56);

    // The trace map: two Box runs (parent's name, then Dog's breed),
    // no pointer runs. The scalar `age` is in neither — never traced.
    assert_eq!(
        dog.box_runs(),
        &[
            Run {
                offset: 16,
                count: 1
            },
            Run {
                offset: 40,
                count: 1
            }
        ],
        "age is scalar-shaped, not traced"
    );
    assert!(dog.ptr_runs().is_empty());
}

/// The full slot-kind spread: a bare pointer (traced, stride-8 run), a
/// Box (traced, stride-16 run), a scalar and a bool (never traced),
/// each at the offset its kind and the run grouping dictate. Also pins
/// declaration order surviving the physical regrouping, and the exact
/// `layout_end` / `object_size` split that a subclass builds on.
#[test]
fn slot_kinds_lay_out_in_three_runs() {
    let _g = crate::memory::block_pool::test_guard();
    let cls_ptr = ClassBuilder::new("Mixed")
        .prop_pointer("next") // Pointer, 8
        .prop("data", true) // Boxed, 16
        .prop("id", false) // Scalar, 8
        .prop_bool("ok") // Bool, 1
        .build();
    let cls = unsafe { &*cls_ptr };

    // Pointers first, then Boxes, then the rest in declaration order.
    assert_eq!(cls.find_prop(intern_str("next")).unwrap().offset, 16);
    assert_eq!(cls.find_prop(intern_str("data")).unwrap().offset, 24);
    assert_eq!(cls.find_prop(intern_str("id")).unwrap().offset, 40);
    assert_eq!(cls.find_prop(intern_str("ok")).unwrap().offset, 48);

    // One pointer run and one Box run; the scalar and the bool are in
    // neither.
    assert_eq!(
        cls.ptr_runs(),
        &[Run {
            offset: 16,
            count: 1
        }]
    );
    assert_eq!(
        cls.box_runs(),
        &[Run {
            offset: 24,
            count: 1
        }]
    );

    // Slot kinds are recorded per property.
    assert_eq!(
        cls.find_prop(intern_str("next")).unwrap().kind,
        SlotKind::Pointer
    );
    assert_eq!(
        cls.find_prop(intern_str("id")).unwrap().kind,
        SlotKind::Scalar
    );
    assert_eq!(
        cls.find_prop(intern_str("ok")).unwrap().kind,
        SlotKind::Bool
    );

    // Declaration order is preserved though physical order regrouped.
    assert_eq!(
        cls.find_prop(intern_str("next")).unwrap().declaration_index,
        0
    );
    assert_eq!(
        cls.find_prop(intern_str("data")).unwrap().declaration_index,
        1
    );
    assert_eq!(
        cls.find_prop(intern_str("id")).unwrap().declaration_index,
        2
    );
    assert_eq!(
        cls.find_prop(intern_str("ok")).unwrap().declaration_index,
        3
    );

    // The bool ends the layout mid-word: layout_end is unrounded (49),
    // object_size is it rounded up to 8 (56), leaving 7 bytes of tail.
    assert_eq!(cls.layout_end, 49);
    assert_eq!(cls.object_size, 56);
}

/// A defaultless Box slot is regrouped behind the defaulted ones so
/// the factory's undef stamp is one contiguous sub-run at the box
/// run's tail, and a subclass appends its own undef run beside the
/// inherited one — the same shape as the trace runs.
#[test]
fn defaultless_boxes_group_at_the_box_runs_tail_and_inherit() {
    let _g = crate::memory::block_pool::test_guard();
    let base_ptr = ClassBuilder::new("UndefBase")
        .prop_boxed_without_default("bare") // declared first,
        .prop("defaulted", true) // laid out last: defaulted@16, bare@32
        .build();
    let sub_ptr = ClassBuilder::new("UndefSub")
        .parent(base_ptr)
        .prop_boxed_without_default("own_bare")
        .build();
    let (base, sub) = unsafe { (&*base_ptr, &*sub_ptr) };

    assert_eq!(base.find_prop(intern_str("defaulted")).unwrap().offset, 16);
    assert_eq!(base.find_prop(intern_str("bare")).unwrap().offset, 32);
    assert_eq!(
        base.box_runs(),
        &[Run {
            offset: 16,
            count: 2
        }]
    );
    assert_eq!(
        base.undef_runs(),
        &[Run {
            offset: 32,
            count: 1
        }],
        "the defaultless tail of the box run"
    );

    // Declaration order survives the regrouping.
    assert_eq!(
        base.find_prop(intern_str("bare"))
            .unwrap()
            .declaration_index,
        0
    );
    assert_eq!(
        base.find_prop(intern_str("defaulted"))
            .unwrap()
            .declaration_index,
        1
    );

    // The subclass inherits the parent's undef run and appends its own.
    assert_eq!(sub.find_prop(intern_str("own_bare")).unwrap().offset, 48);
    assert_eq!(
        sub.box_runs(),
        &[
            Run {
                offset: 16,
                count: 2
            },
            Run {
                offset: 48,
                count: 1
            }
        ]
    );
    assert_eq!(
        sub.undef_runs(),
        &[
            Run {
                offset: 32,
                count: 1
            },
            Run {
                offset: 48,
                count: 1
            }
        ]
    );
}

/// A subclass field falls into the parent's tail padding (JDK-15 rule):
/// `Mixed` ends at layout_end 49 with object_size 56, so a 1-byte bool
/// added by a subclass lands at 49 and the object does not grow.
#[test]
fn subclass_reuses_the_parents_tail_padding() {
    let _g = crate::memory::block_pool::test_guard();
    let parent = ClassBuilder::new("Mixed2")
        .prop_pointer("next")
        .prop("data", true)
        .prop("id", false)
        .prop_bool("ok")
        .build();
    let sub = ClassBuilder::new("Sub")
        .parent(parent)
        .prop_bool("flag")
        .build();
    let sub = unsafe { &*sub };

    // flag sits inside the parent's [49, 56) padding, so object_size
    // stays 56 — the field cost nothing in allocation.
    assert_eq!(sub.find_prop(intern_str("flag")).unwrap().offset, 49);
    assert_eq!(sub.layout_end, 50);
    assert_eq!(
        sub.object_size, 56,
        "own field fit in the parent's tail padding"
    );
}
