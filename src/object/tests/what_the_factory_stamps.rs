//! A fresh object carries its header, its class and one starting
//! state per slot: a defaultless Box slot starts undefined and a
//! bitmap-tracked raw slot starts with its bit clear, which is what
//! tells an uninitialized slot from one holding null. The
//! construct-into-a-reserved-cell door shares the stamp, and the C
//! entry point takes the category as a `u32`.

use super::*;

#[test]
fn new_stamps_header_class_and_null_props() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Plain").prop("x", true).build();

    with_ctx(|ctx| {
        let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::RequestArena) };
        let o = unsafe { &mut *obj };
        assert_eq!(unsafe { crate::refcount::entity_refcount(obj) }, 1);
        assert_eq!(
            unsafe { crate::refcount::entity_category(obj) },
            MemoryCategory::RequestArena
        );
        assert_eq!(o.class, cls);
        assert_eq!(
            unsafe { crate::refcount::entity_flags(obj) } & DESTRUCTOR_PENDING,
            0,
            "no destructor declared"
        );
        let x = unsafe { Object::prop_at(obj, 16).read() };
        assert_eq!(x.tag(), Tag::Null);
    });
}

/// A5: a defaultless `mixed` Box slot starts *undefined* — the factory
/// stamps `VALUE_UNDEF` from the descriptor's undef runs after the
/// zero-fill — while a defaulted one starts `null`. Undef is invisible
/// to the trace walk (the refcounted flag is clear), any store clears
/// it (the barrier writes all 16 bytes), and `unset()` is the
/// undef-store + `drop_ref` composition, which restores the state and
/// releases the displaced entity.
#[test]
fn defaultless_box_slot_lives_the_undef_lifecycle() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("A5Undef")
        .prop("defaulted", true) // Boxed with a default: starts null
        .prop_boxed_without_default("bare") // starts undef
        .build();
    let child_cls = ClassBuilder::new("A5Child").build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    unsafe {
        let obj = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
        let defaulted = Object::prop_at(obj, 16);
        let bare = Object::prop_at(obj, 32);

        assert!(
            !defaulted.read().is_undef(),
            "a default means never tracked"
        );
        assert_eq!(defaulted.read().tag(), Tag::Null);
        assert!(bare.read().is_undef(), "stamped by the factory");

        // Undef is not traced: the walk sees no children yet.
        let mut children = 0;
        for_each_counted_child(obj, |_| children += 1);
        assert_eq!(children, 0, "an undef slot must not be walked");

        // Any store clears undef — the whole 16 bytes are written.
        let child = new_constructed(&mut ctx, child_cls, MemoryCategory::GcHeap);
        assert!(crate::memory::barrier::ref_store(
            &mut arena,
            obj as *mut RcHeader,
            bare,
            std::ptr::null_mut(),
            Value::entity(crate::value::Tag::Object, child as *mut RcHeader),
        ));
        assert!(!bare.read().is_undef());
        assert_eq!(
            crate::refcount::entity_refcount(child),
            2,
            "creation + the slot"
        );
        let mut children = 0;
        for_each_counted_child(obj, |_| children += 1);
        assert_eq!(children, 1, "a stored entity is walked again");

        // `unset($obj->bare)`: store undef back, drop the displaced
        // entity — the same publish-then-release order as any
        // overwriting store.
        assert!(crate::memory::barrier::ref_store(
            &mut arena,
            obj as *mut RcHeader,
            bare,
            child as *mut RcHeader,
            Value::undef(),
        ));
        assert!(bare.read().is_undef(), "unset returns the slot to undef");
        assert_eq!(
            crate::refcount::entity_refcount(child),
            1,
            "the slot's reference released"
        );
        let mut children = 0;
        for_each_counted_child(obj, |_| children += 1);
        assert_eq!(children, 0);

        // Teardown strides the same runs: the undef slot releases
        // nothing, and both die cleanly.
        for entity in [child as *mut RcHeader, obj as *mut RcHeader] {
            assert!(crate::refcount::ll_release(entity));
            ll_entity_die(entity);
        }
    }

    arena.reset(|_| {});
}

/// A5 commit 2: raw slots with no marker of their own — a defaultless
/// `?T` pointer (`NULL` is PHP null there) and a defaultless scalar —
/// are tracked by the init bitmap in the byte block. The factory's
/// zero-fill starts every bit clear (uninitialized); a write sets the
/// bit beside the value store; `unset()` clears it, for the pointer
/// slot together with the NULL store + drop of the displaced entity.
#[test]
fn bitmap_tracked_raw_slots_live_the_init_lifecycle() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("A5Bitmap")
        .prop_nullable_pointer_without_default("p") // @16, run member
        .prop_scalar_without_default("n") // @24; block byte 32
        .build();
    let child_cls = ClassBuilder::new("A5BitmapChild").build();

    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    unsafe {
        let obj = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
        let p_bit = (*cls)
            .find_prop(crate::intern::intern_str("p"))
            .unwrap()
            .init_bit;
        let n_bit = (*cls)
            .find_prop(crate::intern::intern_str("n"))
            .unwrap()
            .init_bit;

        // The zero-fill made both uninitialized, no explicit store.
        assert!(!Object::init_bit_test(obj, p_bit));
        assert!(!Object::init_bit_test(obj, n_bit));

        // $obj->n = 42: the value store plus the bit set.
        let n_slot = (obj as *mut u8).add(24) as *mut i64;
        n_slot.write(42);
        Object::init_bit_set(obj, n_bit);
        assert!(Object::init_bit_test(obj, n_bit));
        assert!(!Object::init_bit_test(obj, p_bit), "bits are independent");

        // $obj->p = $child: the barrier's pointer store + the bit set.
        let child = new_constructed(&mut ctx, child_cls, MemoryCategory::GcHeap);
        let p_slot = (obj as *mut u8).add(16) as *mut *mut RcHeader;
        assert!(crate::memory::barrier::store_ptr(
            &mut arena,
            MemoryCategory::GcHeap,
            p_slot,
            child as *mut RcHeader,
        ));
        Object::init_bit_set(obj, p_bit);
        assert_eq!(
            crate::refcount::entity_refcount(child),
            2,
            "creation + the slot"
        );

        // A walked child now; the bitmap never affects the trace.
        let mut children = 0;
        for_each_counted_child(obj, |_| children += 1);
        assert_eq!(children, 1);

        // $obj->p = null: a real null for `?T` — the slot goes back to
        // NULL, the displaced child is dropped, and the bit STAYS set:
        // the bit, not the pointer, answers isset.
        assert!(crate::memory::barrier::store_ptr(
            &mut arena,
            MemoryCategory::GcHeap,
            p_slot,
            std::ptr::null_mut(),
        ));
        crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, child as *mut RcHeader);
        assert!(
            Object::init_bit_test(obj, p_bit),
            "null is a value, still initialized"
        );
        assert_eq!(
            crate::refcount::entity_refcount(child),
            1,
            "the slot's reference released"
        );

        // unset($obj->p) / unset($obj->n): back to uninitialized. The
        // pointer slot is already NULL; a raw slot has only the bit.
        Object::init_bit_clear(obj, p_bit);
        Object::init_bit_clear(obj, n_bit);
        assert!(!Object::init_bit_test(obj, p_bit));
        assert!(!Object::init_bit_test(obj, n_bit));

        for entity in [child as *mut RcHeader, obj as *mut RcHeader] {
            assert!(crate::refcount::ll_release(entity));
            ll_entity_die(entity);
        }
    }

    arena.reset(|_| {});
}

/// The construct-into-a-reserved-cell path shares `stamp_into`, so it
/// stamps undef the same way the allocating factory does.
#[test]
fn object_new_in_a_reserved_cell_stamps_undef_too() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("A5UndefInCell")
        .prop_boxed_without_default("bare")
        .build();

    unsafe {
        let mut cell: *mut u8 = std::ptr::null_mut();
        let mut contiguous = 0usize;
        let got = crate::memory::heap::ll_entity_reserve(
            (*cls).object_size as usize,
            1,
            &mut cell,
            &mut contiguous,
        );
        assert_eq!(got, 1, "one cell for the test object");
        let obj = ll_object_new_in(cell, cls);
        assert!(Object::prop_at(obj, 16).read().is_undef());
        assert!(crate::refcount::ll_release(obj as *mut RcHeader));
        ll_entity_die(obj as *mut RcHeader);
    }
}

#[test]
fn abi_object_new_takes_the_category_as_u32() {
    let _g = crate::memory::block_pool::test_guard();
    let cls = ClassBuilder::new("Plain").build();
    with_ctx(|ctx| {
        // As generated code passes it: the category is a raw u32.
        let obj = unsafe { ll_object_new_abi(ctx, cls, MemoryCategory::RequestArena as u32) };
        assert_eq!(
            unsafe { crate::refcount::entity_category(obj) },
            MemoryCategory::RequestArena
        );
    });
}
