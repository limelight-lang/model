use super::*;
use crate::array::entity::ll_array_new;
use crate::array::table::Key;
use crate::class::ClassBuilder;
use crate::memory::arena::Arena;
use crate::memory::block_pool::FORCE_OOM;
use crate::memory::stdapi::ll_free;
use crate::object::{Object, ll_object_die, new_constructed};
use crate::refcount::{MemoryCategory, ll_release};
use crate::string::{LLString, ll_string_new};
use crate::value::Value;
use std::sync::atomic::Ordering;

fn mk(bytes: &[u8]) -> *mut LLString {
    let s = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) };
    assert!(!s.is_null());
    s
}

/// What the first growth asks for: 16 index slots and 16 entries.
const DOUBLED_STORAGE_BYTES: usize = 576;

/// Eat every buffer-arena source that could serve `size` — warm
/// block tails and recycled holes left by earlier tests on this
/// thread — so the next such allocation must draw a pool block,
/// which the already-raised `FORCE_OOM` refuses. Without this a
/// forced refusal is a coin flip on test order: the source array's
/// own storage warms a 64 KiB block and the copy's fits its tail.
/// The fillers go back through the caller, after the assertion.
unsafe fn exhaust_buffer_sources(size: usize) -> Vec<(*mut u8, usize)> {
    let mut fillers = Vec::new();
    loop {
        let (p, granted) = crate::memory::buffer_arena::buffer_alloc_longlived_payload(size);
        if p.is_null() {
            break;
        }

        fillers.push((p, granted));
    }

    fillers
}

fn free_fillers(fillers: Vec<(*mut u8, usize)>) {
    for (p, granted) in fillers {
        unsafe { crate::memory::buffer_arena::buffer_free_longlived_payload(p, granted) };
    }
}

/// Eat every free entity slot that could serve an inline string of
/// `len` bytes, so the next such allocation must draw a pool block,
/// which the already-raised `FORCE_OOM` refuses. The buffer-arena
/// helper above cannot stand in for this: an entity comes from the
/// object heap, which that one never touches.
unsafe fn exhaust_string_entities(len: usize) -> Vec<*mut LLString> {
    let bytes = vec![b'x'; len];
    let mut fillers = Vec::new();
    loop {
        let s = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, &bytes) };
        if s.is_null() {
            break;
        }

        fillers.push(s);
    }

    fillers
}

fn free_string_fillers(fillers: Vec<*mut LLString>) {
    for s in fillers {
        free(s);
    }
}

/// A heap holder object with two array-slot props, both naming
/// `src`, which therefore reads as shared: the two-`$var` setup of
/// every criterion the groups taking it measure, built through the
/// real barrier.
unsafe fn two_holders(
    ctx: *mut crate::memory::context::LLContext,
    arena: *mut Arena,
    src: *mut LLArray,
) -> (*mut crate::object::Object, *mut Value, *mut Value) {
    let class = ClassBuilder::new("ElementHolder")
        .prop("a", true)
        .prop("b", true)
        .build();
    let h = unsafe { new_constructed(ctx, class, MemoryCategory::GcHeap) };
    let slot_a = unsafe { Object::prop_at(h, 16) };
    let slot_b = unsafe { Object::prop_at(h, 32) };
    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena,
            h as *mut RcHeader,
            slot_a,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        assert!(crate::memory::barrier::ref_store(
            arena,
            h as *mut RcHeader,
            slot_b,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, src as *mut RcHeader),
        ));
        // The creation reference goes: the two slots are the holders.
        ll_release(src as *mut RcHeader);
    }

    (h, slot_a, slot_b)
}

fn free(s: *mut LLString) {
    unsafe {
        (*s).rc.refcount = 0;
        ll_free(s as *mut u8);
    }
}

mod a_box_outliving_what_moves_the_entry;
mod an_element_in_a_reference_state;
mod crossing_out_of_the_arena;
mod the_key_a_spelling_means;
mod the_writes_and_the_separation_they_share;
mod what_a_copy_does_with_a_box;
mod what_a_refusal_leaves_behind;
