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
    // No context to pass: an array factory takes none, so the arena is
    // the mounted one either way.
    let mem =
        unsafe { crate::memory::routing::entity_alloc_in(std::ptr::null_mut(), category, size) };
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

/// Copy `src` into a fresh array of `category` — **one body for both
/// depths**, with `category` supplying the depth.
///
/// Every element is copied as a `Value` and every counted child is
/// published for the copy through the store barrier
/// (`barrier::store_category_barrier`), which is where the two depths
/// part company. With an arena destination the barrier's copy arm cannot
/// fire and nothing is walked recursively: both arrays share the children
/// until one is written, which is the shallow separation. With a
/// longer-lived destination over an arena source every arena COW child is
/// copied in turn, which is the deep copy of
/// `rfc/model/arrays-hashtable.md` clause for clause. Two call sites, one
/// operation (`dev/DECISIONS.md`).
///
/// **The barrier rather than a bare `ll_retain`**, and not for tidiness:
/// `release_children` gives references back through `drop_ref`, which
/// calls `escape_lose`, so a copy that recorded no gain would spend a
/// hold-count belonging to a real holder.
///
/// Insertion order survives, because the copy replays `src` in order,
/// and so does the flood backstop's state: a copy of an attacked table
/// is attacked.
///
/// **Null on refusal**, with nothing published: the copy is private until
/// it is returned, so a failure part-way releases what it has retained
/// and leaves the source untouched.
///
/// **Nesting is worked through a list, not the machine stack.** Depth
/// here is attacker-shaped input on a store path — `$deep = [[[[…]]]]`
/// in the arena, then one assignment into a heap slot — and a limit was
/// rejected as the answer: it would have to refuse through a channel
/// whose only meaning is "out of memory", PHP has no depth at which an
/// assignment becomes invalid, and teardown of whatever the limit
/// permitted is recursive anyway (`dev/DECISIONS.md`, `PLAN.md` item 11).
/// So a nested arena array is copied empty, published, and its filling
/// pushed onto [`WorkList`], which lives in a buffer-arena chunk.
///
/// **Termination needs no visited set.** The list is entered only by an
/// arena COW child, and a cycle cannot close inside a pure-COW subgraph
/// while count-equals-holders holds: every entity a real ring passes
/// through is non-COW, and those are published by the barrier rather
/// than entered. A debug build checks that claim rather than paying for
/// it.
///
/// # Safety
/// `src` is a live array entity; `arena` the live mounted arena, which
/// the barrier needs to count an escape or log a release at reset.
pub unsafe fn separate(
    src: *mut LLArray,
    category: MemoryCategory,
    arena: *mut crate::memory::arena::Arena,
) -> *mut LLArray {
    let dst = unsafe { new_empty_copy(src, category) };
    if dst.is_null() {
        return std::ptr::null_mut();
    }
    let mut pending = WorkList::new();
    #[cfg(debug_assertions)]
    let mut entered: Vec<*mut LLArray> = Vec::new();
    let mut next = Some((src, dst));
    while let Some((s, d)) = next {
        #[cfg(debug_assertions)]
        {
            assert!(
                !entered.contains(&s),
                "a COW subgraph closed on itself: count-equals-holders is broken"
            );
            entered.push(s);
        }
        if !unsafe { fill_from(s, d, arena, &mut pending) } {
            // Refused part-way. Releasing the root's children cascades
            // into every copy this call published, nested ones included:
            // each is held once, by the entry naming it.
            unsafe { release_children(dst) };
            unsafe { (*dst).table.dispose() };
            pending.dispose();
            return std::ptr::null_mut();
        }
        next = pending.pop();
    }
    pending.dispose();
    dst
}

/// A destination array with the source's salt and flood state and no
/// entries. Null when the allocation is refused.
///
/// The flood state goes in before the first insert, because it decides
/// how a key is hashed: a copy that starts weak re-installs an
/// attacker's whole collision set under the hash the source escalated
/// away from.
///
/// # Safety
/// `src` is a live array entity.
unsafe fn new_empty_copy(src: *mut LLArray, category: MemoryCategory) -> *mut LLArray {
    let salt = unsafe { (*src).table.salt() };
    let dst = unsafe { ll_array_new(category, salt) };
    if dst.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { (*dst).table.adopt_flood_state(&(*src).table) };
    dst
}

/// Copy `src`'s entries into the empty `dst`, publishing every counted
/// child for it. False on refusal, with what this call took given back
/// and `dst` left holding whatever it had published — the caller
/// releases that through the root.
///
/// A nested arena array is not copied here: it is copied *empty*,
/// published, and pushed onto `pending`. Everything else goes through
/// the store barrier, whose copy arm is a leaf for every kind but this
/// one.
///
/// The destination's category comes from `dst` rather than from the
/// caller, because a nested copy's category is not the root's: it is
/// `separation_category` of it, which the empty copy already carries.
///
/// # Safety
/// `src` and `dst` are live arrays, `dst` empty; `arena` the live
/// mounted arena.
unsafe fn fill_from(
    src: *mut LLArray,
    dst: *mut LLArray,
    arena: *mut crate::memory::arena::Arena,
    pending: &mut WorkList,
) -> bool {
    let category = unsafe { (*dst).category() };
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
        // Publish the element for the copy *before* the entry is written,
        // so the entry never names something the copy does not hold, and
        // so the barrier can hand back a different entity: an arena COW
        // child crossing into a longer-lived copy is replaced by a copy of
        // its own.
        let mut v = e.value;
        if v.is_refcounted() {
            let child = v.entity_ptr();
            if unsafe { is_nested_arena_array(child, category) } {
                let copy = unsafe {
                    new_empty_copy(
                        child as *mut LLArray,
                        crate::refcount::separation_category(category),
                    )
                };
                if copy.is_null() || !pending.push((child as *mut LLArray, copy)) {
                    // The copy is held by nothing yet, so it goes back
                    // here rather than through the root's cascade.
                    unsafe { crate::refcount::ll_release(copy as *mut RcHeader) };
                    unsafe { (*copy).table.dispose() };
                    return false;
                }
                v = Value::entity(v.tag(), copy as *mut RcHeader);
            } else {
                unsafe { crate::refcount::ll_retain(child) };
                let stored = unsafe {
                    crate::memory::barrier::store_category_barrier(arena, category, child)
                };
                if stored.is_null() {
                    unsafe { crate::refcount::ll_release(child) };
                    return false;
                }
                if stored != child {
                    // The barrier copied it. The copy arrives at +1 and is
                    // what the entry names; the retain above goes back.
                    unsafe { crate::refcount::ll_release(child) };
                    v = Value::entity(v.tag(), stored);
                }
            }
        }
        let published_key = if let Key::Str(k) = key {
            let child = k as *mut RcHeader;
            unsafe { crate::refcount::ll_retain(child) };
            let stored =
                unsafe { crate::memory::barrier::store_category_barrier(arena, category, child) };
            if stored.is_null() {
                unsafe { crate::refcount::ll_release(child) };
                unsafe { release_value(&v) };
                return false;
            }
            if stored != child {
                unsafe { crate::refcount::ll_release(child) };
            }
            Key::Str(stored as *mut crate::string::LLString)
        } else {
            key
        };
        if unsafe { (*dst).table.insert(published_key, v) }.is_none() {
            // Out of memory part-way. Give back what this element took;
            // the source is untouched.
            unsafe { release_value(&v) };
            if let Key::Str(k) = published_key {
                unsafe { crate::refcount::ll_release(k as *mut RcHeader) };
            }
            return false;
        }
    }
    true
}

/// Whether publishing `child` into a slot of `category` would copy it
/// **and** the copy would contain another copy — an arena COW array
/// crossing into a longer-lived destination. That is the one child whose
/// copying recurses, and the only one the work list exists for; the
/// barrier's copy arm is a leaf for every other kind.
///
/// # Safety
/// `child` is a live entity.
#[inline]
unsafe fn is_nested_arena_array(child: *mut RcHeader, category: MemoryCategory) -> bool {
    if category == MemoryCategory::RequestArena {
        return false;
    }
    let flags = unsafe { crate::refcount::header_flags(child) };
    MemoryCategory::from_flags(flags) == MemoryCategory::RequestArena
        && flags & COW != 0
        && (flags & crate::refcount::ENTITY_KIND_MASK) >> crate::refcount::ENTITY_KIND_SHIFT
            == EntityKind::Array as u32
}

/// The arrays a deep copy still owes, as `(source, destination)` pairs.
///
/// **In a buffer-arena chunk**, which is the decision rather than an
/// implementation detail. The machine stack is what this replaces. Arena
/// bump memory is wrong because the list outlives nothing and would be
/// held to the reset. The system allocator's `Vec` would abort the
/// process when it cannot grow, and growth here is driven by the
/// attacker's nesting depth, so a refusal has to be a value rather than
/// an abort.
///
/// Empty until the first nested array, so an ordinary copy allocates
/// nothing.
struct WorkList {
    items: *mut (*mut LLArray, *mut LLArray),
    len: usize,
    capacity: usize,
    /// Bytes granted, which is what the free needs: the buffer arena's
    /// free is size-carrying and a chunk holds no metadata.
    granted: usize,
}

impl WorkList {
    /// The pairs a first growth makes room for. Deep enough that
    /// ordinary nesting never grows twice, small enough to be one
    /// allocation.
    const FIRST: usize = 8;

    const fn new() -> Self {
        WorkList {
            items: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
            granted: 0,
        }
    }

    /// False when the chunk could not grow, which the caller turns into
    /// a refused copy.
    fn push(&mut self, pair: (*mut LLArray, *mut LLArray)) -> bool {
        if self.len == self.capacity && !self.grow() {
            return false;
        }
        unsafe { self.items.add(self.len).write(pair) };
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<(*mut LLArray, *mut LLArray)> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(unsafe { self.items.add(self.len).read() })
    }

    fn grow(&mut self) -> bool {
        let capacity = if self.capacity == 0 {
            Self::FIRST
        } else {
            self.capacity * 2
        };
        let bytes = capacity * size_of::<(*mut LLArray, *mut LLArray)>();
        let (fresh, granted) = unsafe {
            crate::memory::routing::body_alloc(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes)
        };
        if fresh.is_null() {
            return false;
        }
        debug_assert_eq!(
            fresh as usize % align_of::<(*mut LLArray, *mut LLArray)>(),
            0,
            "the buffer arena hands out 8-aligned chunks"
        );
        let fresh = fresh as *mut (*mut LLArray, *mut LLArray);
        if self.len > 0 {
            unsafe { std::ptr::copy_nonoverlapping(self.items, fresh, self.len) };
        }
        // The old chunk directly rather than through `dispose`, which
        // empties the list: what is being replaced is the storage, not
        // the contents.
        unsafe {
            crate::memory::routing::body_free(
                MemoryCategory::GcHeap,
                self.items as *mut u8,
                self.granted,
            )
        };
        self.items = fresh;
        self.capacity = capacity;
        self.granted = granted;
        true
    }

    fn dispose(&mut self) {
        unsafe {
            crate::memory::routing::body_free(
                MemoryCategory::GcHeap,
                self.items as *mut u8,
                self.granted,
            )
        };
        self.items = std::ptr::null_mut();
        self.capacity = 0;
        self.granted = 0;
        self.len = 0;
    }
}

#[inline]
unsafe fn release_value(v: &Value) {
    if v.is_refcounted() {
        unsafe { crate::refcount::ll_release(v.entity_ptr()) };
    }
}

/// Every counted child of `a` — elements **and** string keys, a table
/// holding a reference to each string it keys on.
///
/// An adapter over the one tracing stride rather than a walk of its own:
/// the array's cells are `walk::trace_cells`' since the coherent read
/// exists (`PLAN.md`, item 12). Kept as a name because the release side
/// reads better for it, and because a caller here has an `LLArray` rather
/// than a bare header and a kind.
///
/// # Safety
/// `a` is a live array entity.
pub unsafe fn for_each_counted_child(a: *mut LLArray, mut visit: impl FnMut(*mut RcHeader)) {
    unsafe {
        crate::walk::trace_cells::<crate::walk::PlainCells>(
            a as *mut RcHeader,
            crate::refcount::EntityKind::Array as u32,
            |cell| visit(cell.child),
        )
    };
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
    unsafe { (*a).table.storage_and_capacity().0 }
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
    use crate::refcount::ll_release;
    use crate::string::{LLString, ll_string_new};

    /// The COW door. A shared array asked to separate must hand back a
    /// **different** array; returning the original is a write into a value
    /// two holders share, which in release happens with no signal at all.
    #[test]
    fn a_shared_array_separates_into_a_copy_of_its_own() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap, 0x9E37_79B9) };
        let key = mk(b"k");
        let value = mk(b"v");
        unsafe {
            // `insert` writes the entry raw and leaves the counting to the
            // caller, so these are the source array's own references — and
            // they are taken first, because an entry a walker can reach
            // must already be backed by a count.
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(value as *mut RcHeader);
            (*src).table.insert(
                Key::Str(key),
                Value::entity(crate::value::Tag::String, value as *mut RcHeader),
            );
        }
        // A second holder is what makes the write a separation.
        unsafe { crate::refcount::ll_retain(src as *mut RcHeader) };

        let copy = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, src as *mut RcHeader)
        } as *mut LLArray;
        assert_ne!(copy, src, "the shared array was written in place");
        assert_eq!(
            unsafe { (*copy).table.used() },
            1,
            "the entry did not survive"
        );
        // Three each: this test, the source array, and the copy.
        assert_eq!(
            unsafe { (*(key as *mut RcHeader)).refcount },
            3,
            "the copy did not take a reference to the key"
        );
        assert_eq!(
            unsafe { (*(value as *mut RcHeader)).refcount },
            3,
            "the copy did not take a reference to the element"
        );

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(!ll_release(src as *mut RcHeader));
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
            assert!(ll_release(key as *mut RcHeader));
            crate::object::ll_entity_die(key as *mut RcHeader);
            assert!(ll_release(value as *mut RcHeader));
            crate::object::ll_entity_die(value as *mut RcHeader);
        }
        arena.reset(|_| {});
    }

    /// The escape door. An arena array taken by a longer-lived holder is
    /// copied out, and its arena COW children are copied with it — a hold
    /// on arena memory in a heap slot dangles at the reset.
    #[test]
    fn an_arena_array_taken_by_a_heap_holder_is_copied_out_with_its_children() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        // `ll_array_new` takes no context and resolves this thread's, so
        // an arena array needs one mounted. One raw pointer, reused: a
        // fresh `&mut` per call retags and invalidates what TLS holds
        // (`dev/WORKFLOW.md`, Miri).
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let holder_class = crate::class::ClassBuilder::new("ArrayHolder")
            .prop("a", true)
            .build();
        let holder = unsafe {
            crate::object::new_constructed(context_ptr, holder_class, MemoryCategory::GcHeap)
        };

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena, 0x9E37_79B9) };
        let element =
            unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"in the arena") };
        unsafe {
            crate::refcount::ll_retain(element as *mut RcHeader);
            (*src).table.insert(
                Key::Int(1),
                Value::entity(crate::value::Tag::String, element as *mut RcHeader),
            );
        }

        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                crate::object::Object::prop_at(holder, 16),
                std::ptr::null_mut(),
                Value::entity(crate::value::Tag::Array, src as *mut RcHeader),
            ));
        }

        let stored =
            unsafe { (*crate::object::Object::prop_at(holder, 16)).entity_ptr() } as *mut LLArray;
        assert_ne!(stored, src, "the heap slot took the arena array itself");
        assert_eq!(
            unsafe { (*stored).rc.memory_category() },
            MemoryCategory::GcHeap,
            "the copy did not land in the heap"
        );
        let copied_element = unsafe { (*stored).table.entry(0).value.entity_ptr() };
        assert_ne!(
            copied_element, element as *mut RcHeader,
            "the copy still holds the arena string"
        );
        assert_eq!(
            unsafe { crate::object::header_category(copied_element) },
            MemoryCategory::GcHeap,
            "the copied element did not leave the arena"
        );

        unsafe {
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
        }
        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// Nesting is worked through the list, so the copy of a deep arena
    /// array touches one stack frame per *call*, not one per level. The
    /// depth is well past `WorkList::FIRST`, so the chunk grows more than
    /// once on the way through.
    ///
    /// **What this does not prove** is a bound on the attacker's depth:
    /// teardown of the copy is still recursive, one nested set of frames
    /// per level, which is why the depth here is modest and why the
    /// remaining half is named in `PLAN.md`.
    #[test]
    fn a_deep_arena_array_is_copied_out_through_the_work_list() {
        const DEPTH: usize = 200;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        // `[[[…]]]`, built from the inside out: each array holds the one
        // below it under key 0, and the source stays entirely in the
        // arena, where COW children are shared rather than copied.
        let mut levels = Vec::with_capacity(DEPTH);
        let innermost = unsafe { ll_array_new(MemoryCategory::RequestArena, 0x9E37_79B9) };
        levels.push(innermost);
        for _ in 1..DEPTH {
            let outer = unsafe { ll_array_new(MemoryCategory::RequestArena, 0x9E37_79B9) };
            let inner = *levels.last().unwrap();
            unsafe {
                crate::refcount::ll_retain(inner as *mut RcHeader);
                (*outer).table.insert(
                    Key::Int(0),
                    Value::entity(crate::value::Tag::Array, inner as *mut RcHeader),
                );
            }
            levels.push(outer);
        }
        let src = *levels.last().unwrap();

        let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr) };
        assert!(!copy.is_null(), "the deep copy was refused");

        // Every level is a heap array of its own, and none of them is the
        // arena array it was copied from.
        let mut level = copy;
        for depth in 0..DEPTH {
            assert_eq!(
                unsafe { (*level).rc.memory_category() },
                MemoryCategory::GcHeap,
                "level {depth} did not leave the arena"
            );
            assert_ne!(
                level,
                levels[DEPTH - 1 - depth],
                "level {depth} is the source array itself"
            );
            let entry = unsafe { &(*level).table }.get(Key::Int(0));
            if depth == DEPTH - 1 {
                assert!(entry.is_none(), "the innermost copy holds something");
                break;
            }
            level = entry
                .expect("the copy is shallower than its source")
                .entity_ptr() as *mut LLArray;
        }

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
        }
        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// The list grows past its first chunk and hands pairs back in
    /// reverse, which is all the copy asks of it. Growth copies what was
    /// already there — losing it would drop whole subtrees of a deep copy
    /// without any assertion firing.
    #[test]
    fn the_work_list_grows_and_keeps_what_it_held() {
        let _g = crate::memory::block_pool::test_guard();
        let n = WorkList::FIRST * 3;
        let mut list = WorkList::new();
        let pair = |i: usize| (i as *mut LLArray, (i + 1000) as *mut LLArray);
        for i in 0..n {
            assert!(list.push(pair(i)), "the list refused at {i}");
        }
        assert!(list.capacity >= n);
        for i in (0..n).rev() {
            assert_eq!(list.pop(), Some(pair(i)));
        }
        assert_eq!(list.pop(), None);
        list.dispose();
    }

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
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(child as *mut RcHeader);
            (*src).table.insert(Key::Int(1), Value::int(10));
            (*src).table.insert(
                Key::Str(key),
                Value::entity(crate::value::Tag::String, child as *mut RcHeader),
            );
            (*src).table.insert(Key::Int(2), Value::int(20));
        }

        let before_key = unsafe { (*(key as *mut RcHeader)).refcount };
        let before_child = unsafe { (*(child as *mut RcHeader)).refcount };

        let dst = unsafe { separate(src, MemoryCategory::GcHeap, std::ptr::null_mut()) };
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
            let dst = separate(src, MemoryCategory::GcHeap, std::ptr::null_mut());
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
            let (storage, capacity) = (*a).table.storage_and_capacity();
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
            let (storage, capacity) = (*inner).table.storage_and_capacity();
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

    /// A copy of an attacked table is attacked. The mode is one-way on
    /// the source and `$b = $a` is the ordinary thing the language does,
    /// so a copy that starts weak hands the attacker an unescalated table
    /// whenever they want one.
    ///
    /// **The colliding set is removed before the copy, and that is the
    /// point.** While the whole set is still in the table the copy
    /// re-escalates on its own — the equal-hash trigger fires again on
    /// the ninth collider it re-inserts — so a copy made then proves
    /// nothing about carrying the state. `unset` is what makes the loss
    /// permanent: below the trigger's threshold nothing re-fires, and the
    /// table is back to the hash the attacker already knows, ready for
    /// the same flood again.
    ///
    /// Seen failing on `is_strong` for the copy.
    #[test]
    fn a_copy_of_an_escalated_table_is_escalated() {
        use crate::array::table::EQUAL_HASH_LIMIT;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

        let src = arr();
        let colliders: Vec<*mut LLString> = (0..EQUAL_HASH_LIMIT as usize + 4)
            .map(|i| {
                let s = mk(format!("collider-{i}").as_bytes());
                // Forged rather than found: constructing a set of equal
                // full hashes needs a break of the hash, and the code
                // path the attack reaches is this one.
                unsafe { (*s).hash = 0x0BAD_C0DE_0BAD_C0DE };
                unsafe {
                    crate::refcount::ll_retain(s as *mut RcHeader);
                    (*src).table.insert(Key::Str(s), Value::int(i as i64));
                }
                s
            })
            .collect();
        assert!(
            unsafe { (*src).table.is_strong() },
            "the forged set did not escalate the source, so this proves nothing"
        );

        // Leave one collider behind: far below the trigger, so nothing in
        // the copy can re-fire it.
        for s in &colliders[1..] {
            assert!(unsafe { (*src).table.remove(Key::Str(*s)) }.is_some());
            unsafe {
                // `remove` hands back the value and leaves the key's
                // reference to the caller, so two go here: the table's
                // and this test's own creation reference.
                assert!(!ll_release(*s as *mut RcHeader), "the table's");
                assert!(ll_release(*s as *mut RcHeader), "and the test's");
                crate::object::ll_entity_die(*s as *mut RcHeader);
            }
        }

        let copy = unsafe { separate(src, MemoryCategory::GcHeap, arena_ptr) };
        assert!(!copy.is_null());
        assert!(
            unsafe { (*copy).table.is_strong() },
            "the copy came back to the hash the attacker already knows"
        );
        assert_eq!(
            unsafe { (*copy).table.get(Key::Str(colliders[0])) }
                .unwrap()
                .as_int(),
            0,
            "a key was lost by the copy's own hashing"
        );

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
            assert!(ll_release(colliders[0] as *mut RcHeader));
            crate::object::ll_entity_die(colliders[0] as *mut RcHeader);
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
