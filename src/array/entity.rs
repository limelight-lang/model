//! The `array` entity: a refcounted header over the ordered hash.
//!
//! ```text
//! +0   RcHeader   (kind = Array, COW set)
//! +8   Table      the storage handle
//! ```
//!
//! **No per-instance class pointer**, the same construction as a string
//! (`rfc/model/arrays.md`: "a single final class … no per-instance class
//! pointer, devirtualized methods"). The entity kind *is* the class:
//! `array` is final, so nothing needs to be read to know what this is,
//! and the storage-strategy tag is an internal bit invisible to
//! `instanceof`. Spending eight bytes on a word that would hold the same
//! value in every array ever allocated is exactly the trade the string
//! layout already refused.
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
use crate::memory::immortal::immortal_alloc;
use crate::refcount::{COW, EntityKind, MemoryCategory, RcHeader, publish_header};
use crate::value::Value;

/// The array entity.
#[repr(C)]
pub struct LLArray {
    pub rc: RcHeader,
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
pub unsafe fn ll_array_new(category: MemoryCategory, salt: u64) -> *mut LLArray {
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
    let salt = unsafe { (*src).table.salt() };
    let dst = unsafe { ll_array_new(category, salt) };
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
            unsafe { crate::refcount::ll_retain(s as *mut RcHeader) };
        }
    }
    dst
}

#[inline]
unsafe fn retain_value(v: &Value) {
    if v.is_refcounted() {
        unsafe { crate::refcount::ll_retain(v.entity_ptr()) };
    }
}

/// Every counted child of `a` — elements and string keys — in insertion
/// order, element before its own key.
///
/// One walk with three consumers, which is why it exists rather than
/// three loops over the same entries: teardown releases what it yields,
/// the tracer counts it as an out-edge, and the arena reset reaches an
/// array's children through the same tracer. A key is a child like an
/// element is: a table holds a reference to each string it keys on.
///
/// # Safety
/// `a` is a live array entity whose storage is still readable.
pub unsafe fn for_each_counted_child(a: *mut LLArray, mut visit: impl FnMut(*mut RcHeader)) {
    let n = unsafe { (*a).table.used() };
    for i in 0..n {
        let e = unsafe { (*a).table.entry(i) };
        if e.is_hole() {
            continue;
        }
        if e.value.is_refcounted() {
            visit(e.value.entity_ptr());
        }
        let s = e.string_key();
        if !s.is_null() {
            visit(s as *mut RcHeader);
        }
    }
}

/// Release every counted child of `a`. The storage is not freed here;
/// teardown does that after, because the order matters to the collector.
///
/// Each release goes through the store barrier's `drop_ref` rather than
/// through `ll_release` directly, and the difference is not stylistic.
/// `ll_release` only decrements and reports; whoever gets `true` owes the
/// teardown, so dropping that answer leaks every child the array was the
/// last holder of. `drop_ref` also settles the two rules an owner letting
/// go has to obey: an arena child held by a longer-lived array loses its
/// escape hold-count, and a heap child inside an arena array is left to
/// the release-at-reset log that owns it.
///
/// # Safety
/// `a` is a live array entity whose children are still counted.
pub unsafe fn release_children(a: *mut LLArray) {
    let owner_cat = unsafe { crate::object::header_category(a as *const RcHeader) };
    unsafe {
        for_each_counted_child(a, |child| {
            crate::memory::barrier::drop_ref(owner_cat, child);
        })
    };
}

/// Sever this array's counted children — elements and string keys alike
/// — collecting them into `displaced` without releasing them. The array's
/// arm of the drain's Phase 4, and the counterpart of
/// [`crate::object::sever_counted_children`]: same contract, same reason
/// for not dropping inline, and the caller owes one drop per entry.
///
/// One line, because every entry the walk yields is the table's and so is
/// the state that has to replace it: see
/// [`crate::array::table::Table::sever_entries`].
///
/// # Safety
/// `a` must be a live array entity whose storage is readable and
/// writable.
pub(crate) unsafe fn sever_counted_children(a: *mut LLArray, displaced: &mut Vec<*mut RcHeader>) {
    unsafe { (*a).table.sever_entries(displaced) };
}

/// The address of the array's storage and the bytes granted for it — the
/// block promotion retains when a carry was refused. Null when the array
/// never grew a table.
///
/// # Safety
/// `a` must be a live array entity.
pub(crate) unsafe fn storage_address(a: *mut LLArray) -> *mut u8 {
    unsafe { (*a).table.storage().0 }
}

/// Bring a surviving array's storage out of the arena that is about to
/// reset. One line, because the storage is the table's and so is every
/// reason: see [`crate::array::table::Table::carry_out_of`].
///
/// # Safety
/// `a` must be a live request-arena array of `arena`, mid-reset.
pub(crate) unsafe fn carry_storage_out_of(
    arena: *mut crate::memory::arena::Arena,
    a: *mut LLArray,
) -> bool {
    unsafe { (*a).table.carry_out_of(arena) }
}

/// Teardown for an array whose count reached zero, or that a collector
/// owns: children first, then the storage, then the entity's own memory.
///
/// The order is forced rather than chosen. `release_children` reads the
/// storage to find the children, so the storage cannot already be gone;
/// and a child's death can run user code, which must not meet a table
/// half-disposed. The entity's own memory follows the same rule as a
/// string's: only the GC heap frees here, an arena entity dying with its
/// reset and an immortal one not dying at all.
///
/// # Safety
/// `a` must be a live array entity.
pub(crate) unsafe fn array_die(a: *mut LLArray) {
    unsafe { release_children(a) };
    unsafe { (*a).table.dispose() };
    if unsafe { crate::object::header_category(a as *const RcHeader) } == MemoryCategory::GcHeap {
        unsafe { crate::memory::stdapi::ll_free(a as *mut u8) };
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
        unsafe { ll_array_new(MemoryCategory::GcHeap, 0x9E37_79B9) }
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
            (*src).table.insert(
                Key::Str(key),
                Value::entity(crate::value::Tag::String, child as *mut RcHeader),
            );
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
                .map(|e| {
                    if e.is_int_key() {
                        e.hash_or_key as i64
                    } else {
                        -1
                    }
                })
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

    /// Death through the kind switch, which is the only door a bare
    /// entity pointer has. Before the Array arm existed this reached a
    /// `debug_assert!(false)` and, in release, did nothing at all: the
    /// children kept the references the array owed them and the storage
    /// was never returned.
    #[test]
    fn dying_through_the_kind_switch_releases_the_children_and_the_storage() {
        use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK};
        use crate::memory::buffer::{PressureMode, set_pressure_mode};
        use crate::memory::buffer_arena::with_buffer_arena;
        use crate::refcount::{ll_release, ll_retain};
        let _g = crate::memory::block_pool::test_guard();

        let a = arr();
        let key = mk(b"key");
        let value = mk(b"value");
        unsafe {
            // One reference each for the array, one for this test, so the
            // children outlive the array and can be read afterwards.
            ll_retain(key as *mut RcHeader);
            ll_retain(value as *mut RcHeader);
            (*a).table.insert(
                Key::Str(key),
                Value::entity(crate::value::Tag::String, value as *mut RcHeader),
            );
            let (storage, capacity) = (*a).table.storage();
            assert!(
                !storage.is_null(),
                "the insert allocated storage to release"
            );

            assert!(
                ll_release(a as *mut RcHeader),
                "the array was the last holder"
            );
            crate::object::ll_entity_die(a as *mut RcHeader);

            assert_eq!(
                (*(key as *mut RcHeader)).refcount,
                1,
                "the key's reference was not let go"
            );
            assert_eq!(
                (*(value as *mut RcHeader)).refcount,
                1,
                "the element's reference was not let go"
            );
            // The storage came back: in critical mode an allocation
            // searches the block's free list, so the same address
            // returning means teardown really disposed of the table
            // rather than only dropping the entity.
            let kind = *(((storage as usize) & !BLOCK_MASK) as *const u32);
            assert_eq!(
                kind, BLOCK_KIND_BUFFER,
                "the storage was not a buffer chunk"
            );
            set_pressure_mode(PressureMode::Critical);
            let (reused, _) = with_buffer_arena(|arena| arena.alloc(capacity));
            set_pressure_mode(PressureMode::Plenty);
            assert_eq!(reused, storage, "teardown left the storage unreturned");
            with_buffer_arena(|arena| arena.free(reused, capacity));

            // Released first: a slot freed while its header still reads
            // refcount 1 is enumerated as a live entity by every later
            // process-global walk (`PLAN.md`, the census flake).
            assert!(ll_release(key as *mut RcHeader));
            crate::object::ll_entity_die(key as *mut RcHeader);
            assert!(ll_release(value as *mut RcHeader));
            crate::object::ll_entity_die(value as *mut RcHeader);
        }
    }

    /// A child the array was the last holder of has to be torn down, not
    /// merely decremented. `ll_release` reports the death and the report
    /// is an obligation: dropping it leaves the child's own memory — and
    /// everything *it* holds — unreachable and unfreed. Observed through a
    /// nested array, whose storage is a buffer chunk that can be seen
    /// coming back.
    #[test]
    fn a_child_the_array_held_last_is_torn_down_and_not_only_released() {
        use crate::memory::buffer::{PressureMode, set_pressure_mode};
        use crate::memory::buffer_arena::with_buffer_arena;
        use crate::refcount::ll_release;
        let _g = crate::memory::block_pool::test_guard();

        let outer = arr();
        let inner = arr();
        unsafe {
            (*inner).table.insert(Key::Int(1), Value::int(1));
            let (storage, capacity) = (*inner).table.storage();
            assert!(!storage.is_null(), "the inner array has storage to reclaim");

            // The inner array's only reference is the outer array's
            // element, so the outer's death is the inner's death.
            (*outer).table.insert(
                Key::Int(0),
                Value::entity(crate::value::Tag::Array, inner as *mut RcHeader),
            );

            assert!(ll_release(outer as *mut RcHeader));
            crate::object::ll_entity_die(outer as *mut RcHeader);

            // Both tables are freed by this teardown — the inner one by
            // the cascade, the outer one by its own dispose — and the
            // free list is LIFO, so the inner chunk is the second one
            // back, not the first.
            set_pressure_mode(PressureMode::Critical);
            let first = with_buffer_arena(|arena| arena.alloc(capacity));
            let second = with_buffer_arena(|arena| arena.alloc(capacity));
            set_pressure_mode(PressureMode::Plenty);
            assert!(
                first.0 == storage || second.0 == storage,
                "the inner array was released but never torn down: its storage never came back"
            );
            with_buffer_arena(|arena| {
                arena.free(first.0, first.1);
                arena.free(second.0, second.1);
            });
        }
    }

    /// The layout the design fixes: no per-instance class pointer, the
    /// same construction as a string. `array` is final, so the entity
    /// kind already says what this is.
    #[test]
    fn an_array_carries_no_class_pointer() {
        assert_eq!(std::mem::offset_of!(LLArray, rc), 0);
        assert_eq!(
            std::mem::offset_of!(LLArray, table),
            8,
            "the table starts straight after the header — nothing between"
        );
    }
}
