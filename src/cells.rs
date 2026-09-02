//! Counted cells: where each entity kind keeps them, how a reader reads
//! one, how a writer empties one.
//!
//! Trace an entity's counted children by its kind, without touching `+8`
//! unless the kind carries a class pointer there. Four callers stand on
//! it and none of them is a cycle collector: the class descriptor's
//! outside-cells group (`class`), the arena reset's mark and its
//! copy-on-write reconciliation (`promote`), the dispose path
//! (`object::for_each_counted_cell`), and the array entity.
//!
//! Knowledge split: `memory::heap` knows blocks, slots and occupancy
//! ([`crate::memory::heap::for_each_entity_slot`]); this module knows
//! entity kinds and what each kind's out-edges are. Neither knows the
//! other's internals.
//!
//! This file was the upper half of `walk.rs` until 2026-08-26, when the
//! `rc-walk` collector below its build-step-2 marker was deleted and the
//! substrate above it moved here under a name that is not a collector's.
//! The collector, its census and its Phase 4 drain are readable at
//! `git show archive/pre-rc-cycle:src/walk.rs`; `rc-cycle`'s mark traces
//! through [`trace_cells`] rather than growing a stride of its own
//! (`crate::cycle::mark`).

use crate::object::Object;
use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind, RcHeader};
use crate::value::Value;

/// The kind bits of a live entity's header.
///
/// # Safety
/// `e` must point to a live entity header.
#[inline]
pub(crate) unsafe fn entity_kind(e: *mut RcHeader) -> u32 {
    (unsafe { crate::refcount::mutator_flags(e) } & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT
}

/// One counted cell of an entity: where it is, and the child the word
/// in it designates.
///
/// The address rides along because the sever needs it — it empties the
/// cell it names ([`empty_cell`]) — and a tracer that yielded only the
/// child would send the sever over the layout a second time. The raw
/// word rode along too, for the re-read `rc-walk` made at its Phase 3,
/// and went with that collector.
#[derive(Clone, Copy)]
pub(crate) struct Cell {
    pub addr: usize,
    pub child: *mut RcHeader,
    pub shape: CellShape,
}

/// How wide a cell is, and therefore how a writer empties it: a bare
/// 8-byte pointer takes `NULL`, a 16-byte `Value` takes `Value::null()`.
///
/// This is the one fact about a cell that its address does not carry, and
/// the sever is what needs it — the tracer reads the child and never
/// writes. Without it the sever would have to stride the layout a second
/// time to learn which runs it was in, which is the duplication this
/// walker exists to remove.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CellShape {
    Pointer,
    Box,
}

/// The four behaviours a class owes when its counted cells lie outside
/// its own body — a coroutine's waker block, a map's table chunk. It was
/// six until 2026-08-26: a walk per reader and a re-check went with
/// `rc-walk`, which is what asked for them. One group rather than four
/// nullable fields, because a class carrying some
/// of them and not others fails silently in both directions: a walk
/// without a sever lets the drain empty a table entry cell-wise, and a
/// sever without a walk makes every child of the chunk a computed root
/// (`dev/DECISIONS.md`, "a class with cells outside itself carries one
/// flag and one group of five").
///
/// The group is immortal static data, reached through
/// [`crate::class::Class::outside_cells`], and the flag
/// [`crate::class::CLASS_OUTSIDE_CELLS`] is the predicate: a class
/// without it loads nothing here.
///
/// **The storage these yield cells from is drawn under the instance's own
/// memory category**, through [`crate::memory::routing::body_alloc`], the way a
/// table's storage is. Two obligations meet in that one rule. The storage must
/// be withholdable — a block whose cells a worker trace is reading may not be
/// freed under it (S38.3), and that withholding machinery must take a freeable
/// block kind or a buffer-arena chunk, never an allocation from `std::alloc`.
/// And the category is what decides who frees the storage of an instance that
/// dies without a teardown: an arena object gets the user destructor alone at
/// reset, so
/// storage drawn under any other category is storage nothing ever gives back.
///
/// The category rule covers the zero-count member, whose storage dies with the
/// arena's pages, and [`OutsideCells::carry`] covers the survivor, whom
/// the reset promotes into the heap while its storage is still arena
/// memory (`dev/DECISIONS.md`, "a hooked class draws its storage under its
/// own category, and the arena carry waits").
///
/// **The category rule is the class's to keep, and nothing here can check it.**
/// A zero-count member runs no member of this group — that is what makes the
/// rule worth having — so the one moment the mistake matters is the one moment
/// the crate is not called. A class that draws its storage under `GcHeap` for
/// an arena instance passes every test the crate can write: its survivors are
/// carried correctly and merely leak the old chunk, and its zero-count members
/// leak one chunk each with no symptom short of RSS.
pub(crate) struct OutsideCells {
    /// Yield every cell outside the body, on a quiescent heap.
    ///
    /// **It has no way to say "I gave up"**, and that is the type doing
    /// the work an assertion would otherwise do: a pass that reads
    /// plainly has no writer to lose a race to, and one of them assigns a
    /// survivor's count from the edges it finds
    /// (`promote::reconcile_cow_counts`), so a walk that yielded nothing
    /// would write that count below the truth.
    pub walk_plain: unsafe fn(*mut u8, *const crate::class::Class, &mut dyn FnMut(Cell)),
    /// Empty the outside cells and collect their former occupants,
    /// without dropping them. Not [`empty_cell`], which writes a whole
    /// `Value` and a bare `NULL`: in a table entry the first zeroes the
    /// collision link into a self-referencing chain and the second reads
    /// as an integer key rather than a hole.
    pub sever: unsafe fn(*mut RcHeader, &mut Vec<*mut RcHeader>),
    /// Release the storage itself, as the last act of the ordinary
    /// dispose (`object.rs`, the field teardown). Dispose is the only
    /// caller: a
    /// confirmed cycle member is freed through that same death path
    /// (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 6), so no
    /// collector reaches this hook directly.
    pub free: unsafe fn(*mut RcHeader),
    /// Bring the storage out of a dying request arena, for an instance
    /// the reset promotes. The zero-count member needs nothing: its storage was
    /// drawn under its own category and dies with the pages.
    ///
    /// Three things the implementer owes, none of them derivable from the
    /// signature. The destination category is **named**, `GcHeap`, and not
    /// read from the header, which still says `RequestArena` here —
    /// promotion rewrites it after this call, so that everything a
    /// survivor owns moves while the category still describes where it
    /// lives. The old storage is **not** freed: arena memory has no free,
    /// and the pages go back whole at the end of the reset. And `arena` is
    /// the dying arena, for the one operation that needs it — an
    /// allocation the arena made directly from the system is transferred
    /// by forgetting the arena's record of it (`Arena::forget_large`),
    /// never copied and never pinned.
    ///
    /// The argument for the shape is `dev/DECISIONS.md`, "the arena carry
    /// is the group's sixth member, and a refusal answers the bytes it
    /// left behind" — that refusal is [`OutsideCarry::Pinned`].
    pub carry: unsafe fn(*mut crate::memory::arena::Arena, *mut RcHeader) -> OutsideCarry,
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "only a class hook constructs one, and no class does yet"
    )
)]
/// What a class's [`OutsideCells::carry`] did about a survivor's storage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OutsideCarry {
    /// The storage is out of the arena, and the instance points at it.
    Carried,
    /// The bytes are a pinned payload: they stay where they are, at
    /// `memory`, and the reset keeps the block holding them out of
    /// circulation.
    ///
    /// **The bytes, not their block.** The reset masks the address into a
    /// block header itself, as it does for the two kinds that answer
    /// through their own carry — a stamp written through an unmasked
    /// pointer lands in the storage's own first word and leaves the block
    /// unretained.
    ///
    /// **Only memory inside a block of this arena may be pinned.** An
    /// allocation the arena made directly from the system has no block of
    /// the arena's, and the reset frees every one it logged, so such
    /// storage is transferred rather than pinned — the arena forgets the
    /// record and the address does not move
    /// (`array::entity::carry_storage_out_of` is the worked example).
    Pinned { memory: *mut u8 },
    /// The instance has no storage to carry.
    Nothing,
}

/// What a class's outside-cell carry answers, and the three cases are
/// not two: a promotion that moved the storage and one that had none to
/// move leave the arena reset different work.
///
/// Only a class's own hook constructs one, and no class does yet: the
/// first is the map of `rfc/model/maps.md`, and the tests here build one
/// of their own.
/// How a walk reads the entity memory it strides over.
///
/// This is the **only** difference between the walks that would
/// otherwise be one per layout. Tracing on a quiescent heap reads
/// plainly; a collector thread races the mutator and must read
/// relaxed-atomically, because a plain read against a concurrent store
/// is undefined behaviour rather than a torn value — and the design
/// rests on a torn read costing at most a phantom edge or a missed one.
/// Parameterizing the read instead of copying the stride is what lets
/// one enumerator serve both. Only the plain reader exists today; S38.0
/// adds the collector's (`PLAN.md`).
///
/// It covers reads of the **entity's own** memory only. A class descriptor and
/// a template shape are immortal static data no mutator writes, so both
/// instantiations read those plainly, and the word that *points* at them is
/// what goes through the reader. **Two methods, and the second is not a
/// convenience.** A cell holding a pointer must be read *as* a pointer:
/// recovering one from an integer load strips its provenance, and Miri rejects
/// the first dereference of the result unless the target's address happens to
/// have been exposed as an integer somewhere. The collector could afford the
/// integer form — everything it chases is entity or immortal memory whose
/// address was — but the quiescent walk chases a template shape that may be an
/// ordinary Rust static, and that one Miri refuses. Found by Miri, not by
/// reasoning: the first version of this trait had only `word`, and
/// `template::tests::the_instance_as_an_ordinary_entity::a_dying_template_releases_what_it_held`
/// reported a dangling pointer with no provenance.
pub(crate) trait CellReader {
    /// Walk a class's cells outside its own body with this reader's
    /// member of the group. The trait is the one place the two readers
    /// differ, so the choice belongs here.
    ///
    /// # Safety
    /// As the group's own members: `base` addresses a live region laid
    /// out by `cls`, whose class carries the group.
    unsafe fn walk_outside(
        group: &OutsideCells,
        base: *mut u8,
        cls: *const crate::class::Class,
        visit: &mut dyn FnMut(Cell),
    );

    /// Read the eight bytes at `addr` as an integer. For the second word
    /// of a `Value`, which carries the tag and flags rather than an
    /// address.
    ///
    /// # Safety
    /// `addr` must be an aligned, readable eight-byte word of a live
    /// entity.
    unsafe fn word(at: *const u8) -> u64;

    /// Read the pointer at `at`, **keeping its provenance**. For a class
    /// word, a template's shape word, a pointer slot, and a Box's payload
    /// word.
    ///
    /// # Safety
    /// As [`CellReader::word`], and the cell must hold a pointer or null.
    unsafe fn ptr(at: *const u8) -> *mut u8;
}

/// The ordinary reader: a quiescent heap, or memory only this thread can
/// reach.
pub(crate) struct PlainCells;

impl CellReader for PlainCells {
    #[inline]
    unsafe fn walk_outside(
        group: &OutsideCells,
        base: *mut u8,
        cls: *const crate::class::Class,
        visit: &mut dyn FnMut(Cell),
    ) {
        unsafe { (group.walk_plain)(base, cls, visit) }
    }

    #[inline]
    unsafe fn word(at: *const u8) -> u64 {
        unsafe { (at as *const u64).read() }
    }

    #[inline]
    unsafe fn ptr(at: *const u8) -> *mut u8 {
        unsafe { (at as *const *mut u8).read() }
    }
}

/// The counted child of the sixteen-byte `Value` at `at`, or `None` when
/// the cell holds nothing counted.
///
/// The payload word is read as an **integer** rather than as a pointer,
/// which is `Value`'s doing rather than a shortcut: `Value::entity` stores
/// the address as a `u64`, so the bytes carry no provenance and reading
/// them back as a pointer yields one Miri rejects on first use.
/// `entity_ptr` recovers it by the same cast.
///
/// The payload is read before the flags, and both readers may see a store
/// land between the two, so a `Value` can be read torn across its words.
/// A torn read costs a phantom edge or a missed one, never a wrong free
/// (`rfc/model/gc/rc-cycle.md`, "Speculative tracing and exact validation").
///
/// # Safety
/// `at` addresses a readable, aligned `Value` of a live entity, which a
/// concurrent reader `R` may find the mutator writing.
#[inline]
pub(crate) unsafe fn counted_box_cell<R: CellReader>(at: *const u8) -> Option<Cell> {
    let child = unsafe { R::word(at) } as *mut RcHeader;
    if !Value::refcounted_in_meta_word(unsafe { R::word(at.add(8)) }) {
        return None;
    }

    Some(Cell {
        addr: at as usize,
        child,
        shape: CellShape::Box,
    })
}

/// Visit every counted child of `entity`, dispatching on the kind bits
/// **before** touching `+8`: only Object (0) and Lazy (1) carry a class
/// pointer there (`rfc/model/classes.md`, "the class pointer lives in
/// the body"), and reaching for the trace map through a class that does
/// not exist is a wild read.
///
/// A reference box (kind 3) is traced through its one Value. An array
/// (kind 2) is traced through the counted children of its table —
/// elements and string keys alike, since a table holds a reference to
/// each string it keys on. Box is skipped, which is conservative: an
/// omitted source only removes in-edges, so its targets are pinned as
/// roots. String, WeakRef and Box stay skipped by design. A
/// string is a leaf whichever layout it has: its payload is bytes, never
/// entities, so no out-edge of one can close a ring. (Box: untraceable C
/// payload; a weak cell's target is deliberately uncounted, `src/weak.rs`.)
///
/// # Safety
/// `entity` must point to a live entity whose slots are still readable.
pub unsafe fn trace_entity(entity: *mut RcHeader, mut visit: impl FnMut(*mut RcHeader)) {
    let kind = unsafe { entity_kind(entity) };
    unsafe { trace_cells::<PlainCells>(entity, kind, |cell| visit(cell.child)) };
}

/// Every counted cell of `entity`, dispatched on its kind: the one
/// tracing stride, shared by the quiescent walk and a collector's trace,
/// which differ only in `R`. Which kinds have counted cells is the
/// dispatch at [`trace_entity`].
///
/// `kind` is passed rather than loaded, because the collector holds it
/// from its own snapshot and must not re-read a header the mutator is
/// writing.
///
/// Yields nothing for a kind that keeps its cells in its own slot, and
/// for an array whose head would not read coherently — an array given up
/// this way is the safe direction of the `RC − IN` root identity, since
/// its children read as externally referenced.
///
/// # Safety
/// `entity` is a live entity of `kind` whose cells are readable. Under a
/// concurrent reader `R` it must be **mature**: the class word at `+8` is
/// chased, and that is safe only for an entity published long enough ago
/// for the read to be ordered.
pub(crate) unsafe fn trace_cells<R: CellReader>(
    entity: *mut RcHeader,
    kind: u32,
    mut visit: impl FnMut(Cell),
) {
    const OBJECT: u32 = EntityKind::Object as u32;
    const LAZY: u32 = EntityKind::Lazy as u32;
    const REFERENCE: u32 = EntityKind::Reference as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    const KEY_OFFSET: usize = std::mem::offset_of!(crate::array::entry::Entry, key_word);
    const VALUE_OFFSET: usize = crate::array::entry::ELEMENT_OFFSET;
    match kind {
        OBJECT | LAZY => {
            // The class word is the entity's own and goes through the
            // reader; the descriptor it names is immortal and does not.
            let class =
                unsafe { R::ptr((entity as *const u8).add(8)) } as *const crate::class::Class;
            unsafe { crate::object::for_each_counted_cell::<R>(entity as *mut u8, class, visit) }
        }
        REFERENCE => {
            let at = unsafe { (entity as *const u8).add(8) };
            if let Some(cell) = unsafe { counted_box_cell::<R>(at) } {
                visit(cell);
            }
        }

        // The mutator moves an array's cells, so the head is read
        // coherently first and the array given up rather than strided over
        // a stale chunk (`StorageHead::coherent`). Giving it up costs one
        // collection and frees nothing early: the children whose in-edges
        // go unseen read as externally referenced, which is the safe
        // direction of the `RC − IN` root identity
        // (`rfc/model/gc/rc-cycle.md`).
        ARRAY => {
            let head = unsafe {
                crate::array::entity::storage_head(entity as *mut crate::array::entity::LLArray)
            };
            let Some(view) = (unsafe { crate::array::head::StorageHead::coherent(head) }) else {
                return;
            };

            // The stride is chosen here and nowhere earlier: the tag came
            // out of the same validated reading as the chunk, so a stale
            // one was discarded with it rather than selecting a layout.
            // Matched rather than tested against one value, because a tag
            // with no stride here must give the array up the way an
            // incoherent reading does. Falling through to the hash stride
            // would let a *valid* tag select the wrong layout, which is
            // what the read protocol exists to prevent. `Typed` is that
            // tag today.
            match view.tag {
                crate::array::head::StorageTag::Hash => {}
                crate::array::head::StorageTag::Typed => {
                    debug_assert!(false, "the walker has no stride for the typed vector");
                    return;
                }
                crate::array::head::StorageTag::Vector => {
                    let (elements, used) =
                        unsafe { crate::array::vector::Vector::elements_of(&view) };
                    for i in 0..used {
                        // No key beside the element: a vector's key is the
                        // position, so every cell here is a Box.
                        let value_at = unsafe {
                            elements.add(i * crate::array::vector::ELEMENT_STRIDE) as *const u8
                        };
                        if let Some(cell) = unsafe { counted_box_cell::<R>(value_at) } {
                            visit(cell);
                        }
                    }

                    return;
                }
            }

            let (entries, used) = unsafe { crate::array::table::Table::entries_of(&view) };

            for i in 0..used {
                let at = unsafe { entries.add(i) as *const u8 };
                // A string key is a counted child behind a tagged word;
                // the sentinels below the limit are an integer key and a
                // hole, and neither is a cell. The child is the masked
                // pointer, because the collector looks an edge up by the
                // entity's true address (`array/entry.rs`, the key word's
                // encoding).
                let key = unsafe { R::ptr(at.add(KEY_OFFSET)) };
                if key as usize >= crate::array::entry::KEY_SENTINEL_LIMIT {
                    visit(Cell {
                        addr: at as usize + KEY_OFFSET,
                        child: (key as usize & !crate::array::entry::KEY_TAG_MASK) as *mut RcHeader,
                        shape: CellShape::Pointer,
                    });
                }

                let value_at = unsafe { at.add(VALUE_OFFSET) };
                if let Some(cell) = unsafe { counted_box_cell::<R>(value_at) } {
                    visit(cell);
                }
            }
        }
        _ => {}
    }
}

/// Empty one cell through the store barrier, by its shape.
///
/// The barrier is not ceremony here: this store runs on the mutator while
/// a collection may be in flight, and the collector reads the same cell
/// as a relaxed atomic. A plain write against that load is a
/// mixed-atomicity data race, which is undefined behaviour rather than
/// the torn value a trace is built to tolerate.
///
/// # Safety
/// `cell` addresses a live, writable cell of the shape it names.
#[inline]
pub(crate) unsafe fn empty_cell(cell: Cell) {
    match cell.shape {
        CellShape::Pointer => unsafe {
            crate::memory::barrier::write_ptr_slot(
                cell.addr as *mut *mut RcHeader,
                std::ptr::null_mut(),
            )
        },
        CellShape::Box => unsafe {
            crate::memory::barrier::write_value_slot(cell.addr as *mut Value, Value::null())
        },
    }
}

/// Sever every counted cell of `entity`: empty the cell and collect the
/// child it held into `displaced`, **without dropping it** — the caller
/// owes one drop per entry.
///
/// **The single sever dispatch**, beside [`trace_cells`], and it goes
/// through that walker rather than striding again: one layout, one
/// stride, two operations over it.
///
/// The kinds are named rather than left to a default. A kind that falls
/// off this dispatch is not a leak the next pass finds: the component was
/// already confirmed garbage, so its members are guarded, severed of
/// nothing, and un-guarded back to the counts they started with —
/// collected zero, forever, on every call. That is how the Array kind sat
/// here from the day it became producible, and an empty fall-through is
/// what hid it.
///
/// # Safety
/// `entity` is a live entity of `kind` whose cells are readable and
/// writable, and no other thread writes them.
#[expect(
    dead_code,
    reason = "the commit stage that severs a condemned component is S36.5"
)]
pub(crate) unsafe fn sever_cells(
    entity: *mut RcHeader,
    kind: u32,
    displaced: &mut Vec<*mut RcHeader>,
) {
    const OBJECT: u32 = EntityKind::Object as u32;
    const LAZY: u32 = EntityKind::Lazy as u32;
    const REFERENCE: u32 = EntityKind::Reference as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    const STRING: u32 = EntityKind::String as u32;
    const STRING_DYNAMIC: u32 = EntityKind::StringDynamic as u32;
    const WEAKREF: u32 = EntityKind::WeakRef as u32;
    const BOX: u32 = EntityKind::Box as u32;
    match kind {
        // A reference box has no class word at `+8` — its `Value` is
        // there — so it takes the tracing walker, which dispatches on the
        // kind, and it can own nothing outside itself.
        REFERENCE => unsafe {
            trace_cells::<PlainCells>(entity, kind, |cell| {
                empty_cell(cell);
                displaced.push(cell.child);
            });
        },
        OBJECT | LAZY => unsafe {
            // The body's own cells, and only those: `empty_cell` writes a
            // whole `Value` or a bare `NULL`, which is right for a cell
            // inside the entity and wrong for anything a class keeps
            let cls = (*(entity as *mut Object)).class;
            crate::object::for_each_body_cell::<PlainCells>(entity as *mut u8, cls, &mut |cell| {
                empty_cell(cell);
                displaced.push(cell.child);
            });

            // A class whose cells lie outside its body empties them
            // itself: a table entry cleared cell-wise loses its collision
            // link to a whole-Box store and reads its key word's null as
            // an integer key rather than a hole (`dev/DECISIONS.md`, "a
            // class with cells outside itself carries one flag and one
            // group of five").
            if let Some(group) = crate::class::Class::outside_cells(cls) {
                (group.sever)(entity, displaced);
            }
        },
        // Severing an array is the table's, not `empty_cell`'s: a
        // cleared entry is a hole rather than a null, and an
        // integer-keyed entry has no key cell to empty at all.
        ARRAY => unsafe {
            crate::array::entity::sever_counted_children(
                entity as *mut crate::array::entity::LLArray,
                displaced,
            )
        },
        // The kinds with no counted children. Reaching one here is
        // ordinary, not an error: a component is weakly connected, so a
        // string element of a dying object and a weak cell that died
        // inside the garbage are both members. Severing them is genuinely
        // nothing — a string is a leaf, a weak cell's target is
        // deliberately uncounted and the drain's weak pass has already
        // nulled it, and an FFI Box holds an opaque C payload the runtime
        // never counted.
        STRING | STRING_DYNAMIC | WEAKREF | BOX => {}
        _ => debug_assert!(false, "entity kind codes 4-7 and 12-15 are unassigned"),
    }
}

#[cfg(test)]
use crate::memory::heap::for_each_entity_slot;

#[cfg(test)]
/// A point-in-time census of the walked entity population.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Occupied entity-block slots.
    pub entities: usize,
    /// Entities per kind code (index = kind bits; codes 4-7 and 12-15
    /// are unassigned and stay zero). Sixteen entries because the field
    /// is four bits wide: a census over a code the array cannot hold
    /// would panic rather than report an unknown kind.
    pub by_kind: [usize; 16],
    /// Counted out-edges of walked entities, targets anywhere.
    pub edges: usize,
}

#[cfg(test)]
/// Count every live entity in the entity-block population, by kind, with
/// its counted out-edges — the whole-heap leak-detector precursor of
/// build step 2.
///
/// # Safety
/// As [`for_each_entity_slot`]: a quiescent mutator.
pub unsafe fn heap_census() -> Census {
    let mut census = Census::default();
    unsafe {
        for_each_entity_slot(|entity| {
            census.entities += 1;
            census.by_kind[entity_kind(entity) as usize] += 1;
            trace_entity(entity, |_child| census.edges += 1);
        });
    }

    census
}

#[cfg(test)]
mod tests;
