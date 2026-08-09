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
use crate::journal::kinds::journal_event;
use crate::refcount::{COW, EntityKind, MemoryCategory, RcHeader, publish_header};
use crate::value::{Tag, Value};

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
pub unsafe fn ll_array_new(category: MemoryCategory) -> *mut LLArray {
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
        (&raw mut (*a).table).write(Table::empty());
        publish_header(
            a as *mut RcHeader,
            RcHeader::new(category, COW | EntityKind::Array.to_flags()),
        );
    }
    a
}

/// The memory category of `a`, and the one place it is read: the array's
/// header holds it, so nothing can hold a second copy that drifts when
/// promotion rewrites it (`dev/DECISIONS.md`, 2026-08-07). The table
/// below takes the answer as a parameter and names no entity at all,
/// which is what leaves it reusable by a second kind (S10).
///
/// A relaxed read under `rc-walk`, like every header read on a path the
/// collector may be walking: it stores bytes of that header during an
/// epoch, so a plain read is a data race rather than a stale value.
///
/// **The array is a parameter and is never derived from the table.** A
/// table sits one `RcHeader` past its array's header, so the address is
/// a subtraction away — but a reference to the body carries provenance
/// over the body alone, and this read asks for a permission a shared
/// reference cannot grant at any offset. Only Miri sees the difference;
/// every other build performs the read and reports nothing.
///
/// # Safety
/// `a` is a live array entity.
#[inline]
pub(crate) unsafe fn category_of(a: *const LLArray) -> MemoryCategory {
    MemoryCategory::from_flags(unsafe { crate::refcount::header_flags(a as *const RcHeader) })
}

impl LLArray {
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
/// (`barrier::publish_child`), which is where the two depths
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
/// permitted was recursive as well — which [`array_die`] answers the same
/// way rather than by a limit (`dev/DECISIONS.md`, 2026-08-07 and
/// 2026-08-08).
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
/// **`reason` decides one element case and nothing else**
/// ([`CopyReason`], and `element_for_copy` below): a duplication unwraps a
/// reference box the source's entry alone holds, an escape carries every
/// box across untouched. It reaches every level of the nesting, because
/// a nested array is duplicated for the same reason its root is.
///
/// # Safety
/// `src` is a live array entity; `arena` the live mounted arena, which
/// the barrier needs to count an escape or log a release at reset.
pub unsafe fn separate(
    src: *mut LLArray,
    category: MemoryCategory,
    arena: *mut crate::memory::arena::Arena,
    reason: CopyReason,
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
        if !unsafe { fill_from(s, d, arena, &mut pending, reason) } {
            // Refused part-way. Releasing the root's children cascades
            // into every copy this call published, nested ones included:
            // each is held once, by the entry naming it.
            unsafe { release_children(dst) };
            unsafe { (*dst).table.dispose(category_of(dst)) };
            pending.dispose();
            return std::ptr::null_mut();
        }
        next = pending.pop();
    }
    pending.dispose();
    dst
}

/// A destination array with the source's salt, flood state and append
/// cursor, and no entries. Null when the allocation is refused.
///
/// The flood state goes in before the first insert, because it decides
/// how a key is hashed: a copy that starts weak re-installs an
/// attacker's whole collision set under the hash the source escalated
/// away from.
///
/// # Safety
/// `src` is a live array entity.
unsafe fn new_empty_copy(src: *mut LLArray, category: MemoryCategory) -> *mut LLArray {
    let dst = unsafe { ll_array_new(category) };
    if dst.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { (*dst).table.adopt_flood_state(&(*src).table) };
    unsafe { (*dst).table.adopt_append_state(&(*src).table) };
    dst
}

/// Why a copy is being made, which decides one thing and only one: a
/// duplication collapses a reference nobody else names, and a
/// relocation out of the dying arena does not.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CopyReason {
    /// `$b = $a` followed by a write — PHP's own duplication, and the
    /// one place it collapses a reference.
    Duplication,
    /// A value crossing out of the request arena into a longer-lived
    /// holder. The program stores, it does not duplicate: the copy is
    /// owed by the lifetime rather than by sharing, so an element's
    /// reference state is carried across unchanged.
    Escape,
}

/// What the copy takes for an element of the source: the element
/// itself, except that a **reference box with a single holder is
/// unwrapped** and the copy takes the value behind it.
///
/// This is where PHP collapses a reference, and it is the only place it
/// does. Measured against php 8.3.6: `unset($r)` does not collapse the
/// element, nor does a write to it, nor a write to another element —
/// the source still reads `reference refcount(1)` after each. The
/// duplication is what collapses it, and `zend_array_dup_element` is the
/// same rule (`rfc/model/arrays-hashtable.md`, "Element states").
///
/// One holder means the source's own entry and nobody else, so no name
/// in the program can observe the two arrays sharing the element. Two or
/// more means a live `&` binding, and then the copy shares the box —
/// which is how `$r = &$a['x']; $b = $a; $r = 3;` reaches `$b['x']`.
/// The count is exact because a box is a heap entity in every case
/// (`dev/DECISIONS.md`, 2026-08-08); that is what the ruling bought.
///
/// **Only a duplication collapses anything.** An escape copy is a store
/// crossing a lifetime boundary, and PHP duplicates nothing there, so a
/// box travels with the element and both arrays go on naming it — which
/// is also what keeps the box's identity, the property `escape_copy`'s
/// own contract rests on.
///
/// # Safety
/// `element` is a live entry's value.
unsafe fn element_for_copy(element: Value, reason: CopyReason) -> Value {
    if reason != CopyReason::Duplication || element.tag() != crate::value::Tag::Reference {
        return element;
    }
    let boxed = element.entity_ptr();
    if unsafe { crate::refcount::header_refcount(boxed) } != 1 {
        return element;
    }
    unsafe { (*(boxed as *const crate::reference::LLReference)).value }
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
/// `reason` is the root call's, carried down every level unchanged, and
/// only `element_for_copy` reads it.
///
/// # Safety
/// `src` and `dst` are live arrays, `dst` empty; `arena` the live
/// mounted arena.
unsafe fn fill_from(
    src: *mut LLArray,
    dst: *mut LLArray,
    arena: *mut crate::memory::arena::Arena,
    pending: &mut WorkList<(*mut LLArray, *mut LLArray)>,
    reason: CopyReason,
) -> bool {
    let category = unsafe { category_of(dst) };
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
        let mut v = unsafe { element_for_copy(e.value(), reason) };
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
                    unsafe { (*copy).table.dispose(category_of(copy)) };
                    return false;
                }
                v = Value::entity(v.tag(), copy as *mut RcHeader);
            } else {
                match unsafe { crate::memory::barrier::publish_child(arena, category, v) } {
                    Some(published) => v = published,
                    None => return false,
                }
            }
        }
        let published_key = match unsafe { publish_key(arena, category, key) } {
            Some(published) => published,
            None => {
                unsafe { give_value_back(category, &v) };
                return false;
            }
        };
        if unsafe { (*dst).table.insert(category_of(dst), published_key, v) }.is_none() {
            // Out of memory part-way. Give back what this element took —
            // through the barrier, key and value alike; the source is
            // untouched.
            unsafe { give_value_back(category, &v) };
            if let Key::Str(k) = published_key {
                unsafe { crate::memory::barrier::drop_ref(category, k as *mut RcHeader) };
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

/// The work a nesting-deep operation still owes, and the reason it is
/// generic: the deep copy queues `(source, destination)` pairs, the
/// teardown queues [`Pending`] lines, and both need the same refusable
/// growth in the same memory.
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
struct WorkList<T> {
    items: *mut T,
    len: usize,
    capacity: usize,
    /// Bytes granted, which is what the free needs: the buffer arena's
    /// free is size-carrying and a chunk holds no metadata.
    granted: usize,
}

impl<T: Copy> WorkList<T> {
    /// The items a first growth makes room for. Deep enough that
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

    /// False when the chunk could not grow. What the caller does with
    /// that differs and neither answer is this type's: the copy refuses
    /// and unwinds what it published, the teardown drops that one child
    /// onto the recursive path, since an entity at count zero has nowhere
    /// to refuse to.
    fn push(&mut self, item: T) -> bool {
        if self.len == self.capacity && !self.grow() {
            return false;
        }
        unsafe { self.items.add(self.len).write(item) };
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(unsafe { self.items.add(self.len).read() })
    }

    /// Reverse everything pushed since `base`, so that a LIFO drain hands
    /// the segment back in the order it was pushed. The teardown's whole
    /// claim to Zend's destructor order rests on this call
    /// ([`array_die`]); the copy never uses it, order being nothing to a
    /// copy.
    fn reverse_from(&mut self, base: usize) {
        let (mut low, mut high) = (base, self.len);
        while low + 1 < high {
            high -= 1;
            unsafe { std::ptr::swap(self.items.add(low), self.items.add(high)) };
            low += 1;
        }
    }

    fn grow(&mut self) -> bool {
        let capacity = if self.capacity == 0 {
            Self::FIRST
        } else {
            self.capacity * 2
        };
        let bytes = capacity * size_of::<T>();
        let (fresh, granted) = unsafe {
            crate::memory::routing::body_alloc(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes)
        };
        if fresh.is_null() {
            return false;
        }
        const {
            assert!(
                align_of::<T>() <= 8,
                "the buffer arena hands out 8-aligned chunks"
            )
        };
        let fresh = fresh as *mut T;
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

/// Give a value's published reference back on a refusal path — through
/// the barrier's `drop_ref` with the destination's category, never a
/// bare release. The publication may have logged the release (a heap
/// child entering an arena destination) or counted an escape (a non-COW
/// arena child entering a longer-lived one), and each of those is
/// `drop_ref`'s to settle: a bare release double-frees the first at the
/// reset and leaves the second counted forever.
#[inline]
pub(crate) unsafe fn give_value_back(category: MemoryCategory, v: &Value) {
    if v.is_refcounted() {
        unsafe { crate::memory::barrier::drop_ref(category, v.entity_ptr()) };
    }
}

/// Publish a key's string entity for a table of `category`, the operation
/// [`give_value_back`] undoes on a refusal path. An integer key carries
/// no reference and comes back unchanged; `None` is the refused copy,
/// with nothing spent.
///
/// A string key is a string entity making the element's crossing, so the
/// publication is the element's — `barrier::publish_child` under
/// `Tag::String` — and the wrap is what lets one body serve both halves
/// of an entry.
///
/// # Safety
/// `key`'s string, if any, live; `arena` the live mounted arena.
#[inline]
pub(crate) unsafe fn publish_key(
    arena: *mut crate::memory::arena::Arena,
    category: MemoryCategory,
    key: Key,
) -> Option<Key> {
    let Key::Str(s) = key else {
        return Some(key);
    };
    let published = unsafe {
        crate::memory::barrier::publish_child(
            arena,
            category,
            Value::entity(Tag::String, s as *mut RcHeader),
        )
    }?;
    Some(Key::Str(
        published.entity_ptr() as *mut crate::string::LLString
    ))
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
    unsafe { (*a).table.carry_out_of(category_of(a), arena) }
}

/// Teardown for an array whose count reached zero, or that a collector
/// owns: children first, then the storage, then the entity's own memory.
///
/// The order is forced rather than chosen.
/// [`release_children_in_order`] reads the storage to find the children,
/// so the storage cannot already be gone;
/// and a child's death can run user code, which must not meet a table
/// half-disposed. The entity's own memory follows the same rule as a
/// string's: only the GC heap frees here, an arena entity dying with its
/// reset and an immortal one not dying at all.
///
/// **Nesting is drained, not recursed**, and for the reason the copy above
/// takes the same shape: depth is the caller's input — `$deep = [[[…]]]`
/// and then one release — so a frame set per level is a stack overflow,
/// which the guard page turns into a dead process with no unwinding and
/// no record. A nested array whose last reference this teardown drops is
/// pushed onto a list and torn down by this call's own loop. The list
/// lives in a buffer-arena chunk.
///
/// **Destructors keep Zend's order**, which is depth first and, inside a
/// level, the order the entries were inserted in: `[[$b], $a]` runs
/// `$b`'s destructor before `$a`'s, exactly as the recursion did. That
/// order is a contract on this path (`dev/DECISIONS.md`, 2026-08-08);
/// the collector and the arena reset order their own destructors, as
/// Zend's GC and its shutdown do.
///
/// Holding it costs the list one more kind of line and no cursor into the
/// table. Until a dying nested array turns up, children are released
/// where they are found, so a flat array pushes nothing. The first one
/// turns the rest of that level into **held** lines: their release is
/// postponed, the reference the freed storage held passes to the list,
/// and the segment is reversed so that the LIFO drain hands it back in
/// entry order — the nephews before the next sibling. A held line carries
/// its owner's category, because that is what settles the escape ledger
/// and the release-at-reset log, and one list mixes the children of
/// several owners.
///
/// # Safety
/// `a` must be a live array entity.
pub(crate) unsafe fn array_die(a: *mut LLArray) {
    let mut pending: WorkList<Pending> = WorkList::new();
    let mut dying = a;
    loop {
        // Inside the loop rather than above it: the drain tears down
        // every nested array here, and those pass no other death door —
        // `ll_entity_die` sees the outermost one only.
        journal_event!(
            crate::journal::kinds::KIND_ENTITY_DEATH,
            dying as u64,
            EntityKind::Array as u64,
            0
        );
        unsafe { release_children_in_order(dying, &mut pending) };
        unsafe { (*dying).table.dispose(category_of(dying)) };
        if unsafe { crate::object::header_category(dying as *const RcHeader) }
            == MemoryCategory::GcHeap
        {
            unsafe { crate::memory::stdapi::ll_free(dying as *mut u8) };
        }

        let mut next = None;
        while let Some(line) = pending.pop() {
            match line {
                Pending::DeadArray(array) => {
                    next = Some(array);
                    break;
                }
                Pending::HeldChild(child, owner_cat) => {
                    let dead =
                        unsafe { crate::memory::barrier::drop_ref_deferred(owner_cat, child) };
                    if dead.is_null() {
                        continue;
                    }
                    if unsafe { is_array(dead) } {
                        unsafe { leave_the_candidate_buffer(dead) };
                        next = Some(dead as *mut LLArray);
                        break;
                    }
                    unsafe { crate::object::ll_entity_die(dead) };
                }
            }
        }
        match next {
            Some(array) => dying = array,
            None => break,
        }
    }
    pending.dispose();
}

/// One line of the teardown's list.
#[derive(Clone, Copy)]
enum Pending {
    /// An array at count zero whose children are still counted, waiting
    /// its turn in the drain.
    DeadArray(*mut LLArray),
    /// A child whose release is postponed so that an earlier sibling's
    /// subtree runs its destructors first. It is still counted by a
    /// storage that may already be freed, and the category is its
    /// owner's — [`crate::memory::barrier::drop_ref_deferred`] reads that
    /// to settle the escape ledger and the release-at-reset log.
    HeldChild(*mut RcHeader, MemoryCategory),
}

/// [`release_children`] for the drain: the cascade becomes lines on
/// `pending`, and the order the recursion produced is kept. It sits
/// beside that function rather than replacing it, every other caller
/// wanting the cascade.
///
/// **Refusal.** A list that cannot grow leaves the child it could not
/// take on the recursive path. Every level below it asks the same
/// allocator and can be refused again, so while the exhaustion lasts the
/// depth is the one this drain exists to bound. A held child released
/// that way also runs its destructors ahead of the subtree deferred
/// before it, so a refused chunk costs the order as well. A teardown has
/// no channel to refuse through — the array is already at count zero —
/// and the alternative to both is leaking the subtree.
///
/// # Safety
/// `a` is a live array entity whose children are still counted.
unsafe fn release_children_in_order(a: *mut LLArray, pending: &mut WorkList<Pending>) {
    let owner_cat = unsafe { crate::object::header_category(a as *const RcHeader) };
    let base = pending.len;
    let mut deferring = false;
    unsafe {
        for_each_counted_child(a, |child| {
            if deferring && pending.push(Pending::HeldChild(child, owner_cat)) {
                return;
            }
            let dead = crate::memory::barrier::drop_ref_deferred(owner_cat, child);
            if dead.is_null() {
                return;
            }
            if is_array(dead) && pending.push(Pending::DeadArray(dead as *mut LLArray)) {
                leave_the_candidate_buffer(dead);
                deferring = true;
                return;
            }
            crate::object::ll_entity_die(dead);
        })
    };
    pending.reverse_from(base);
}

/// The kind of an entity whose teardown is owed, read from the flags the
/// release left behind.
///
/// # Safety
/// `entity` is an entity at count zero whose memory is still there.
#[inline]
unsafe fn is_array(entity: *mut RcHeader) -> bool {
    use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind};
    let flags = unsafe { crate::refcount::header_flags(entity) };
    (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT == EntityKind::Array as u32
}

/// The duty [`crate::object::ll_entity_die`]'s door performs that the
/// drain takes over along with the array: under rc-trace a dying entity
/// leaves the candidate buffer, or a later collection reads a slot that
/// is gone. Under rc-walk the door has no such duty and neither has this.
///
/// # Safety
/// `entity` is an entity at count zero, taken over by the drain.
#[cfg(not(feature = "rc-walk"))]
#[inline]
unsafe fn leave_the_candidate_buffer(entity: *mut RcHeader) {
    let flags = unsafe { crate::refcount::header_flags(entity) };
    if flags & crate::refcount::CYCLE_COLLECTOR_BUFFERED != 0 {
        unsafe { crate::gc::forget_candidate(entity) };
    }
}

#[cfg(feature = "rc-walk")]
#[inline]
unsafe fn leave_the_candidate_buffer(_entity: *mut RcHeader) {}

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

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
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
                category_of(src),
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

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let element =
            unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"in the arena") };
        unsafe {
            crate::refcount::ll_retain(element as *mut RcHeader);
            (*src).table.insert(
                category_of(src),
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
        let copied_element = unsafe { (*stored).table.entry(0).value().entity_ptr() };
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
    /// The depth is modest because what is measured here is the copy.
    /// Teardown of the copy is drained rather than recursed since
    /// 2026-08-08, and the depth that bound is measured at lives in
    /// `a_deep_array_tears_down_without_the_machine_stack` below, on a
    /// thread whose stack is small enough for the answer to mean
    /// something.
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
        let innermost = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        levels.push(innermost);
        for _ in 1..DEPTH {
            let outer = unsafe { ll_array_new(MemoryCategory::RequestArena) };
            let inner = *levels.last().unwrap();
            unsafe {
                crate::refcount::ll_retain(inner as *mut RcHeader);
                (*outer).table.insert(
                    category_of(outer),
                    Key::Int(0),
                    Value::entity(crate::value::Tag::Array, inner as *mut RcHeader),
                );
            }
            levels.push(outer);
        }
        let src = *levels.last().unwrap();

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };
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

    /// Teardown of a deep array is a drain, not a recursion. The depth is
    /// the caller's input — `$deep = [[[…]]]`, then one release — and a
    /// frame set per level overflows the stack, which no arm of this
    /// crate can catch: the guard page kills the process with no
    /// unwinding and no record.
    ///
    /// Measured on a thread whose stack is deliberately small, because a
    /// depth that overflows the ordinary 8 MiB one costs minutes to build
    /// and dominates the Miri run. 64 KiB against 2 000 levels leaves
    /// under 33 bytes a level, and the smallest frame set here is far
    /// above that; what the drain spends is a fixed frame and one list
    /// entry, so the margin is total rather than per level. Verified by
    /// forcing the list to refuse (`FORCE_REFUSE_LONGLIVED`), which
    /// returns the teardown to the recursive path: the thread dies on the
    /// guard page at this depth.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "2000 levels of build and teardown is minutes under Miri, which would\
                  dominate the whole-suite run; the drain's UB surface is covered by the\
                  destructor-order tests and the deep-copy test, which Miri does run"
    )]
    fn a_deep_array_tears_down_without_the_machine_stack() {
        const DEPTH: usize = 2_000;
        const STACK: usize = 64 * 1024;

        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(|| {
                let _g = crate::memory::block_pool::test_guard();

                // `[[[…]]]`, built from the inside out and iteratively:
                // each level holds the one below it under key 0, so the
                // outermost is the chain's only holder. The creation
                // reference `ll_array_new` returns is what the entry
                // takes, so no level is retained twice and none is
                // released here.
                let mut level = unsafe { ll_array_new(MemoryCategory::GcHeap) };
                for _ in 1..DEPTH {
                    let outer = unsafe { ll_array_new(MemoryCategory::GcHeap) };
                    unsafe {
                        (*outer).table.insert(
                            category_of(outer),
                            Key::Int(0),
                            Value::entity(crate::value::Tag::Array, level as *mut RcHeader),
                        );
                    }
                    level = outer;
                }

                // The chain is the thing under test, so its depth is
                // asserted rather than assumed: a refused insert would
                // leave a shallow array that tears down on any stack.
                let mut built = 1;
                let mut walk = level;
                while let Some(v) = unsafe { &(*walk).table }.get(Key::Int(0)) {
                    walk = v.entity_ptr() as *mut LLArray;
                    built += 1;
                }
                assert_eq!(built, DEPTH, "the chain is shallower than it was built");

                unsafe {
                    assert!(
                        ll_release(level as *mut RcHeader),
                        "the outermost array had a second holder"
                    );
                    crate::object::ll_entity_die(level as *mut RcHeader);
                }
            })
            .expect("the small-stack thread did not start")
            .join()
            .expect("teardown of the deep array killed its thread");
    }

    /// The order `__destruct` bodies run in when a nested array dies is
    /// Zend's: depth first, and inside a level the order the entries were
    /// inserted in. The drain has to reproduce it, because a program
    /// observes it — a destructor writes a log, closes a handle, or reads
    /// another object that is about to die.
    ///
    /// `[[$b], $a]`: `$b` is one level down and first in the entry order,
    /// so it goes first. Seen failing as `AB` on the drain's first shape,
    /// which released `$a` where it found it and left the nested array
    /// for the pop.
    #[test]
    fn a_nested_destructor_runs_before_a_later_sibling() {
        assert_eq!(destructor_order(Shape::NestedThenObject), "BA");
    }

    /// Two nested arrays, so the reversal of the held segment is what is
    /// under test rather than the interleaving: `[[$b], [$c]]` runs `$b`
    /// before `$c`. Seen failing as `CB`, the LIFO order of the pushes.
    #[test]
    fn nested_siblings_run_their_destructors_in_entry_order() {
        assert_eq!(destructor_order(Shape::TwoNested), "BC");
    }

    /// The mixed case both of the above are corners of:
    /// `[$1, [$2, [$3], $4], $5]` runs `1 2 3 4 5`. It exercises a held
    /// segment inside a held segment, which is where a reversal that
    /// reversed the whole list rather than the segment would show.
    #[test]
    fn a_mixed_nesting_runs_its_destructors_in_zend_order() {
        assert_eq!(destructor_order(Shape::Mixed), "12345");
    }

    /// The refusal path, which is the exhaustion path: a list that cannot
    /// grow drops each child it could not take onto the recursive one.
    /// What must survive that is the outcome — everything is torn down,
    /// in Zend's order — and what does not is the bound on depth, which
    /// is why this shape is three levels rather than two thousand.
    ///
    /// It was untestable until `FORCE_REFUSE_LONGLIVED` existed for the
    /// refused carry (S4.2); the plan recorded it as owed on the strength
    /// of `FORCE_OOM` alone, which the buffer arena can go around.
    #[test]
    fn a_refused_list_still_tears_everything_down_in_order() {
        assert_eq!(destructor_order_with(Shape::Mixed, true), "12345");
    }

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

    /// Build one of the shapes out of heap arrays and recording objects,
    /// release the outermost array, and return what the destructors
    /// wrote. Every entity is created at +1 and handed to the entry that
    /// takes it, so the outermost array is the only holder.
    fn destructor_order(shape: Shape) -> String {
        destructor_order_with(shape, false)
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
            (*owner)
                .table
                .insert(category_of(owner), Key::Int(key), Value::entity(tag, child));
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

    /// The list grows past its first chunk and hands pairs back in
    /// reverse, which is all the copy asks of it. Growth copies what was
    /// already there — losing it would drop whole subtrees of a deep copy
    /// without any assertion firing.
    #[test]
    fn the_work_list_grows_and_keeps_what_it_held() {
        let _g = crate::memory::block_pool::test_guard();
        type Pairs = WorkList<(*mut LLArray, *mut LLArray)>;
        let n = Pairs::FIRST * 3;
        let mut list = Pairs::new();
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
        unsafe { ll_array_new(MemoryCategory::GcHeap) }
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
            assert_eq!(category_of(a), MemoryCategory::GcHeap);
            assert!((*a).table.is_empty());
            (*a).table.dispose(category_of(a));
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
            (*a).table.dispose(category_of(a));
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
            (*src)
                .table
                .insert(category_of(src), Key::Int(1), Value::int(10));
            (*src).table.insert(
                category_of(src),
                Key::Str(key),
                Value::entity(crate::value::Tag::String, child as *mut RcHeader),
            );
            (*src)
                .table
                .insert(category_of(src), Key::Int(2), Value::int(20));
        }

        let before_key = unsafe { (*(key as *mut RcHeader)).refcount };
        let before_child = unsafe { (*(child as *mut RcHeader)).refcount };

        let dst = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                std::ptr::null_mut(),
                CopyReason::Duplication,
            )
        };
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
            (*dst)
                .table
                .insert(category_of(dst), Key::Int(1), Value::int(999));
            assert_eq!((*src).table.get(Key::Int(1)).unwrap().as_int(), 10);
            assert_eq!((*dst).table.get(Key::Int(1)).unwrap().as_int(), 999);

            release_children(dst);
            (*dst).table.dispose(category_of(dst));
            release_children(src);
            (*src).table.dispose(category_of(src));
        }
    }

    #[test]
    fn separation_carries_holes_away_rather_than_copying_them() {
        let _g = crate::memory::block_pool::test_guard();
        let src = arr();
        unsafe {
            for i in 0..10i64 {
                (*src)
                    .table
                    .insert(category_of(src), Key::Int(i), Value::int(i));
            }
            for i in [2i64, 5, 8] {
                let _ = (*src).table.remove(Key::Int(i));
            }
            let dst = separate(
                src,
                MemoryCategory::GcHeap,
                std::ptr::null_mut(),
                CopyReason::Duplication,
            );
            assert!(!dst.is_null());
            assert_eq!((*dst).table.len(), 7);
            assert_eq!(
                (*dst).table.used(),
                7,
                "the copy starts dense: a hole is not worth copying"
            );
            let order: Vec<i64> = (*dst).table.iter().map(|e| e.hash_or_key as i64).collect();
            assert_eq!(order, vec![0, 1, 3, 4, 6, 7, 9]);

            (*dst).table.dispose(category_of(dst));
            (*src).table.dispose(category_of(src));
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
                category_of(a),
                Key::Str(key),
                Value::entity(crate::value::Tag::String, child as *mut RcHeader),
            );
            let k0 = (*(key as *mut RcHeader)).refcount;
            let c0 = (*(child as *mut RcHeader)).refcount;

            release_children(a);
            assert_eq!((*(key as *mut RcHeader)).refcount, k0 - 1);
            assert_eq!((*(child as *mut RcHeader)).refcount, c0 - 1);

            (*a).table.dispose(category_of(a));
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
                category_of(a),
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
            (*inner)
                .table
                .insert(category_of(inner), Key::Int(1), Value::int(1));
            let (storage, capacity) = (*inner).table.storage_and_capacity();
            assert!(!storage.is_null(), "the inner array has storage to reclaim");

            // The inner array's only reference is the outer array's
            // element, so the outer's death is the inner's death.
            (*outer).table.insert(
                category_of(outer),
                Key::Int(0),
                Value::entity(crate::value::Tag::Array, inner as *mut RcHeader),
            );

            assert!(ll_release(outer as *mut RcHeader));
            crate::object::ll_entity_die(outer as *mut RcHeader);

            // Both tables are freed by this teardown, the outer one
            // first: the drain disposes a level before it takes the next
            // one off the list. Which of the two the free list hands back
            // first is the allocator's business, so the assertion takes
            // either — what it is here to catch is the inner storage
            // never coming back at all.
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
                    (*src)
                        .table
                        .insert(category_of(src), Key::Str(s), Value::int(i as i64));
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
            // `remove` hands the stored key back with the value — the
            // table's one reference per stored key — so the table's
            // reference is released through what came back and the
            // creation reference through the test's own pointer.
            let (_, key) = unsafe { (*src).table.remove(Key::Str(*s)) }.unwrap();
            assert_eq!(key, *s, "the entry held the inserted key entity");
            unsafe {
                assert!(!ll_release(key as *mut RcHeader), "the table's");
                assert!(ll_release(*s as *mut RcHeader), "and the test's");
                crate::object::ll_entity_die(*s as *mut RcHeader);
            }
        }

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };
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

    /// A copy of a table whose salt the first rung drew indexes exactly
    /// as the source does. The bit without the number would mean
    /// `mix_int(k, 0)` — a mix every attacker computes offline — and a
    /// fresh draw would break the ladder's bound: a copy's second long
    /// chain must escalate, not rebuild again. Seen failing on the salt
    /// equality.
    #[test]
    fn a_copy_of_a_reseeded_table_inherits_the_drawn_salt() {
        use crate::array::table::CHAIN_LIMIT;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

        let src = arr();
        for i in 0..(CHAIN_LIMIT as i64 + 1) {
            unsafe {
                (*src)
                    .table
                    .insert(category_of(src), Key::Int(i * 1024), Value::int(i));
            }
        }
        assert!(
            unsafe { (*src).table.is_reseeded() },
            "the stride flood did not fire the rung, so this proves nothing"
        );
        let drawn = unsafe { (*src).table.salt() };

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };
        assert!(!copy.is_null());
        assert!(unsafe { (*copy).table.is_reseeded() });
        assert_eq!(
            unsafe { (*copy).table.salt() },
            drawn,
            "the copy indexes under a salt of its own"
        );
        for i in 0..(CHAIN_LIMIT as i64 + 1) {
            assert_eq!(
                unsafe { (*copy).table.get(Key::Int(i * 1024)) }
                    .unwrap()
                    .as_int(),
                i
            );
        }

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
        }
    }

    /// The third state a copy can inherit: nothing. A copy of an
    /// unsalted source starts unsalted — by-value integer indexing, no
    /// mix — and stays a full citizen of the ladder: its own flood fires
    /// its own first rung, drawing a salt of its own.
    #[test]
    fn a_copy_of_an_unsalted_table_is_unsalted_and_climbs_its_own_ladder() {
        use crate::array::table::CHAIN_LIMIT;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

        let src = arr();
        for i in 0..3i64 {
            unsafe {
                (*src)
                    .table
                    .insert(category_of(src), Key::Int(i * 1024), Value::int(i));
            }
        }
        assert!(unsafe { !(*src).table.is_reseeded() });

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };
        assert!(!copy.is_null());
        assert!(
            unsafe { !(*copy).table.is_reseeded() },
            "a copy of an unsalted table drew a salt from nowhere"
        );

        for i in 3..(CHAIN_LIMIT as i64 + 1) {
            unsafe {
                (*copy)
                    .table
                    .insert(category_of(copy), Key::Int(i * 1024), Value::int(i));
            }
        }
        assert!(
            unsafe { (*copy).table.is_reseeded() },
            "the copy's own flood must fire the copy's own rung"
        );
        for i in 0..(CHAIN_LIMIT as i64 + 1) {
            assert_eq!(
                unsafe { (*copy).table.get(Key::Int(i * 1024)) }
                    .unwrap()
                    .as_int(),
                i
            );
        }

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
        }
    }

    /// S2.2's ownership rule, both arms measured. Storing a new string
    /// key consumes the caller's reference; the overwrite arm keeps the
    /// entry's original key, so the caller's reference stays the
    /// caller's; removing hands the stored key's reference back. Two
    /// distinct entities with equal bytes, because one entity can
    /// measure only one arm: the stored key catches the remove leak, the
    /// overwriting key catches the stranded retain.
    #[test]
    fn a_stored_key_is_consumed_and_a_dropped_key_comes_back() {
        let _g = crate::memory::block_pool::test_guard();
        let a = mk(b"key");
        let b = mk(b"key");
        assert_ne!(a, b, "two distinct entities, or neither arm is measured");
        let e = arr();
        let category = unsafe { category_of(e) };
        let a0 = unsafe { (*a).rc.refcount };
        let b0 = unsafe { (*b).rc.refcount };

        unsafe {
            crate::refcount::ll_retain(a as *mut RcHeader);
            let (added, old) = (*e)
                .table
                .insert(category, Key::Str(a), Value::int(1))
                .unwrap();
            assert!(added, "the first insert stores a new key");
            assert!(old.is_none());

            crate::refcount::ll_retain(b as *mut RcHeader);
            let (added, old) = (*e)
                .table
                .insert(category, Key::Str(b), Value::int(2))
                .unwrap();
            assert!(!added, "equal bytes overwrite rather than add");
            assert_eq!(old.unwrap().as_int(), 1);
            // `added == false`: the caller's key was not stored, so the
            // retain above is still the caller's to give back.
            assert!(!ll_release(b as *mut RcHeader));

            let (v, key) = (*e).table.remove(Key::Str(b)).unwrap();
            assert_eq!(v.as_int(), 2);
            assert_eq!(key, a, "the entry kept its original key entity");
            assert!(!ll_release(key as *mut RcHeader), "the table's reference");

            assert_eq!((*a).rc.refcount, a0, "the stored key's references balance");
            assert_eq!(
                (*b).rc.refcount,
                b0,
                "the overwriting key's references balance"
            );

            assert!(ll_release(e as *mut RcHeader));
            crate::object::ll_entity_die(e as *mut RcHeader);
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_entity_die(a as *mut RcHeader);
            assert!(ll_release(b as *mut RcHeader));
            crate::object::ll_entity_die(b as *mut RcHeader);
        }
    }

    /// A refusal mid-copy gives a published child back through the
    /// barrier, and the difference from a bare release is an escape
    /// hold-count: the copy's barrier counted the non-COW arena child as
    /// escaping into a heap destination, so the giveback must
    /// `escape_lose` it — a bare `ll_release` no-ops on an arena entity
    /// and leaves the count stuck, and the reset then treats a child
    /// nobody holds as an escapee. Seen failing on the escapee flag.
    #[test]
    fn a_refused_heap_copy_gives_an_escaped_child_back_through_the_barrier() {
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        // Warm a heap entity block, so the forced refusal below lands on
        // the copy's table storage and not on the copy's own slot.
        let warm = arr();

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let d = unsafe {
            crate::string::ll_string_new_dynamic(context_ptr, MemoryCategory::RequestArena, b"p", 0)
        };
        assert!(!d.is_null());
        unsafe {
            crate::refcount::ll_retain(d as *mut RcHeader);
            (*src).table.insert(
                category_of(src),
                Key::Int(0),
                Value::entity(crate::value::Tag::String, d as *mut RcHeader),
            );
            crate::refcount::ll_retain(src as *mut RcHeader);
        }

        FORCE_OOM.store(true, Ordering::Relaxed);
        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };
        FORCE_OOM.store(false, Ordering::Relaxed);
        assert!(
            copy.is_null(),
            "the copy was meant to be refused and was not"
        );

        unsafe {
            assert_eq!(
                crate::refcount::header_flags(d as *const RcHeader) & crate::refcount::IS_ESCAPEE,
                0,
                "the refused copy left the child counted as an escapee"
            );
            crate::refcount::ll_release(src as *mut RcHeader);
            assert!(ll_release(warm as *mut RcHeader));
            crate::object::ll_entity_die(warm as *mut RcHeader);
        }
        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// The ownership rule's cross-category half: in an arena table a
    /// heap key's one release is owed by the reset log — the barrier
    /// records it at publication — so the caller gives the returned key
    /// up through `drop_ref`, which leaves log-owned references alone.
    /// A bare `ll_release` there is the double free `Table::remove`'s
    /// contract names: the reset's own release then drives the string to
    /// death while the test still holds it. Seen failing exactly that
    /// way.
    #[test]
    fn an_arena_tables_key_release_is_owed_by_the_reset_log() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let key = mk(b"heap key in an arena table");
        let e = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let category = unsafe { category_of(e) };

        unsafe {
            crate::refcount::ll_retain(key as *mut RcHeader);
            let published = crate::memory::barrier::store_category_barrier(
                arena_ptr,
                MemoryCategory::RequestArena,
                key as *mut RcHeader,
            );
            assert_eq!(
                published, key as *mut RcHeader,
                "a heap entity entering an arena slot is logged, not copied"
            );
            let (added, old) = (*e)
                .table
                .insert(category, Key::Str(key), Value::int(1))
                .unwrap();
            assert!(added);
            assert!(old.is_none());

            let (v, k) = (*e).table.remove(Key::Str(key)).unwrap();
            assert_eq!(v.as_int(), 1);
            assert_eq!(k, key);
            // The table's reference is the log's to release at reset;
            // `drop_ref` knows that where a bare release would not.
            crate::memory::barrier::drop_ref(MemoryCategory::RequestArena, k as *mut RcHeader);
            assert_eq!(
                (*key).rc.refcount,
                2,
                "the log still holds its one reference"
            );
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});

        unsafe {
            assert_eq!(
                (*key).rc.refcount,
                1,
                "the reset's one release balanced the barrier's one record"
            );
            assert!(ll_release(key as *mut RcHeader));
            crate::object::ll_entity_die(key as *mut RcHeader);
        }
    }

    /// The append cursor survives the copy, where the replay cannot
    /// carry it: `fill_from` copies live entries only, so a hole under
    /// the highest key ever inserted has no witness in the copy — PHP
    /// appends `[9 => 'x']` minus its 9 at 10, and a copy that answered
    /// 0 would hand back keys the source already spent.
    #[test]
    fn a_copy_inherits_the_append_cursor_over_a_hole() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

        let src = arr();
        unsafe {
            (*src)
                .table
                .insert(category_of(src), Key::Int(9), Value::int(1));
            let _ = (*src).table.remove(Key::Int(9));
            assert_eq!((*src).table.append_key(), Some(10));
        }

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };
        assert!(!copy.is_null());
        unsafe {
            assert_eq!((*copy).table.len(), 0, "a hole is not worth copying");
            assert_eq!(
                (*copy).table.append_key(),
                Some(10),
                "the copy rewound the append cursor past a removed key"
            );
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
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
