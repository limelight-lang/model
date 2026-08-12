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

/// A table's first storage: 8 index slots and 8 entries.
const FIRST_STORAGE_BYTES: usize = 288;

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
/// every criterion below, built through the real barrier.
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

/// `$a` with one integer element, `&$a[0]` taken or not and the
/// binding kept or dropped, then `$b = $a; $b[0] = 3;` — and what the
/// two names read afterwards. The whole of S3's criterion runs
/// through this, in both memory categories.
///
/// The holder is one object with two properties, so both arrays are
/// named by real slots and every write goes through the layer rather
/// than through the table.
unsafe fn reference_then_copy(
    ctx: *mut crate::memory::context::LLContext,
    arena: *mut Arena,
    category: MemoryCategory,
    take_reference: bool,
    keep_binding: bool,
) -> (i64, i64) {
    let class = ClassBuilder::new("RefCopyHolder")
        .prop("a", true)
        .prop("b", true)
        .build();
    let holder = unsafe { new_constructed(ctx, class, category) };
    let slot_a = unsafe { Object::prop_at(holder, 16) };
    let slot_b = unsafe { Object::prop_at(holder, 32) };
    let a = unsafe { ll_array_new(category) };
    unsafe {
        assert!(crate::memory::barrier::ref_store(
            arena,
            holder as *mut RcHeader,
            slot_a,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, a as *mut RcHeader),
        ));
        ll_release(a as *mut RcHeader);
        assert!(set(ctx, category, slot_a, Key::Int(0), Value::int(1)));

        if take_reference {
            let r = make_ref(ctx, category, slot_a, Key::Int(0));
            assert!(!r.is_null(), "the reference was refused");
            // The `$r` binding, taken and — unless it is kept — given
            // straight back, which is `unset($r)` and leaves the
            // element a reference with one holder (measured on php
            // 8.3.6: `unset` does not collapse the element).
            //
            // `GcHeap` is the binding's category whatever the array's
            // is, because `$r` is a frame slot rather than a container
            // in the arena: its reference is counted and given back
            // inside the request. Through an arena owner the release
            // would belong to the reset log, and `unset($r)` would not
            // take effect until the request ended.
            crate::refcount::ll_retain(r as *mut RcHeader);
            if !keep_binding {
                crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, r as *mut RcHeader);
            }
        }

        // `$b = $a`, then `$b[0] = 3`.
        assert!(crate::memory::barrier::ref_store(
            arena,
            holder as *mut RcHeader,
            slot_b,
            std::ptr::null_mut(),
            *slot_a,
        ));
        assert!(set(ctx, category, slot_b, Key::Int(0), Value::int(3)));

        let read_a = get(slot_a, Key::Int(0)).expect("the key is there").as_int();
        let read_b = get(slot_b, Key::Int(0)).expect("the key is there").as_int();
        if take_reference && keep_binding {
            let boxed = match get_element(slot_a) {
                Some(v) => v.entity_ptr(),
                None => std::ptr::null_mut(),
            };

            if !boxed.is_null() {
                crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, boxed);
            }
        }

        // The holder goes, and both arrays with it. Left standing,
        // their storage keeps buffer-arena chunks that the arena's
        // own tests then find in a shape they did not put it in.
        if category == MemoryCategory::GcHeap {
            assert!(ll_release(holder as *mut RcHeader));
            ll_object_die(holder);
        }

        (read_a, read_b)
    }
}

/// The element as the entry holds it, box included — what [`get`]
/// deliberately looks through.
unsafe fn get_element(slot: *const Value) -> Option<Value> {
    let a = unsafe { (*slot).entity_ptr() } as *mut LLArray;
    unsafe { crate::array::testing::get(a, Key::Int(0)) }
}

mod a_box_outliving_what_moves_the_entry;
mod an_element_in_a_reference_state;
mod crossing_out_of_the_arena;
mod the_key_a_spelling_means;
mod the_writes_and_the_separation_they_share;
mod what_a_copy_does_with_a_box;
mod what_a_refusal_leaves_behind;
