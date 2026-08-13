//! The `array` entity: a refcounted header over the storage, in
//! whichever of the two representations it currently holds.
//!
//! ```text
//! +0   RcHeader     (kind = Array, COW set)
//! +8   StorageHead  every word a concurrent walker reads
//! +48  Storage      the storage handle, in one of its representations
//! ```
//!
//! **The head is the entity's and not the representation's**, which is the
//! type rule every access path here exists to hold: a mutating operation
//! is reached through `&mut (*a).storage`, and a `&mut` covers its whole
//! range whatever is inside it (`crate::array::head`). [`as_table_mut`]
//! and [`as_vector_mut`] are the only places that derive the disjoint
//! pair, which is what makes the rule checkable rather than remembered.
//!
//! **No per-instance class pointer**, the same construction as a string
//! (`rfc/model/arrays.md`, "a single final class … no per-instance class
//! pointer, devirtualized methods"). The table holds no header either, so
//! this entity supplies the refcount, the memory category and the COW
//! state, and takes the category as a parameter at every allocating call
//! (`dev/DECISIONS.md`, "the table is handed its category and reads no
//! header").
//!
//! **COW separation is shallow and the store barrier's escape copy is
//! deep** — two operations, not one (`dev/DECISIONS.md`, "the array
//! entity, and separation is the shallow copy").

use crate::array::head::{StorageHead, StorageTag};
use crate::array::table::{Key, Table};
use crate::array::vector::Vector;
use crate::journal::kinds::journal_event;
use crate::refcount::{COW, EntityKind, MemoryCategory, RcHeader, publish_header};
use crate::value::{Tag, Value};

/// The array's storage, in whichever representation it currently has —
/// the private tail of one, the head beside it being the entity's.
///
/// A union rather than an enum, because the discriminant already exists
/// and is not Rust's: it is the tag in the [`StorageHead`] at
/// `LLArray + 8`, where a walker can read it atomically
/// (`crate::array::head`). A Rust enum would put a second discriminant
/// beside the first, and the walker cannot read that one at all.
///
/// **Nothing here drops.** Storage is given back by [`dispose_storage`],
/// at a moment the teardown order fixes, so the members are
/// `ManuallyDrop` and reading the wrong one is the caller's error rather
/// than a state this type recovers from.
///
/// The members are private to this module and the projections below are
/// unsafe, which is how the head's placement stays a rule rather than a
/// habit: nothing outside can take a `&mut` over a representation
/// without going through [`as_table_mut`] or [`as_vector_mut`].
#[repr(C)]
pub union Storage {
    vector: std::mem::ManuallyDrop<Vector>,
    table: std::mem::ManuallyDrop<Table>,
}

impl Storage {
    /// An empty ordered hash. The tag saying so is written into the head
    /// by [`new_with_storage`], which is where a *fresh* pair is matched;
    /// [`migrate_to_hash`] is the only other place, and it matches a
    /// filled one.
    pub const fn hash() -> Self {
        Storage {
            table: std::mem::ManuallyDrop::new(Table::empty()),
        }
    }

    /// An empty mixed vector.
    pub const fn vector() -> Self {
        Storage {
            vector: std::mem::ManuallyDrop::new(Vector::empty()),
        }
    }

    /// An ordered hash that already holds entries, for the one operation
    /// that replaces a representation rather than creating one
    /// ([`migrate_to_hash`]). The tag saying so is written by that
    /// operation, inside the same window as this.
    pub(crate) fn of_table(table: Table) -> Self {
        Storage {
            table: std::mem::ManuallyDrop::new(table),
        }
    }
}

/// The array entity.
///
/// **Never form a reference to the whole of one.** A collector thread
/// reads the head at `+8` while the owning thread writes the
/// representation at `+48`, so a `&mut LLArray` claims bytes the walker is
/// reading and a `&LLArray` forbids writes the mutator is making. Both are
/// undefined behaviour that only Miri reports. Every operation here takes
/// `*mut LLArray` and derives what it needs field by field —
/// [`as_table_mut`], [`storage_head`], [`category_of`] — and that is the
/// rule a caller outside this crate has to obey too, holding the pointer
/// `ll_array_new` returned rather than a reference made from it.
#[repr(C)]
pub struct LLArray {
    pub rc: RcHeader,
    /// Every word a concurrent walker may read, held here rather than
    /// inside the representation so that no mutating borrow spans it
    /// (`crate::array::head`). Reached shared by mutator and walker
    /// alike, and **private to this module**, which is what keeps
    /// `&mut (*a).head` — the same defect one level down — from being
    /// spelled anywhere else in the crate.
    head: StorageHead,
    pub(crate) storage: Storage,
}

/// The ordered hash and the head beside it, for a reader.
///
/// **The two references are derived field by field from `a`**, never one
/// from the other: a `&Table` covers the table's bytes alone, and the
/// head's are outside it, so a walker reading the head cannot be
/// invalidated by anything a caller does with the returned table. The
/// same derivation with `&mut` is [`as_table_mut`], and there it is the
/// whole point.
///
/// Asking a vector for its table is a bug in the caller rather than a
/// case to handle: the tag decides the call, and a caller that did not
/// read it has already chosen wrongly. That is what the assertion here
/// says.
///
/// # Safety
/// `a` addresses a live array whose storage is the ordered hash.
#[inline]
pub(crate) unsafe fn as_table<'a>(a: *mut LLArray) -> (&'a Table, &'a StorageHead) {
    debug_assert_eq!(unsafe { (*a).head.tag() }, StorageTag::Hash);
    unsafe { (&(*a).storage.table, &(*a).head) }
}

/// The ordered hash for a writer, and the head it may not borrow
/// mutably.
///
/// # Safety
/// As [`as_table`], and the caller holds exclusive use of the
/// representation for the borrow's life.
#[inline]
pub(crate) unsafe fn as_table_mut<'a>(a: *mut LLArray) -> (&'a mut Table, &'a StorageHead) {
    debug_assert_eq!(unsafe { (*a).head.tag() }, StorageTag::Hash);
    // The `&mut` first and the head second, both from `a` and both
    // field-precise: `&mut (*a).storage` claims the union's bytes and
    // nothing beyond them.
    let table = unsafe { &mut (*a).storage.table };
    (table, unsafe { &(*a).head })
}

/// The mixed vector and its head, under [`as_table`]'s rule.
///
/// # Safety
/// `a` addresses a live array whose storage is the mixed vector.
#[inline]
pub(crate) unsafe fn as_vector<'a>(a: *mut LLArray) -> (&'a Vector, &'a StorageHead) {
    debug_assert_eq!(unsafe { (*a).head.tag() }, StorageTag::Vector);
    unsafe { (&(*a).storage.vector, &(*a).head) }
}

/// # Safety
/// As [`as_vector`], and the caller holds exclusive use of the
/// representation for the borrow's life.
#[inline]
pub(crate) unsafe fn as_vector_mut<'a>(a: *mut LLArray) -> (&'a mut Vector, &'a StorageHead) {
    debug_assert_eq!(unsafe { (*a).head.tag() }, StorageTag::Vector);
    let vector = unsafe { &mut (*a).storage.vector };
    (vector, unsafe { &(*a).head })
}

/// Replace a mixed vector with the ordered hash holding the same
/// elements under the keys their positions were: the 2 → 3 transition of
/// `rfc/model/arrays.md`, "Transition Rules".
///
/// **Insertion order and the append cursor carry over by construction.**
/// The elements go in at `0..used` in that order, so the hash's own order
/// is the vector's, and `Table::insert` advances the cursor past each
/// integer key, which leaves it at the vector's length — the key that
/// vector's next append would have taken. An empty vector inserts
/// nothing and the cursor stays at "no integer key yet", which answers 0
/// for the same reason.
///
/// **No reference moves.** Both representations belong to this array, so
/// an element carried across is the same holder's: nothing is retained
/// and nothing is given back, and a refused migration leaves every count
/// where it was.
///
/// **False leaves the array a vector**, whole and serving, with the
/// caller's references untouched. The only refusal is the table's own
/// allocation, and the partly filled table dies with its chunk — the
/// elements it copied are still the vector's, so freeing its storage
/// releases no child.
///
/// The table is filled through a staging head of its own and swapped in
/// inside one window, because a walker reads the four words and the tag
/// as one reading: the chunk, the counts and the tag that says what they
/// mean have to change together or the reading describes a layout the
/// bytes never had (`crate::array::head`).
///
/// # Safety
/// `a` a live array whose storage is the mixed vector, exclusively the
/// caller's for this call; `category` the one its header carries now
/// ([`category_of`]).
pub(crate) unsafe fn migrate_to_hash(a: *mut LLArray, category: MemoryCategory) -> bool {
    debug_assert_eq!(unsafe { (*a).head.tag() }, StorageTag::Vector);
    let mut table = Table::empty();
    // The head the table fills against, so that nothing a walker reads
    // moves until the swap below. It is a local, and no walker can reach
    // it: the array still describes its vector.
    let staging = StorageHead::empty(StorageTag::Hash);

    let n = unsafe { as_vector(a) }.1.used();
    for i in 0..n {
        // Derived per element and dropped again: the swap writes over the
        // vector's bytes, so no reference to them may outlive the loop.
        let (vector, head) = unsafe { as_vector(a) };
        let element = vector.get(head, i).expect("a position below the count");
        if table
            .insert(&staging, category, Key::Int(i as i64), element)
            .is_none()
        {
            table.dispose(&staging, category);
            return false;
        }
    }

    let (old_storage, old_capacity) = {
        let (vector, head) = unsafe { as_vector_mut(a) };
        (head.storage(), *vector.granted_capacity_mut())
    };

    let head = unsafe { &(*a).head };
    head.begin_move();
    unsafe { (&raw mut (*a).storage).write(Storage::of_table(table)) };
    head.set_storage(staging.storage());
    head.set_nslots(staging.nslots());
    head.set_used(staging.used());
    head.set_tag(StorageTag::Hash);
    head.end_move();

    if !old_storage.is_null() {
        unsafe { crate::memory::routing::body_free(category, old_storage, old_capacity) };
    }

    true
}

/// Give the storage back, whichever representation holds it. The
/// elements are the caller's to release first.
///
/// # Safety
/// `a` addresses a live array, and `category` is the one its header
/// carries at this call ([`category_of`]).
#[inline]
pub(crate) unsafe fn dispose_storage(a: *mut LLArray, category: MemoryCategory) {
    match unsafe { (*a).head.tag() } {
        StorageTag::Hash => {
            let (table, head) = unsafe { as_table_mut(a) };
            table.dispose(head, category);
        }
        StorageTag::Vector => {
            let (vector, head) = unsafe { as_vector_mut(a) };
            vector.dispose(head, category);
        }
        StorageTag::Typed => unreachable!("no producer stamps the typed vector"),
    }
}

/// The words a concurrent walker may read, as a raw pointer: the walker
/// arrives with an entity address and takes the head's, reading neither
/// representation and needing no reference to either.
///
/// # Safety
/// `a` addresses a live array.
#[inline]
pub(crate) unsafe fn storage_head(a: *mut LLArray) -> *const StorageHead {
    unsafe { &raw const (*a).head }
}

/// Allocate an empty array in `category`.
///
/// **The storage is the mixed vector**, strategy 2 of
/// `rfc/model/arrays.md`, "Storage Strategies": a fresh array holds a
/// dense range keyed by position and reaches the ordered hash only by
/// migrating, which the element layer asks for when a key arrives that
/// the range cannot hold ([`crate::array::element`]).
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
    unsafe { new_with_storage(category, StorageTag::Vector, Storage::vector()) }
}

/// An empty array whose storage is the mixed vector, for a caller that
/// needs that representation rather than whatever a fresh array is.
///
/// It stamps what [`ll_array_new`] stamps today and is a separate door on
/// purpose: the COW copy takes the **source's** representation, and a copy
/// reading the factory's default would follow the default when it moves.
///
/// # Safety
/// As [`ll_array_new`].
pub(crate) unsafe fn new_vector_array(category: MemoryCategory) -> *mut LLArray {
    unsafe { new_with_storage(category, StorageTag::Vector, Storage::vector()) }
}

/// [`ll_array_new`]'s body, with the representation named.
///
/// **Private, because `tag` and `storage` are one fact spelled twice.**
/// The union carries no discriminant of its own, so the tag is the only
/// thing that says which member the bytes are, and nothing can check the
/// pair — there is nothing to check it against. Keeping the two arguments
/// inside this module means the pairing is made by the two named
/// factories above and cannot be assembled by a caller.
///
/// # Safety
/// As [`ll_array_new`], and `tag` names the member `storage` holds.
unsafe fn new_with_storage(
    category: MemoryCategory,
    tag: StorageTag,
    storage: Storage,
) -> *mut LLArray {
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
        (&raw mut (*a).head).write(StorageHead::empty(tag));
        (&raw mut (*a).storage).write(storage);
        publish_header(
            a as *mut RcHeader,
            RcHeader::new(category, COW | EntityKind::Array.to_flags()),
        );
    }

    a
}

/// The memory category of `a`, read from the header at the moment it is
/// needed and never cached (`dev/DECISIONS.md`, "the `RcHeader` is the
/// only authority on which memory an entity lives in", and "the table is
/// handed its category and reads no header").
///
/// **The array is a parameter and is never derived from the table.** A
/// table sits one `RcHeader` past its array's header, so the address is
/// a subtraction away — but a reference to the body carries provenance
/// over the body alone, and the read underneath is an atomic load, which
/// asks for a permission a shared reference cannot grant at any offset.
/// Only Miri sees the difference; every other build performs the read and
/// reports nothing.
///
/// # Safety
/// `a` is a live array entity.
#[inline]
pub(crate) unsafe fn category_of(a: *const LLArray) -> MemoryCategory {
    unsafe { crate::object::header_category(a as *const RcHeader) }
}

/// Whether a write to this array has to separate first — the rule in
/// `refcount::cow_separation_needed`, which reads the category before the
/// count.
///
/// **A raw pointer rather than `&self`, for [`category_of`]'s reason and
/// one more.** An autoref would span the whole entity, so it would retag
/// the representation's bytes as read-only alongside the header's — and a
/// `&mut Table` derived a line earlier is invalidated by that, with the
/// next insert becoming undefined behaviour. Only Miri sees it, and no
/// call site does both today, which is exactly why the trap is closed by
/// the signature instead of by a rule someone has to remember.
///
/// # Safety
/// `a` is a live array entity.
#[inline]
pub unsafe fn needs_separation(a: *const LLArray) -> bool {
    let flags = unsafe { (*a).rc.flags };
    let refcount = unsafe { (*a).rc.refcount };
    crate::refcount::cow_separation_needed(flags, refcount)
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
/// operation (`dev/DECISIONS.md`, "the array entity, and separation is
/// the shallow copy").
///
/// **The barrier rather than a bare `ll_retain`**, because the teardown
/// gives references back through `drop_ref`, which calls `escape_lose`:
/// a copy that recorded no gain would spend a hold-count belonging to a
/// real holder.
///
/// Insertion order survives, because the copy replays `src` in order,
/// and so does the flood backstop's state: a copy of an attacked table
/// is attacked.
///
/// **Null on refusal**, with nothing published: the copy is private until
/// it is returned, so a failure part-way releases what it has retained,
/// gives the copy itself back through `object::destroy_unpublished`, and
/// leaves the source untouched.
///
/// **Nesting is worked through a list, not the machine stack**, depth here
/// being attacker-shaped input on a store path: a nested arena array is
/// copied empty, published, and its filling pushed onto [`WorkList`],
/// which lives in a buffer-arena chunk (`dev/DECISIONS.md`, "the deep copy
/// walks a list, and teardown is the half still on the stack").
///
/// **The copy preserves the source's sharing**: one copy per distinct
/// source entity, held once for each entry that names it
/// ([`CopiesMade`]). A COW entity's identity is not observable, so the
/// alternative differs only in cost — and it costs 2^depth on a source
/// whose children name each other twice per level, which is
/// attacker-shaped input on a store path.
///
/// **Termination is by the association for a shared child and by the
/// count for a single-named one.** A child reading count one is proved
/// the only name by the arena's upper-bound reading, and one above it is
/// recorded at its first meeting and hit at every later one, so each
/// distinct entity enters the list once. A debug build asserts that
/// composite claim per level; a subgraph closed through the root — the
/// only source that can re-enter and is not held by the association —
/// re-enters once and is
/// reproduced rather than walked until memory refuses.
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
    let mut copies = CopiesMade::new();
    // The root is the only source that can be met again and is not
    // recorded — a count-one child is unrecorded too, and cannot be met
    // twice — and the
    // exemption is the invariant rather than an oversight: meeting it
    // again means a descendant names it, which is a cycle, and a cycle
    // cannot close inside a pure-COW subgraph — every entity a real ring
    // passes through is non-COW and is published by the barrier rather
    // than entered. Recording it would cost a table allocation on every
    // escape copy to answer a shape nothing can build.
    //
    // The set draws from the system allocator, which no fault injection
    // in this crate names, so the probe cannot move which allocation a
    // refusal test measures. Its growth is capped by the arena: one entry
    // per live source, and a source past the root is a 112-byte arena
    // array entity, so the arena refuses the graph long before the set
    // reaches the allocator's abort (`dev/DECISIONS.md`, "the termination
    // probe's storage, and the bound that prices its abort").
    #[cfg(debug_assertions)]
    let mut entered: std::collections::HashSet<*mut LLArray> = std::collections::HashSet::new();
    let mut next = Some((src, dst));
    while let Some((s, d)) = next {
        #[cfg(debug_assertions)]
        {
            assert!(
                entered.insert(s),
                "a source entered the deep copy twice: a child's count \
                 under-reported its holders, or the graph closes on itself \
                 through the root"
            );
        }

        if !unsafe { fill_from(s, d, arena, &mut pending, &mut copies, reason) } {
            // Refused part-way. The root's teardown cascades into every
            // copy this call published, nested ones included: each is
            // held once per entry naming it.
            unsafe { crate::object::destroy_unpublished(dst as *mut RcHeader) };
            pending.dispose();
            copies.dispose();
            return std::ptr::null_mut();
        }

        next = pending.pop();
    }

    pending.dispose();
    copies.dispose();
    dst
}

/// An empty destination in the source's own representation, carrying
/// whatever state the entries cannot reproduce. Null when the allocation
/// is refused.
///
/// **The copy takes the source's representation** rather than a canonical
/// one (`rfc/model/arrays.md`, "External Contract": "Separation copies the
/// storage in its current representation"). A vector copied into a hash
/// would answer every later question the same way and cost twice the
/// bytes for it, and a hash copied into a vector could not hold what it
/// held.
///
/// For the ordered hash the state is the salt, the flood rung and the
/// append cursor. The flood state goes in before the first insert,
/// because it decides how a key is hashed: a copy that starts weak
/// re-installs an attacker's whole collision set under the hash the
/// source escalated away from. A vector carries none of the three — it
/// hashes nothing, and its cursor is its length, which the copy reaches
/// by taking the same elements.
///
/// # Safety
/// `src` is a live array entity.
unsafe fn new_empty_copy(src: *mut LLArray, category: MemoryCategory) -> *mut LLArray {
    match unsafe { (*src).head.tag() } {
        StorageTag::Vector => unsafe { new_vector_array(category) },
        StorageTag::Hash => {
            let dst = unsafe { new_with_storage(category, StorageTag::Hash, Storage::hash()) };
            if dst.is_null() {
                return std::ptr::null_mut();
            }

            let (source, _) = unsafe { as_table(src) };
            let (copy, copy_head) = unsafe { as_table_mut(dst) };
            copy.adopt_flood_state(copy_head, source);
            copy.adopt_append_state(source);
            dst
        }
        StorageTag::Typed => unreachable!("no producer stamps the typed vector"),
    }
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
/// One holder means the source's own entry and nobody else; in the
/// request arena the count is an upper bound, so the copy errs toward
/// sharing (`dev/DECISIONS.md`, "a copy collapses a reference nobody
/// else names, and the arena reads that count as an upper bound").
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
    copies: &mut CopiesMade,
    reason: CopyReason,
) -> bool {
    // Both arrays are in the source's representation, `new_empty_copy`
    // having built the destination from it, so one tag decides the pair.
    debug_assert_eq!(unsafe { (*src).head.tag() }, unsafe { (*dst).head.tag() });
    match unsafe { (*src).head.tag() } {
        StorageTag::Hash => unsafe { fill_table_from(src, dst, arena, pending, copies, reason) },
        StorageTag::Vector => unsafe { fill_vector_from(src, dst, arena, pending, copies, reason) },
        StorageTag::Typed => unreachable!("no producer stamps the typed vector"),
    }
}

/// [`fill_from`] over the ordered hash: every live entry, with its key.
///
/// # Safety
/// As [`fill_from`], and both arrays are the ordered hash.
unsafe fn fill_table_from(
    src: *mut LLArray,
    dst: *mut LLArray,
    arena: *mut crate::memory::arena::Arena,
    pending: &mut WorkList<(*mut LLArray, *mut LLArray)>,
    copies: &mut CopiesMade,
    reason: CopyReason,
) -> bool {
    let category = unsafe { category_of(dst) };
    let (source, source_head) = unsafe { as_table(src) };
    let n = source_head.used();
    for i in 0..n {
        let e = source.entry(source_head, i);
        if e.is_hole() {
            continue;
        }

        let key = if e.is_int_key() {
            Key::Int(e.hash_or_key as i64)
        } else {
            Key::Str(e.string_key())
        };

        let Some(v) = (unsafe {
            element_for_destination(e.value(), category, arena, pending, copies, reason)
        }) else {
            return false;
        };

        let published_key = match unsafe { publish_key(arena, category, key) } {
            Some(published) => published,
            None => {
                unsafe { give_value_back(category, &v) };
                return false;
            }
        };

        let (copy, copy_head) = unsafe { as_table_mut(dst) };
        if copy.insert(copy_head, category, published_key, v).is_none() {
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

/// [`fill_from`] over the mixed vector: every position in order, and no
/// key at all — the position is the key, so the copy reproduces both by
/// appending in the same order.
///
/// There are no holes to skip and nothing to publish for a key, which is
/// the whole of the difference from the hash's pass.
///
/// # Safety
/// As [`fill_from`], and both arrays are the mixed vector.
unsafe fn fill_vector_from(
    src: *mut LLArray,
    dst: *mut LLArray,
    arena: *mut crate::memory::arena::Arena,
    pending: &mut WorkList<(*mut LLArray, *mut LLArray)>,
    copies: &mut CopiesMade,
    reason: CopyReason,
) -> bool {
    let category = unsafe { category_of(dst) };
    let n = unsafe { as_vector(src) }.1.used();
    for i in 0..n {
        let element = {
            let (source, source_head) = unsafe { as_vector(src) };
            source
                .get(source_head, i)
                .expect("a position below the count")
        };

        let Some(v) =
            (unsafe { element_for_destination(element, category, arena, pending, copies, reason) })
        else {
            return false;
        };

        let (copy, copy_head) = unsafe { as_vector_mut(dst) };
        if !copy.push(copy_head, category, v) {
            unsafe { give_value_back(category, &v) };
            return false;
        }
    }

    true
}

/// What the destination must store for one element of the source, with
/// every reference it needs already taken, or `None` on a refusal that
/// took nothing.
///
/// Published **before** the entry is written, so the entry never names
/// something the copy does not hold, and so the barrier can hand back a
/// different entity: an arena COW child crossing into a longer-lived copy
/// is replaced by a copy of its own. A nested arena array is the one
/// child whose copying recurses, so it is copied empty here and its
/// filling pushed onto `pending` — unless `copies` already holds a copy
/// of it, in which case that copy is published for this entry too and
/// nothing is queued. Which of the two happens is decided by the child's
/// count, the association being neither read nor written for a child a
/// single entry names.
///
/// # Safety
/// `element` is a live element of a live array; `category` is the
/// destination's; `arena` the live mounted arena.
unsafe fn element_for_destination(
    element: Value,
    category: MemoryCategory,
    arena: *mut crate::memory::arena::Arena,
    pending: &mut WorkList<(*mut LLArray, *mut LLArray)>,
    copies: &mut CopiesMade,
    reason: CopyReason,
) -> Option<Value> {
    let v = unsafe { element_for_copy(element, reason) };
    if !v.is_refcounted() {
        return Some(v);
    }

    let child = v.entity_ptr();
    // The destination's category first, and it is a parameter rather than
    // a load: with an arena destination no child recurses, and this is the
    // shallow separation's own path — one element at a time, every write
    // to a shared array. Only past it is the child's header worth reading,
    // and then once for both tests: the kind and the count are halves of
    // one word, which under `rc-walk` the two accessors would load twice.
    if category != MemoryCategory::RequestArena
        && let (count, flags) = unsafe { crate::refcount::header_pair(child) }
        && is_nested_arena_array(flags)
    {
        // A child a single entry names cannot be met a second time, and
        // in the arena the count is an upper bound on holders, so count
        // one proves this entry is the only name and the association is
        // neither read nor written for it. That is the reading
        // [`element_for_copy`] already stakes its unwrap on.
        let shared = count > 1;
        if shared && let Some(made) = copies.get(child as *mut LLArray) {
            // An ordinary longer-lived child from here: for a heap entity
            // into a heap slot the barrier is a retain, which is the one
            // reference this entry owes.
            return unsafe {
                crate::memory::barrier::publish_child(
                    arena,
                    category,
                    Value::entity(v.tag(), made as *mut RcHeader),
                )
            };
        }

        let copy = unsafe {
            new_empty_copy(
                child as *mut LLArray,
                crate::refcount::separation_category(category),
            )
        };

        if copy.is_null() {
            return None;
        }

        // Both refusals leave a structure naming a copy that is gone, and
        // neither is ever read: every refusal path out of `separate`
        // unwinds without popping the list or looking the map up.
        if (shared && !copies.record(child as *mut LLArray, copy))
            || !pending.push((child as *mut LLArray, copy))
        {
            // The copy is named by nothing yet, so it goes back here
            // rather than through the root's cascade.
            unsafe { crate::object::destroy_unpublished(copy as *mut RcHeader) };
            return None;
        }

        return Some(Value::entity(v.tag(), copy as *mut RcHeader));
    }

    unsafe { crate::memory::barrier::publish_child(arena, category, v) }
}

/// Whether publishing `child` into a slot of `category` would copy it
/// **and** the copy would contain another copy — an arena COW array
/// crossing into a longer-lived destination. That is the one child whose
/// copying recurses, and the only one the work list exists for; the
/// barrier's copy arm is a leaf for every other kind.
///
/// `flags` are the child's, read by the caller — which needs the count
/// beside them and would otherwise load the same word twice. **The
/// destination's category is tested before the read**, by the caller and
/// not here: it is a parameter, the read is an atomic load, and with an
/// arena destination the answer is no for every child.
#[inline]
fn is_nested_arena_array(flags: u32) -> bool {
    MemoryCategory::from_flags(flags) == MemoryCategory::RequestArena
        && flags & COW != 0
        && (flags & crate::refcount::ENTITY_KIND_MASK) >> crate::refcount::ENTITY_KIND_SHIFT
            == EntityKind::Array as u32
}

/// Which copy each source entity got, so that a child two entries name is
/// copied once and held twice rather than copied twice
/// (`dev/DECISIONS.md`, "the deep copy preserves the source's sharing").
///
/// **The generic [`Table`] used bare**, which is what that table was kept
/// headerless and category-parameterised for: it allocates no entity and
/// calls no barrier, so a source address is an ordinary integer key and a
/// copy an ordinary value it never counts. Its storage is a buffer-arena
/// chunk, the memory the work list beside it uses and for the same
/// reasons.
///
/// The head is a local rather than an entity's field, so no walker can
/// reach it and the rule against a `&mut` spanning a head does not arise:
/// the pair here is one struct's two fields.
struct CopiesMade {
    table: Table,
    head: StorageHead,
}

impl CopiesMade {
    const CATEGORY: MemoryCategory = MemoryCategory::GcHeap;

    /// Empty, and allocating nothing until the first child worth
    /// recording — which is one named by more than a single entry.
    fn new() -> Self {
        CopiesMade {
            table: Table::empty(),
            head: StorageHead::empty(StorageTag::Hash),
        }
    }

    fn get(&self, source: *mut LLArray) -> Option<*mut LLArray> {
        self.table
            .get(&self.head, Key::Int(source as i64))
            .map(|v| v.entity_ptr() as *mut LLArray)
    }

    /// False when the association cannot grow, which the caller unwinds
    /// like any other refusal: the channel keeps meaning out of memory.
    fn record(&mut self, source: *mut LLArray, copy: *mut LLArray) -> bool {
        self.table
            .insert(
                &self.head,
                Self::CATEGORY,
                Key::Int(source as i64),
                Value::entity(Tag::Array, copy as *mut RcHeader),
            )
            .is_some()
    }

    /// Read by nobody: every refusal path unwinds without popping, so a
    /// copy this names may already have been given back.
    ///
    /// The empty case returns before `Table::dispose`, which brackets its
    /// three publications with a release fence and two more stores even
    /// when there is nothing to free. Nothing here is walker-visible —
    /// the head is a local — and a separation with no shared nested child
    /// never allocates, which is every shallow one.
    fn dispose(&mut self) {
        if self.head.storage().is_null() {
            return;
        }

        self.table.dispose(&self.head, Self::CATEGORY);
    }
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
/// reading the array's cells needs the coherent read of the head, which
/// `walk::trace_cells` performs. Kept as a name because the release side
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
    let owner_cat = unsafe { category_of(a) };
    unsafe {
        for_each_counted_child(a, |child| {
            crate::memory::barrier::drop_ref(owner_cat, child);
        })
    };
}

/// Sever this array's counted children — every element, and under the
/// ordered hash every string key beside them — collecting them into
/// `displaced` without releasing them. The array's arm of the drain's
/// Phase 4, and the counterpart of
/// [`crate::object::sever_counted_slots`]: same contract, same reason
/// for not dropping inline, and the caller owes one drop per entry.
///
/// One call per representation, because the state that replaces a
/// severed entry is the representation's: a hole in the ordered hash
/// ([`crate::array::table::Table::sever_entries`]), a null element and
/// an empty count in the mixed vector
/// ([`crate::array::vector::Vector::sever_entries`]).
///
/// # Safety
/// `a` must be a live array entity whose storage is readable and
/// writable.
pub(crate) unsafe fn sever_counted_children(a: *mut LLArray, displaced: &mut Vec<*mut RcHeader>) {
    match unsafe { (*a).head.tag() } {
        StorageTag::Hash => {
            let (table, head) = unsafe { as_table_mut(a) };
            table.sever_entries(head, displaced);
        }
        StorageTag::Vector => {
            let (vector, head) = unsafe { as_vector_mut(a) };
            vector.sever_entries(head, displaced);
        }
        StorageTag::Typed => unreachable!("no producer stamps the typed vector"),
    }
}

/// The address of the array's storage — the block promotion retains when
/// a carry was refused. Null when the array never allocated one.
///
/// **The head answers this and no representation is named**, which is
/// what makes it correct under either tag: the chunk's address is one of
/// the words a walker reads, so it is the entity's rather than the
/// table's.
///
/// # Safety
/// `a` must be a live array entity.
pub(crate) unsafe fn storage_address(a: *mut LLArray) -> *mut u8 {
    unsafe { (*a).head.storage() }
}

/// Bring a surviving array's storage out of the arena that is about to
/// reset, so that it outlives the reset under its promoted owner — one
/// body for both representations, dispatched on the tag
/// (`dev/DECISIONS.md`, "the storage head is a field of the entity, and
/// no `&mut` may span it").
///
/// The entity's header stays where it is — promotion retains the block
/// holding it — while the storage is arena memory that would go back to
/// the pool and be handed to somebody else. **Nothing inside the storage
/// points into it**: a table's chain links are `u32` indices and a
/// vector has no links at all, which is what makes a flat copy legal and
/// is pinned by a test.
///
/// Two routes, chosen by where the storage came from, and the same pair a
/// string's payload takes (`dev/DECISIONS.md`, "promotion carries a
/// survivor's payload, and the two routes are not the same route"): an
/// **OS-direct** storage, over a block payload, is forgotten by the arena
/// and keeps its address, allocating nothing and so refusing nothing; an
/// **in-block** one is copied into a fresh buffer-arena chunk.
///
/// Nothing here records where the storage now lives, the category being
/// the header's to say and promotion rewriting the header a moment later.
/// A refused carry is safe under the new category too: promotion stamps
/// the storage's block `BLOCK_KIND_RETAINED`, the one kind
/// `buffer_free_longlived_payload` leaves alone.
///
/// **False when the copy was refused**, with the storage untouched.
///
/// # Safety
/// `a` must be a live request-arena array of `arena`, mid-reset.
pub(crate) unsafe fn carry_storage_out_of(
    arena: *mut crate::memory::arena::Arena,
    a: *mut LLArray,
) -> bool {
    debug_assert_eq!(
        unsafe { category_of(a) },
        MemoryCategory::RequestArena,
        "only an arena array is carried out of a reset"
    );
    let granted = match unsafe { (*a).head.tag() } {
        StorageTag::Hash => unsafe { as_table_mut(a) }.0.granted_capacity_mut(),
        StorageTag::Vector => unsafe { as_vector_mut(a) }.0.granted_capacity_mut(),
        StorageTag::Typed => unreachable!("no producer stamps the typed vector"),
    };

    let head = unsafe { &(*a).head };
    if head.storage().is_null() {
        return true;
    }

    if *granted > crate::memory::block_pool::BLOCK_PAYLOAD {
        // True whatever the log says, for the reason
        // `string::carry_payload_out_of` gives: a miss means nothing will
        // free the run, so the storage keeps its address and leaks — the
        // safe direction — while reporting a refusal would send the caller
        // into stamping `BLOCK_KIND_RETAINED` over the run's own header.
        let forgotten = unsafe { (*arena).forget_large(head.storage()) };
        debug_assert!(forgotten, "an OS-direct storage the arena never logged");
        return true;
    }

    // The destination is named rather than read from the header, which
    // still says `RequestArena` here: promotion rewrites it after the
    // carry, so that everything the survivor owns moves while the category
    // still describes where it lives (`promote.rs`).
    let (fresh, fresh_granted) = unsafe {
        crate::memory::routing::body_alloc(std::ptr::null_mut(), MemoryCategory::GcHeap, *granted)
    };

    if fresh.is_null() {
        return false;
    }

    unsafe { std::ptr::copy_nonoverlapping(head.storage(), fresh, *granted) };
    head.set_storage(fresh);
    *granted = fresh_granted;
    true
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
/// **Nesting is drained, not recursed**, for the reason the copy above
/// takes the same shape: depth is the caller's input, so a frame set per
/// level is a stack overflow. A nested array whose last reference this
/// teardown drops is pushed onto a list in a buffer-arena chunk and torn
/// down by this call's own loop.
///
/// **Destructors keep Zend's order**, depth first and, inside a level, the
/// order the entries were inserted in: `[[$b], $a]` runs `$b`'s destructor
/// before `$a`'s. That order is a contract on this path, and holding it
/// costs no cursor into the table and one more kind of line on the list
/// ([`Pending`]) — a flat array pushes nothing, and from the first dying
/// nested array the rest of that level is held, released in entry order
/// after the subtree deferred before it (`dev/DECISIONS.md`, "the deep
/// copy walks a list, and teardown is the half still on the stack").
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
        unsafe { dispose_storage(dying, category_of(dying)) };
        if unsafe { category_of(dying) } == MemoryCategory::GcHeap {
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
    /// to settle the escape ledger and the release-at-reset log. One list
    /// mixes the children of several owners, so the category rides on the
    /// line rather than being read once per drain.
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
    let owner_cat = unsafe { category_of(a) };
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
mod tests;
