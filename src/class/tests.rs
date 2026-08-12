use super::*;
use crate::intern::intern_str;

extern "C" fn m1() {}

extern "C" fn m2() {}

fn base() -> *const Class {
    ClassBuilder::new("Animal")
        .prop("name", true)
        .prop("age", false)
        .method("speak", m1 as *const ())
        .method("eat", m2 as *const ())
        .build()
}

mod the_init_bitmap_at_the_layout_tail;
mod what_the_descriptor_dispatches_through;
mod where_a_property_lands;
