use super::*;
use crate::array::table::Key;
use crate::refcount::ll_release;
use crate::string::{LLString, ll_string_new};

enum Shape {
    NestedThenObject,
    TwoNested,
    Mixed,
}

static DESTRUCTOR_ORDER: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// A class whose destructor appends one character to
/// [`DESTRUCTOR_ORDER`]. Each expansion is a distinct function item,
/// which is what makes the character identify the object.
macro_rules! recording_class {
    ($name:literal, $mark:literal) => {{
        unsafe extern "C" fn record(_o: *mut crate::object::Object) {
            DESTRUCTOR_ORDER.lock().unwrap().push($mark);
        }

        crate::class::ClassBuilder::new($name)
            .destructor(record as *const ())
            .build()
    }};
}

/// The same, with the drain's list refused for the teardown alone —
/// the flag is raised after the shape is built, because it also
/// refuses the storage every array here allocates.
fn destructor_order_with(shape: Shape, refuse_the_list: bool) -> String {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTOR_ORDER.lock().unwrap().clear();
    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext {
        arena: &mut arena as *mut _,
    };

    let ctx_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    // Mounted although every entity here is built with `ctx_ptr` in
    // hand: a `__destruct` is user code and resolves its own context
    // from TLS, which is the shape generated code has.
    crate::memory::context::set_current_context(ctx_ptr);

    let array = || unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let put = |owner: *mut LLArray, key: i64, tag: crate::value::Tag, child: *mut RcHeader| unsafe {
        crate::array::testing::insert(owner, Key::Int(key), Value::entity(tag, child));
    };

    let object = |cls| unsafe {
        crate::object::new_constructed(ctx_ptr, cls, MemoryCategory::GcHeap) as *mut RcHeader
    };

    let root = array();
    match shape {
        Shape::NestedThenObject => {
            let nested = array();
            put(
                nested,
                0,
                crate::value::Tag::Object,
                object(recording_class!("B", 'B')),
            );
            put(root, 0, crate::value::Tag::Array, nested as *mut RcHeader);
            put(
                root,
                1,
                crate::value::Tag::Object,
                object(recording_class!("A", 'A')),
            );
        }
        Shape::TwoNested => {
            let first = array();
            put(
                first,
                0,
                crate::value::Tag::Object,
                object(recording_class!("B", 'B')),
            );
            let second = array();
            put(
                second,
                0,
                crate::value::Tag::Object,
                object(recording_class!("C", 'C')),
            );
            put(root, 0, crate::value::Tag::Array, first as *mut RcHeader);
            put(root, 1, crate::value::Tag::Array, second as *mut RcHeader);
        }
        Shape::Mixed => {
            let innermost = array();
            put(
                innermost,
                0,
                crate::value::Tag::Object,
                object(recording_class!("Three", '3')),
            );
            let middle = array();
            put(
                middle,
                0,
                crate::value::Tag::Object,
                object(recording_class!("Two", '2')),
            );
            put(
                middle,
                1,
                crate::value::Tag::Array,
                innermost as *mut RcHeader,
            );
            put(
                middle,
                2,
                crate::value::Tag::Object,
                object(recording_class!("Four", '4')),
            );
            put(
                root,
                0,
                crate::value::Tag::Object,
                object(recording_class!("One", '1')),
            );
            put(root, 1, crate::value::Tag::Array, middle as *mut RcHeader);
            put(
                root,
                2,
                crate::value::Tag::Object,
                object(recording_class!("Five", '5')),
            );
        }
    }

    if refuse_the_list {
        crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    unsafe {
        assert!(ll_release(root as *mut RcHeader), "the root had a holder");
        crate::object::ll_entity_die(root as *mut RcHeader);
    }

    crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED
        .store(false, std::sync::atomic::Ordering::Relaxed);
    crate::memory::context::set_current_context(std::ptr::null_mut());
    let written = DESTRUCTOR_ORDER.lock().unwrap().clone();
    written
}

fn mk(bytes: &[u8]) -> *mut LLString {
    unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) }
}

fn arr() -> *mut LLArray {
    unsafe { ll_array_new(MemoryCategory::GcHeap) }
}

mod nesting_worked_through_a_list;
mod the_entity_around_the_table;
mod the_flood_state_a_copy_inherits;
mod the_head_a_walker_reads;
mod the_order_destructors_run_in;
mod the_two_cow_doors;
mod what_a_death_gives_back;
mod who_owns_a_key_reference;
