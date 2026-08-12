use super::*;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::context::LLContext;
use crate::object::{Object, new_constructed};
use crate::value::{Tag, Value};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A static block is a bare allocation with an object's layout and
/// no header — exactly what the compiler will emit for a class's
/// `static` properties.
fn static_block(layout: *const Class) -> *mut u8 {
    let size = unsafe { (*layout).object_size } as usize;
    let p =
        unsafe { std::alloc::alloc_zeroed(std::alloc::Layout::from_size_align(size, 16).unwrap()) };

    assert!(!p.is_null());
    p
}

unsafe fn free_static_block(p: *mut u8, layout: *const Class) {
    let size = unsafe { (*layout).object_size } as usize;
    unsafe { std::alloc::dealloc(p, std::alloc::Layout::from_size_align(size, 16).unwrap()) };
}

mod the_order_within_the_pass;
mod the_pass_nobody_calls_by_hand;
mod what_the_exit_pass_gives_back;
