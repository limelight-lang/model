use super::*;
use crate::intern::intern_str;

extern "C" fn m1() {}

extern "C" fn m2() {}

extern "C" fn m2_override() {}

extern "C" fn m3() {}

fn base() -> *const Class {
    ClassBuilder::new("Animal")
        .prop("name", true)
        .prop("age", false)
        .method("speak", m1 as *const ())
        .method("eat", m2 as *const ())
        .build()
}

/// An offset comes from the slot's kind and from the run it is
/// grouped into, so declaration order survives the physical
/// regrouping. A subclass appends after the parent's layout and
/// falls into its tail padding, the JDK-15 rule, which is why
/// `layout_end` and `object_size` are two numbers.
mod where_a_property_lands {
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
}

/// A slot with no marker of its own — a defaultless `?T` pointer, a
/// scalar, a bool — carries one bit in the byte block at the tail,
/// numbered in declaration order, and `init_bit` is absolute, so the
/// ninth crosses into a second byte. A subclass gets a block of its
/// own because the parent's bit positions are compiled into parent
/// code.
mod the_init_bitmap_at_the_layout_tail {
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
}

/// A method keeps its slot down the hierarchy, so an override is
/// written in place rather than appended, and the destructor slot
/// inherits with it. The interface tables live in the descriptor's
/// own tail, and `instanceof` is a Cohen display read. Three of
/// these compare a function against the pointer built from it, which
/// Miri cannot model — it gives one function several addresses.
mod what_the_descriptor_dispatches_through {
    use super::*;

    /// Not runnable under Miri: it compares vtable entries against the
    /// functions they should hold, and Miri does not give a function a
    /// single address — `m2 as *const ()` yields a different pointer at
    /// the builder's cast site than at the assertion's. The dispatch
    /// tables round-trip exactly (verified by reading back what was
    /// written); it is function *identity* Miri cannot model. On real
    /// targets a function has one address, so this runs normally under
    /// `cargo test` and the contract is unweakened.
    #[test]
    #[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
    fn slots_are_stable_and_overrides_land_in_place() {
        let _g = crate::memory::block_pool::test_guard();
        let animal = base();
        let dog = ClassBuilder::new("Dog")
            .parent(animal)
            .method("eat", m2_override as *const ())
            .method("fetch", m3 as *const ())
            .build();

        let (animal_ptr, dog_ptr) = (animal, dog);
        let (animal, dog) = unsafe { (&*animal, &*dog) };
        let eat = intern_str("eat");
        let speak = intern_str("speak");

        assert_eq!(animal.find_method(eat), dog.find_method(eat), "slot stable");
        assert_eq!(animal.find_method(speak), dog.find_method(speak));

        let slot = dog.find_method(eat).unwrap() as usize;
        assert_eq!(unsafe { Class::vtbl(animal_ptr) }[slot], m2 as *const ());
        assert_eq!(
            unsafe { Class::vtbl(dog_ptr) }[slot],
            m2_override as *const (),
            "override in place"
        );
        assert_eq!(dog.vtbl_len, animal.vtbl_len + 1, "fetch appended");
    }

    /// Miri-ignored for the same reason as
    /// [`slots_are_stable_and_overrides_land_in_place`]: function identity.
    #[test]
    #[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
    fn inherited_itable_sees_the_override() {
        let _g = crate::memory::block_pool::test_guard();
        let feedable = ClassBuilder::interface("Feedable");

        let animal = ClassBuilder::new("Animal")
            .method("eat", m2 as *const ())
            .implement(unsafe { &*feedable }, vec![0]) // interface slot 0 → vtbl slot 0 (eat)
            .build();
        let dog = ClassBuilder::new("Dog")
            .parent(animal)
            .method("eat", m2_override as *const ())
            .build();

        let id = unsafe { (*feedable).interface_id };
        let animal_it = unsafe { ll_find_itable(animal, id) };
        let dog_it = unsafe { ll_find_itable(dog, id) };
        assert!(!animal_it.is_null() && !dog_it.is_null());
        unsafe {
            assert_eq!(*animal_it, m2 as *const ());
            assert_eq!(
                *dog_it, m2_override as *const (),
                "inherited itable must be re-linked against the subclass vtable"
            );
        }

        let missing = unsafe { ll_find_itable(animal, 9999) };
        assert!(missing.is_null());
    }

    /// Miri-ignored for the same reason as
    /// [`slots_are_stable_and_overrides_land_in_place`]: function identity.
    #[test]
    #[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
    fn itables_ride_the_descriptor_tail() {
        let _g = crate::memory::block_pool::test_guard();
        let i1 = ClassBuilder::interface("A");
        let i2 = ClassBuilder::interface("B");

        let cls = ClassBuilder::new("Train")
            .method("x", m1 as *const ())
            .method("y", m2 as *const ())
            .implement(unsafe { &*i1 }, vec![0, 1])
            .implement(unsafe { &*i2 }, vec![1])
            .build();

        let c = unsafe { &*cls };
        let tail_start = cls as usize + size_of::<Class>();
        let vtbl_end = tail_start + c.vtbl_len as usize * 8;
        let train_end = vtbl_end + 3 * 8; // 2 + 1 itable entries

        let (id1, id2) = unsafe { ((*i1).interface_id, (*i2).interface_id) };
        let t1 = unsafe { ll_find_itable(cls, id1) } as usize;
        let t2 = unsafe { ll_find_itable(cls, id2) } as usize;
        assert!(
            (vtbl_end..train_end).contains(&t1) && (vtbl_end..train_end).contains(&t2),
            "itables must live in the descriptor's own tail"
        );
        unsafe {
            assert_eq!(*(t1 as *const *const ()), m1 as *const ());
            assert_eq!(*(t2 as *const *const ()), m2 as *const ());
        }
    }

    #[test]
    fn destructor_slot_is_tracked_through_inheritance() {
        let _g = crate::memory::block_pool::test_guard();
        let animal = ClassBuilder::new("Animal")
            .destructor(m1 as *const ())
            .build();
        let dog = ClassBuilder::new("Dog").parent(animal).build();

        let (ca, cd) = unsafe { (&*animal, &*dog) };
        assert!(ca.has_destructor());
        assert!(cd.has_destructor(), "destructor presence inherits");
        assert_eq!(ca.destruct_slot, cd.destruct_slot);
        assert_ne!(ca.destruct_slot, NO_DESTRUCT_SLOT);
    }

    #[test]
    fn cohen_display_answers_instanceof_in_o1() {
        let _g = crate::memory::block_pool::test_guard();
        let a = base();
        let b = ClassBuilder::new("Dog").parent(a).build();
        let c = ClassBuilder::new("Puppy").parent(b).build();
        let other = ClassBuilder::new("Rock").build();

        let (ca, cb, cc, co) = unsafe { (&*a, &*b, &*c, &*other) };
        assert!(cc.instance_of_class(ca));
        assert!(cc.instance_of_class(cb));
        assert!(cc.instance_of_class(cc));
        assert!(cb.instance_of_class(ca));
        assert!(!ca.instance_of_class(cb), "parent is not a child");
        assert!(!co.instance_of_class(ca));
        assert_eq!(cc.display_len, 3);
    }
}
