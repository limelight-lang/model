//! The `array` entity: a refcounted header over the ordered hash.
//!
//! ```text
//! +0   RcHeader   (kind = Array, COW set)
//! +8   class      *const Class
//! +16  Table      the storage handle
//! ```
//!
//! The table itself holds no header: this is what supplies the refcount,
//! the memory category and the COW state, so the table can be tested and
//! reasoned about without an entity around it.
//!
//! **COW separation is shallow**, which is what `rfc/model/arrays.md`
//! says and what PHP's semantics require: the copy gets its own storage
//! and its own index, and the children are retained and shared until one
//! of them is written. That is a different operation from the store
//! barrier's *escape* copy, which is deep and category-driven; conflating
//! the two is the contradiction `rfc/model/arrays-hashtable.md` had to
//! settle, and an implementer who reads "copy" without asking which one
//! will build the wrong thing.

use crate::array::table::{Key, Table};
use crate::class::Class;
use crate::memory::immortal::immortal_alloc;
use crate::refcount::{COW, EntityKind, MemoryCategory, RcHeader, publish_header};
use crate::value::Value;

/// The array entity.
#[repr(C)]
pub struct LLArray {
    pub rc: RcHeader,
    pub class: *const Class,
    pub table: Table,
}

/// Allocate an empty array in `category`.
///
/// **Null when the allocation fails.** An array can be built mid-request
/// from a size the program chose, so a refusal has to reach a frame that
/// can raise rather than end the process.
///
/// The header is published last, as one store, so a walker never sees a
/// header over a body that is not yet formed — the same commissioning
/// contract as every other factory here.
///
/// # Safety
/// Standard factory contract: the result is a fresh entity at count 1.
pub unsafe fn ll_array_new(category: MemoryCategory, class: *const Class, salt: u64) -> *mut LLArray {
    let size = size_of::<LLArray>();
    let mem = match category {
        MemoryCategory::RequestArena => unsafe {
            (*crate::memory::context::resolve_arena(std::ptr::null_mut())).alloc(size)
        },
        MemoryCategory::GcHeap | MemoryCategory::LongLived => unsafe {
            crate::memory::heap::entity_alloc(size)
        },
        MemoryCategory::Immortal => immortal_alloc(size),
    };
    if mem.is_null() {
        return std::ptr::null_mut();
    }
    let a = mem as *mut LLArray;
    unsafe {
        (&raw mut (*a).class).write(class);
        (&raw mut (*a).table).write(Table::empty(category, salt));
        publish_header(
            a as *mut RcHeader,
            RcHeader::new(category, COW | EntityKind::Array.to_flags()),
        );
    }
    a
}

impl LLArray {
    #[inline]
    pub fn category(&self) -> MemoryCategory {
        MemoryCategory::from_flags(self.rc.flags)
    }

    /// Whether a write to this array has to separate first — the rule in
    /// `refcount::cow_separation_needed`, which reads the category before
    /// the count.
    #[inline]
    pub fn needs_separation(&self) -> bool {
        crate::refcount::cow_separation_needed(self.rc.flags, self.rc.refcount)
    }
}

/// Copy `src` into a fresh array of `category` — the **shallow** COW
/// separation. Every element is copied as a `Value` and every counted
/// child is retained once for the copy; nothing is walked recursively,
/// because both arrays share the children until one is written.
///
/// Insertion order survives, because the copy replays `src` in order.
///
/// **Null on refusal**, with nothing published: the copy is private until
/// it is returned, so a failure part-way releases what it has retained
/// and leaves the source untouched.
///
/// # Safety
/// `src` is a live array entity.
pub unsafe fn separate(src: *mut LLArray, category: MemoryCategory) -> *mut LLArray {
    let class = unsafe { (*src).class };
    let salt = unsafe { (*src).table.salt() };
    let dst = unsafe { ll_array_new(category, class, salt) };
    if dst.is_null() {
        return std::ptr::null_mut();
    }
    let n = unsafe { (*src).table.used() };
    for i in 0..n {
        let e = unsafe { (*src).table.entry(i) };
        if e.is_hole() {
            continue;
        }
        let key = if e.is_int_key() {
            Key::Int(e.hash_or_key as i64)
        } else {
            Key::Str(e.string_key())
        };
        let v = e.value;
        if unsafe { (*dst).table.insert(key, v) }.is_none() {
            // Out of memory part-way. Release what the copy retained and
            // report; the source is exactly as it was.
            unsafe { release_children(dst) };
            unsafe { (*dst).table.dispose() };
            return std::ptr::null_mut();
        }
        // Retain the child for the copy's own reference. Done after the
        // insert succeeded, so a refusal has nothing extra to unwind.
        unsafe { retain_value(&v) };
        if let Key::Str(s) = key {
            crate::refcount::ll_retain(s as *mut RcHeader);
        }
    }
    dst
}

#[inline]
unsafe fn retain_value(v: &Value) {
    if v.is_refcounted() {
        crate::refcount::ll_retain(v.entity_ptr());
    }
}

/// Release every counted child of `a` — elements and string keys — in
/// insertion order. The storage is not freed here; teardown does that
/// after, because the order matters to the collector.
///
/// # Safety
/// `a` is a live array entity whose children are still counted.
pub unsafe fn release_children(a: *mut LLArray) {
    let n = unsafe { (*a).table.used() };
    for i in 0..n {
        let e = unsafe { (*a).table.entry(i) };
        if e.is_hole() {
            continue;
        }
        if e.value.is_refcounted() {
            unsafe { crate::refcount::ll_release(e.value.entity_ptr()) };
        }
        let s = e.string_key();
        if !s.is_null() {
            unsafe { crate::refcount::ll_release(s as *mut RcHeader) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::table::Key;
    use crate::string::{LLString, ll_string_new};

    fn mk(bytes: &[u8]) -> *mut LLString {
        unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) }
    }

    fn arr() -> *mut LLArray {
        unsafe { ll_array_new(MemoryCategory::GcHeap, std::ptr::null(), 0x9E37_79B9) }
    }

    #[test]
    fn a_fresh_array_is_a_cow_entity_of_the_array_kind_at_count_one() {
        let _g = crate::memory::block_pool::test_guard();
        let a = arr();
        assert!(!a.is_null());
        unsafe {
            assert_eq!((*a).rc.refcount, 1);
            assert_eq!((*a).rc.flags & crate::refcount::COW, crate::refcount::COW);
            assert_eq!(
                (*a).rc.flags & crate::refcount::ENTITY_KIND_MASK,
                EntityKind::Array.to_flags()
            );
            assert_eq!((*a).category(), MemoryCategory::GcHeap);
            assert!((*a).table.is_empty());
            (*a).table.dispose();
        }
    }

    /// The rule reads the category before the count: a heap array at
    /// count 1 is exclusively owned and writes in place.
    #[test]
    fn separation_is_needed_only_when_the_array_is_shared() {
        let _g = crate::memory::block_pool::test_guard();
        let a = arr();
        unsafe {
            assert!(!(*a).needs_separation(), "count 1 writes in place");
            crate::refcount::ll_retain(a as *mut RcHeader);
            assert!((*a).needs_separation(), "a second holder forces a copy");
            crate::refcount::ll_release(a as *mut RcHeader);
            (*a).table.dispose();
        }
    }

    #[test]
    fn separation_copies_the_order_and_shares_the_children() {
        let _g = crate::memory::block_pool::test_guard();
        let src = arr();
        let key = mk(b"shared");
        let child = mk(b"child-value");
        unsafe {
            (*src).table.insert(Key::Int(1), Value::int(10));
            (*src)
                .table
                .insert(Key::Str(key), Value::entity(crate::value::Tag::String, child as *mut RcHeader));
            (*src).table.insert(Key::Int(2), Value::int(20));
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(child as *mut RcHeader);
        }

        let before_key = unsafe { (*(key as *mut RcHeader)).refcount };
        let before_child = unsafe { (*(child as *mut RcHeader)).refcount };

        let dst = unsafe { separate(src, MemoryCategory::GcHeap) };
        assert!(!dst.is_null());

        unsafe {
            // Order survives.
            let order: Vec<i64> = (*dst)
                .table
                .iter()
                .map(|e| if e.is_int_key() { e.hash_or_key as i64 } else { -1 })
                .collect();
            assert_eq!(order, vec![1, -1, 2]);

            // The children are shared, each counted once more.
            assert_eq!((*(key as *mut RcHeader)).refcount, before_key + 1);
            assert_eq!((*(child as *mut RcHeader)).refcount, before_child + 1);

            // Writing the copy does not touch the source.
            (*dst).table.insert(Key::Int(1), Value::int(999));
            assert_eq!((*src).table.get(Key::Int(1)).unwrap().as_int(), 10);
            assert_eq!((*dst).table.get(Key::Int(1)).unwrap().as_int(), 999);

            release_children(dst);
            (*dst).table.dispose();
            release_children(src);
            (*src).table.dispose();
        }
    }

    #[test]
    fn separation_carries_holes_away_rather_than_copying_them() {
        let _g = crate::memory::block_pool::test_guard();
        let src = arr();
        unsafe {
            for i in 0..10i64 {
                (*src).table.insert(Key::Int(i), Value::int(i));
            }
            for i in [2i64, 5, 8] {
                (*src).table.remove(Key::Int(i));
            }
            let dst = separate(src, MemoryCategory::GcHeap);
            assert!(!dst.is_null());
            assert_eq!((*dst).table.len(), 7);
            assert_eq!(
                (*dst).table.used(),
                7,
                "the copy starts dense: a hole is not worth copying"
            );
            let order: Vec<i64> = (*dst).table.iter().map(|e| e.hash_or_key as i64).collect();
            assert_eq!(order, vec![0, 1, 3, 4, 6, 7, 9]);

            (*dst).table.dispose();
            (*src).table.dispose();
        }
    }

    #[test]
    fn releasing_children_walks_elements_and_string_keys_once_each() {
        let _g = crate::memory::block_pool::test_guard();
        let a = arr();
        let key = mk(b"k");
        let child = mk(b"v");
        unsafe {
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(child as *mut RcHeader);
            (*a).table.insert(
                Key::Str(key),
                Value::entity(crate::value::Tag::String, child as *mut RcHeader),
            );
            let k0 = (*(key as *mut RcHeader)).refcount;
            let c0 = (*(child as *mut RcHeader)).refcount;

            release_children(a);
            assert_eq!((*(key as *mut RcHeader)).refcount, k0 - 1);
            assert_eq!((*(child as *mut RcHeader)).refcount, c0 - 1);

            (*a).table.dispose();
        }
    }
}
