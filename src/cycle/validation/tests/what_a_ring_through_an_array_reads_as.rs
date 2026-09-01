//! A component whose members are of two kinds: the ring runs through an
//! array's element and back through an object's property.
//!
//! Every other test here judges plain objects, and the arithmetic goes
//! through `cells::trace_cells`, which strides each kind differently. An
//! array is the kind a real component is most likely to hold, and the
//! only one whose walk can give up on an incoherent head — which reads
//! as a lost in-edge, so the component acquits rather than frees.

use super::*;
use crate::array::entity::ll_array_new;
use crate::array::testing::push;
use crate::memory::barrier::ref_store;
use crate::object::ll_entity_die;
use crate::value::{Tag, Value};

#[test]
fn an_element_and_a_property_close_the_same_ring() {
    let _g = test_guard();
    let node = ClassBuilder::new("ExactArrayNode")
        .prop("next", true)
        .build();

    let mut arena = Arena::new();
    let mut context = LLContext { arena: &mut arena };
    let holder = unsafe { new_constructed(&mut context, node, MemoryCategory::GcHeap) };
    let array = unsafe { ll_array_new(MemoryCategory::GcHeap) };

    unsafe {
        let slot = Object::prop_at(holder, prop_offset(0));
        assert!(ref_store(
            &mut arena,
            holder as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));

        // `push` stores the word and counts nothing, so the element's
        // reference is retained here.
        ll_retain(holder as *mut RcHeader);
        assert!(push(
            array,
            Value::entity(Tag::Object, holder as *mut RcHeader)
        ));

        assert!(!ll_release(holder as *mut RcHeader));
        assert!(!ll_release(array as *mut RcHeader));
    }

    let mut shadow_arena = unsafe { traced_unreachable_from(holder, &[holder]) };
    assert_eq!(
        unsafe { row_color(array as *mut RcHeader) },
        Color::PotentiallyUnreachable,
        "the array is a member of the ring rather than an external holder"
    );
    shadow_arena.reset();

    let mut members = [holder as *mut RcHeader, array as *mut RcHeader];
    assert_eq!(
        unsafe { validate_component(&mut members, 0) },
        ValidationResult::Unreachable,
        "the property and the element are the only two references there are"
    );

    unsafe {
        ll_retain(holder as *mut RcHeader);
        ll_retain(array as *mut RcHeader);
        let slot = Object::prop_at(holder, prop_offset(0));
        assert!(ref_store(
            &mut arena,
            holder as *mut RcHeader,
            slot,
            array as *mut RcHeader,
            Value::null(),
        ));

        // The array's teardown releases the element, which is what puts
        // the holder back at the fixture's own reference.
        assert!(ll_release(array as *mut RcHeader));
        ll_entity_die(array as *mut RcHeader);
        assert!(ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}
