//! Entity walking: the kind-dispatched tracer and the heap census
//! (`rc-walk` build step 1), the synchronous whole-heap collection
//! ([`collect_cycles`], step 2) and the Phase 4 drain the collector posts
//! to (`drain_confirmed`, step 3) — `rfc/model/gc/rc-walk.md`, "Build
//! order". One walking substrate serves all three: enumerate every live
//! entity through the region registry, and trace an entity's counted
//! children by its kind without touching `+8` unless the kind carries a
//! class pointer there.
//!
//! Knowledge split: `memory::heap` knows blocks, slots and occupancy
//! ([`for_each_entity_slot`]); this module knows entity kinds and what
//! each kind's out-edges are. Neither knows the other's internals.

use crate::memory::heap::for_each_entity_slot;
use crate::object::Object;
use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind, RcHeader};
use crate::value::Value;

/// The kind bits of a live entity's header.
///
/// # Safety
/// `e` must point to a live entity header.
#[inline]
unsafe fn entity_kind(e: *mut RcHeader) -> u32 {
    (unsafe { (*e).flags } & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT
}

/// One counted cell of an entity: where it is, the word that is in it,
/// and the child that word designates.
///
/// The address and the raw word are not decoration for the tracer's
/// benefit — the epoch records both in its `Edge` and re-reads the cell
/// at Phase 3 to see whether the mutator has moved it. A walker that
/// yielded only the child could not serve the collector, which is how
/// the collector came to carry its own copy of every stride.
/// The raw word is the epoch's alone, hence its `rc-trace` dead-code
/// exemption below; the address is the sever's too, which empties the
/// cell it names ([`empty_cell`]).
#[derive(Clone, Copy)]
pub(crate) struct Cell {
    pub addr: usize,
    #[cfg_attr(not(feature = "rc-walk"), expect(dead_code))]
    pub raw: u64,
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

/// How a walk reads the entity memory it strides over.
///
/// This is the **only** difference between the three walks that used to
/// exist per layout. Tracing on a quiescent heap reads plainly; the
/// concurrent collector races the mutator and must read relaxed-atomically,
/// because a plain read against a concurrent store is undefined behaviour
/// rather than a torn value — and the whole design rests on a torn read
/// being merely a phantom or a missed edge (`rfc/model/gc/rc-walk.md`).
/// Parameterizing the read instead of copying the stride is what lets one
/// enumerator serve both.
///
/// It covers reads of the **entity's own** memory only. A class
/// descriptor and a template shape are immortal static data no mutator
/// writes, so both instantiations read those plainly, and the word that
/// *points* at them is what goes through the reader.
/// **Two methods, and the second is not a convenience.** A cell holding a
/// pointer must be read *as* a pointer: recovering one from an integer
/// load strips its provenance, and Miri rejects the first dereference of
/// the result unless the target's address happens to have been exposed as
/// an integer somewhere. The collector could afford the integer form —
/// everything it chases is entity or immortal memory whose address was —
/// but the quiescent walk chases a template shape that may be an ordinary
/// Rust static, and that one Miri refuses. Found by Miri, not by reasoning:
/// the first version of this trait had only `word`, and
/// `template::tests::the_instance_as_an_ordinary_entity::a_dying_template_releases_what_it_held` reported a
/// dangling pointer with no provenance.
pub(crate) trait CellReader {
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
    unsafe fn word(at: *const u8) -> u64 {
        unsafe { (at as *const u64).read() }
    }

    #[inline]
    unsafe fn ptr(at: *const u8) -> *mut u8 {
        unsafe { (at as *const *mut u8).read() }
    }
}

/// The collector's reader: the mutator is running and may store into any
/// of these cells. Exists only where a concurrent collector does.
#[cfg(feature = "rc-walk")]
pub(crate) struct RelaxedCells;

#[cfg(feature = "rc-walk")]
impl CellReader for RelaxedCells {
    #[inline]
    unsafe fn word(at: *const u8) -> u64 {
        unsafe {
            (*(at as *const std::sync::atomic::AtomicU64))
                .load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// `AtomicPtr`, not an `AtomicU64` cast back: the atomic load keeps
    /// the provenance an integer load would drop, so the collector gains
    /// what the quiescent walk needed rather than merely tolerating what
    /// it had.
    #[inline]
    unsafe fn ptr(at: *const u8) -> *mut u8 {
        unsafe {
            (*(at as *const std::sync::atomic::AtomicPtr<u8>))
                .load(std::sync::atomic::Ordering::Relaxed)
        }
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
/// land between the two: a `Value` torn across its words is what the
/// epoch's re-check exists to catch (`collector::Edge`).
///
/// # Safety
/// `at` addresses a readable, aligned `Value` of a live entity, which
/// under `R = RelaxedCells` the mutator may be writing.
#[inline]
pub(crate) unsafe fn counted_box_cell<R: CellReader>(at: *const u8) -> Option<Cell> {
    let child = unsafe { R::word(at) } as *mut RcHeader;
    if !Value::refcounted_in_meta_word(unsafe { R::word(at.add(8)) }) {
        return None;
    }

    Some(Cell {
        addr: at as usize,
        raw: child as u64,
        child,
        shape: CellShape::Box,
    })
}

/// Visit every counted child of `entity`, dispatching on the kind bits
/// **before** touching `+8`: only Object (0) and Lazy (6) carry a class
/// pointer there, and reaching for the trace map through a class that
/// does not exist is a wild read (`rfc/model/gc/rc-walk.md`, "What the
/// walker traces").
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
/// tracing stride, shared by the quiescent walk and the collector's epoch,
/// which differ only in `R`. Which kinds have counted cells is the
/// dispatch at [`trace_entity`].
///
/// `kind` is passed rather than loaded, because the collector holds it
/// from its own snapshot and must not re-read a header the mutator is
/// writing.
///
/// Answers the version of the storage the cells came out of, and `None`
/// for a kind that keeps its cells in its own slot or an array whose head
/// would not read coherently. Neither can leave a cell behind in a chunk
/// it has left, so neither gives the re-check anything to ask a version
/// about (`collector::Edge`).
///
/// # Safety
/// `entity` is a live entity of `kind` whose cells are readable. Under
/// `R = RelaxedCells` it must be **mature**: the class word at `+8` is
/// chased, which is safe only because a handshake ordered its publication
/// epochs ago.
pub(crate) unsafe fn trace_cells<R: CellReader>(
    entity: *mut RcHeader,
    kind: u32,
    mut visit: impl FnMut(Cell),
) -> Option<usize> {
    const OBJECT: u32 = EntityKind::Object as u32;
    const LAZY: u32 = EntityKind::Lazy as u32;
    const REFERENCE: u32 = EntityKind::Reference as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    const KEY_OFFSET: usize = std::mem::offset_of!(crate::array::entry::Entry, key);
    const VALUE_OFFSET: usize = crate::array::entry::ELEMENT_OFFSET;
    match kind {
        OBJECT | LAZY => {
            // The class word is the entity's own and goes through the
            // reader; the descriptor it names is immortal and does not.
            let class =
                unsafe { R::ptr((entity as *const u8).add(8)) } as *const crate::class::Class;
            unsafe { crate::object::for_each_counted_cell::<R>(entity as *mut u8, class, visit) };
            None
        }
        REFERENCE => {
            let at = unsafe { (entity as *const u8).add(8) };
            if let Some(cell) = unsafe { counted_box_cell::<R>(at) } {
                visit(cell);
            }

            None
        }

        // The mutator moves an array's cells, so the head is read
        // coherently first and the array given up rather than strided over
        // a stale chunk (`StorageHead::coherent`). Giving it up leaks one
        // epoch and frees nothing early: `rfc/model/gc/rc-walk.md`, "The
        // central identity: roots are derived, not enumerated".
        ARRAY => {
            let head = unsafe {
                crate::array::entity::storage_head(entity as *mut crate::array::entity::LLArray)
            };
            let Some(view) = (unsafe { crate::array::head::StorageHead::coherent(head) }) else {
                return None;
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
                    return None;
                }
                crate::array::head::StorageTag::Vector => {
                    let (elements, used) =
                        unsafe { crate::array::vector::Vector::elements_of(&view) };
                    for i in 0..used {
                        // No key beside the element: a vector's key is the
                        // position, so every cell here is a Box.
                        let value_at = unsafe { elements.add(i * 16) as *const u8 };
                        if let Some(cell) = unsafe { counted_box_cell::<R>(value_at) } {
                            visit(cell);
                        }
                    }

                    return Some(view.version);
                }
            }

            let (entries, used) = unsafe { crate::array::table::Table::entries_of(&view) };

            for i in 0..used {
                let at = unsafe { entries.add(i) as *const u8 };
                // A string key is a counted child; the two sentinels below
                // it are an integer key and a hole, and neither is a cell.
                let key = unsafe { R::ptr(at.add(KEY_OFFSET)) };
                if key as usize > crate::array::entry::KEY_HOLE {
                    visit(Cell {
                        addr: at as usize + KEY_OFFSET,
                        raw: key as u64,
                        child: key as *mut RcHeader,
                        shape: CellShape::Pointer,
                    });
                }

                let value_at = unsafe { at.add(VALUE_OFFSET) };
                if let Some(cell) = unsafe { counted_box_cell::<R>(value_at) } {
                    visit(cell);
                }
            }

            Some(view.version)
        }
        _ => None,
    }
}

/// Empty one cell through the store barrier, by its shape.
///
/// The barrier is not ceremony here: this store runs on the mutator while
/// an epoch may be live, and the collector reads the same cell as a
/// relaxed atomic (`collector::walk_edges`). A plain write against that
/// load is a mixed-atomicity data race, which is undefined behaviour
/// rather than the torn value Phases 3 and 4 are built to repair.
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
    const WEAKREF: u32 = EntityKind::WeakRef as u32;
    const BOX: u32 = EntityKind::Box as u32;
    match kind {
        OBJECT | LAZY | REFERENCE => unsafe {
            // The storage version the walk answers with is the epoch's
            // instrument and nothing to a sever: these three kinds keep
            // their cells in their own slot, which no move replaces.
            trace_cells::<PlainCells>(entity, kind, |cell| {
                empty_cell(cell);
                displaced.push(cell.child);
            });
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
        STRING | WEAKREF | BOX => {}
        _ => debug_assert!(false, "entity kind 7 is reserved"),
    }
}

/// A point-in-time census of the walked entity population.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Occupied entity-block slots.
    pub entities: usize,
    /// Entities per kind code (index = kind bits; 7 is reserved).
    pub by_kind: [usize; 8],
    /// Counted out-edges of walked entities, targets anywhere.
    pub edges: usize,
}

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

// --- Synchronous cycle collection (rc-walk build step 2) -------------------

use crate::refcount::{MemoryCategory, is_object, ll_release};
use std::collections::{HashMap, HashSet};

thread_local! {
    /// Reentrancy guard, mirroring `gc::GC_ACTIVE`: a destructor that
    /// somehow reaches `collect_cycles` again becomes a no-op instead of
    /// re-walking a heap whose guards are outstanding (the drain is not
    /// re-entrant by design — finding F8, `rfc/model/gc/rc-walk-proof.md`).
    /// Second duty since 2026-07-28: the epoch pickup gate reads it
    /// ([`walk_active`]) — while set, checkpoints on this thread refuse
    /// verdict messages, so it must stay set until every guard this
    /// walk placed is gone (the `Drop` clear below covers that).
    static WALK_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a synchronous collection is running on this thread. The
/// epoch pickup gate refuses messages while it is set: the collection
/// is drain-class — it holds guards on members an epoch message may
/// name (`rfc/model/gc/rc-walk.md`, "When the collector runs", step 4).
#[cfg(feature = "rc-walk")]
pub(crate) fn walk_active() -> bool {
    WALK_ACTIVE.with(|a| a.get())
}

/// Statistics of one synchronous collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CollectStats {
    /// GcHeap entities whose refcount and out-edges entered the snapshot.
    pub walked: usize,
    /// Weakly-connected garbage components the mark phase produced.
    pub candidate_components: usize,
    /// Components dropped by the exact test or the post-destructor
    /// re-verify (a resurrection).
    pub acquitted: usize,
    /// Entities freed.
    pub collected: usize,
}

/// The whole-heap synchronous cycle collection — rc-walk build step 2
/// (`rfc/model/gc/rc-walk.md`, "Build order"). No collector thread and
/// no Phase 3 filter: with a quiescent mutator every read is already
/// exact, so condemnation and the handshake have nothing to repair, and
/// no candidate buffer is involved at all.
///
/// The drain is the discipline `gc::run_cyclic_destructors` proves, minus
/// its restore step — these counts are already real: exact test, guard,
/// pending destructors once each, re-verify discounting the guard
/// (`rc − 1 = indeg`, finding F1), then sever and un-guard through the
/// ordinary teardown path.
///
/// The reverse of the epoch pickup gate — refusing to *start* mid-drain
/// or mid-teardown — is deliberately not built: today's callers are
/// tests and the explicit ABI, and a mid-drain call is conservative
/// anyway (the drain's guards inflate rc, so guarded members classify
/// live). The entry gate belongs to the pressure ladder
/// (`rfc/model/gc/rc-walk.md`, "When the collector runs", unbuilt).
///
/// # Safety
/// As [`for_each_entity_slot`], and it must fire at a clean point — where
/// refcounts and physical edges agree, never mid-store or mid-teardown
/// (the arm/fire rule of `rfc/model/gc/strategies.md`).
pub unsafe fn collect_cycles() -> CollectStats {
    if WALK_ACTIVE.with(|a| a.get()) {
        return CollectStats::default();
    }

    struct Active;
    impl Drop for Active {
        fn drop(&mut self) {
            WALK_ACTIVE.with(|a| a.set(false));
        }
    }

    WALK_ACTIVE.with(|a| a.set(true));
    let _active = Active;
    unsafe { collect_cycles_inner() }
}

unsafe fn collect_cycles_inner() -> CollectStats {
    let mut stats = CollectStats::default();

    // Phase 1 — WALK: snapshot the walked population. Only GcHeap
    // entities get a row; every other category is a root source by the
    // corollary of the central identity (its edges appear in RC, never
    // in IN). The acyclic-class skip is not taken yet: the flag is
    // compiler-owed and no compiler exists — recall, not correctness.
    let mut entities: Vec<*mut RcHeader> = Vec::new();
    unsafe {
        for_each_entity_slot(|e| {
            if (*e).memory_category() == MemoryCategory::GcHeap {
                entities.push(e);
            }
        });
    }

    let n = entities.len();
    stats.walked = n;
    let ids: HashMap<usize, u32> = entities
        .iter()
        .enumerate()
        .map(|(i, &e)| (e as usize, i as u32))
        .collect();

    // rc[] and edges[]: a child that maps to no walked row contributes to
    // its target's RC and never to IN — dropped, conservative.
    let mut rc = vec![0u32; n];
    let mut edges: Vec<(u32, u32)> = Vec::new();
    for (i, &e) in entities.iter().enumerate() {
        rc[i] = unsafe { (*e).refcount };
        unsafe {
            trace_entity(e, |child| {
                if let Some(&j) = ids.get(&(child as usize)) {
                    edges.push((i as u32, j));
                }
            });
        }
    }

    // Phase 2 — DIFF and MARK (`garbage_components`), then map the index
    // components back onto entity pointers.
    let components: Vec<Vec<*mut RcHeader>> = garbage_components(n, &rc, &edges)
        .into_iter()
        .map(|members| members.into_iter().map(|i| entities[i as usize]).collect())
        .collect();
    stats.candidate_components = components.len();

    // Phase 4 — VERIFY and RELEASE, inline. The exact test runs first,
    // for every component, before any guard or destructor mutates
    // anything: counted references account exactly, so
    // `refcount == in-component in-degree` says every reference comes
    // from inside the component — garbage by the central identity.
    let mut confirmed: Vec<Vec<*mut RcHeader>> = Vec::new();
    for members in components {
        if unsafe { exact_test(&members, 0) } {
            confirmed.push(members);
        } else {
            stats.acquitted += 1;
        }
    }

    // Guard every confirmed member (`+= 1`): a release from inside a
    // destructor — of any confirmed component — stops at the guard,
    // never at zero.
    for members in &confirmed {
        for &m in members {
            unsafe { (*m).refcount += 1 };
        }
    }

    // Null every confirmed member's weak cell BEFORE any destructor runs
    // — the binding obligation of `rfc/model/gc/rc-walk.md`: a weak load
    // is the one channel that could hand a destructor a member the exact
    // test cannot account for. Irrevocable if the re-verify acquits.
    for members in &confirmed {
        unsafe { crate::weak::notify_members(members) };
    }

    // Run each pending `__destruct` exactly once. PHP code: it may store,
    // release, allocate, resurrect — a store retains normally.
    //
    // The `is_object` gates here and below must widen to cover the Lazy
    // kind when A2 starts producing it: a lazy object carries a class
    // pointer and its `__destruct` would otherwise never run. Sever and
    // death already name it (`sever_cells`, `object::ll_entity_die`).
    let mut any_destructor_ran = false;
    for members in &confirmed {
        for &m in members {
            if is_object(unsafe { (*m).flags }) {
                any_destructor_ran |=
                    unsafe { crate::object::run_pre_destructor(m as *mut Object) };
            }
        }
    }

    // Re-verify with the guard discounted (`rc − 1 = indeg`, finding F1:
    // without the discount the guard itself acquits every component and
    // nothing is ever freed). A destructor that stored a member anywhere
    // gave it RC > IN beyond the guard — the component is acquitted,
    // guards come off through `ll_release`, survivors live on with true
    // counts and their destructors behind them.
    //
    // Skipped wholesale when no destructor ran anywhere: the only writes
    // since the first exact test were our own guards (+1 each, exactly
    // the discount), so the re-verify would recompute the identical
    // equality. Destructor-less classes are the common case, and this
    // saves the second trace of every component. Global flag, not
    // per-component, so the skip owes nothing to any cross-component
    // reasoning about what a destructor can reach.
    for members in confirmed {
        if any_destructor_ran && !unsafe { exact_test(&members, 1) } {
            stats.acquitted += 1;
            stats.collected += unsafe { unguard(&members) };
            continue;
        }

        // Sever, un-guard, then drop the deferred external children —
        // the shared tail (`sever_component`, `unguard`): between sever
        // and free no user code runs at all.
        let external = unsafe { sever_component(&members) };
        stats.collected += unsafe { unguard(&members) };
        // The members are gone; now the severed external children die
        // ordinarily, destructors and all. Members were GcHeap holders,
        // so the barrier's drop handles an arena escapee's hold-count
        // (`escape_lose`) exactly as member teardown would have.
        for child in external {
            unsafe { crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, child) };
        }
    }

    stats
}

/// Sever every member's counted children: each member's slots are
/// nulled and the displaced children collected; in-component children
/// are released immediately (they stop at their guards), **external
/// children are returned for the deferred drop after the members are
/// freed**. The exact test already proves no external reference to any
/// member exists, so an external `__destruct` could not name a member
/// even if it ran between sever and free — but deferring makes that a
/// structural property instead of a proof-dependent one: no user code
/// runs at all in the window (the hazard `rfc/model/gc/rc-walk-review.md`
/// leaves open around weak references).
///
/// # Safety
/// Every member must be a live, guarded component member.
unsafe fn sever_component(members: &[*mut RcHeader]) -> Vec<*mut RcHeader> {
    let member_set: HashSet<usize> = members.iter().map(|&m| m as usize).collect();
    let mut displaced: Vec<*mut RcHeader> = Vec::new();
    for &m in members {
        unsafe { sever_cells(m, entity_kind(m), &mut displaced) };
    }

    let mut external: Vec<*mut RcHeader> = Vec::new();
    for child in displaced {
        if member_set.contains(&(child as usize)) {
            let died = unsafe { ll_release(child) };
            debug_assert!(!died, "a guarded member cannot die of a sever release");
        } else {
            external.push(child);
        }
    }

    external
}

/// Phase 2 of `rfc/model/gc/rc-walk.md` — DIFF and MARK over a private
/// snapshot, shared by the synchronous collection and the concurrent
/// collector's judge step. Roots are computed, not enumerated:
/// `RC − IN > 0` means something outside the walked population holds
/// the entity. Unmarked entities are grouped into **weakly** connected
/// components — edges followed in both directions, so a garland of
/// linked garbage rings is judged as one unit (decided 2026-07-26).
/// Pure array math: nothing here touches shared memory.
pub(crate) fn garbage_components(n: usize, rc: &[u32], edges: &[(u32, u32)]) -> Vec<Vec<u32>> {
    let mut in_degree = vec![0u32; n];
    for &(_, dst) in edges {
        in_degree[dst as usize] += 1;
    }

    let mut marked = vec![false; n];
    let mut stack: Vec<u32> = (0..n as u32)
        .filter(|&i| rc[i as usize] > in_degree[i as usize])
        .collect();
    for &i in &stack {
        marked[i as usize] = true;
    }

    // Forward adjacency (CSR) for the mark walk.
    let mut offsets = vec![0u32; n + 1];
    for &(src, _) in edges {
        offsets[src as usize + 1] += 1;
    }

    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }

    let mut forward = vec![0u32; edges.len()];
    let mut cursor = offsets.clone();
    for &(src, dst) in edges {
        forward[cursor[src as usize] as usize] = dst;
        cursor[src as usize] += 1;
    }
    while let Some(i) = stack.pop() {
        for k in offsets[i as usize]..offsets[i as usize + 1] {
            let j = forward[k as usize];
            if !marked[j as usize] {
                marked[j as usize] = true;
                stack.push(j);
            }
        }
    }

    let mut undirected: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(src, dst) in edges {
        if !marked[src as usize] && !marked[dst as usize] {
            undirected[src as usize].push(dst);
            undirected[dst as usize].push(src);
        }
    }

    let mut component_of = vec![u32::MAX; n];
    let mut components: Vec<Vec<u32>> = Vec::new();
    for i in 0..n as u32 {
        if marked[i as usize] || component_of[i as usize] != u32::MAX {
            continue;
        }

        let id = components.len() as u32;
        let mut members = Vec::new();
        let mut queue = vec![i];
        component_of[i as usize] = id;
        while let Some(v) = queue.pop() {
            members.push(v);
            for &w in &undirected[v as usize] {
                if component_of[w as usize] == u32::MAX {
                    component_of[w as usize] = id;
                    queue.push(w);
                }
            }
        }

        components.push(members);
    }

    components
}

/// The exact test over one component's **current** fields:
/// `refcount == in-component in-degree + discount` for every member
/// (`discount` is 1 while the Phase 4 guard is outstanding, else 0).
unsafe fn exact_test(members: &[*mut RcHeader], discount: u32) -> bool {
    // The corpse rule, before any tracing (eager-death amendment,
    // 2026-07-27, `rfc/model/gc/rc-walk.md` Phase 4): a member at rc 0
    // is a corpse — it died ordinarily since the verdict was posted,
    // its teardown is complete and its free is parked. Its fields are
    // teardown residue; the message is dropped whole before any field
    // of any member is traced and before any guard is written.
    // rc-trace has no condemnation and no epoch: nothing dies between
    // its stop-the-thread collection and this test.
    #[cfg(feature = "rc-walk")]
    if discount == 0
        && members
            .iter()
            .any(|&m| unsafe { crate::refcount::header_refcount(m) } == 0)
    {
        return false;
    }

    let local: HashMap<usize, u32> = members
        .iter()
        .enumerate()
        .map(|(i, &m)| (m as usize, i as u32))
        .collect();
    let mut in_degree = vec![0u32; members.len()];
    for &m in members {
        unsafe {
            trace_entity(m, |child| {
                if let Some(&j) = local.get(&(child as usize)) {
                    in_degree[j as usize] += 1;
                }
            });
        }
    }

    members
        .iter()
        .enumerate()
        .all(|(i, &m)| unsafe { crate::refcount::header_refcount(m) } == in_degree[i] + discount)
}

/// Drop the Phase 4 guards through `ll_release` — never a raw `-= 1`: a
/// member that reaches zero dies through the proven teardown; an
/// acquittal survivor keeps its true count and lives on. On the
/// confirmed path every member reaches zero here: external drops are
/// deferred past this point, so nothing can have retained a member since
/// the re-verify. Returns how many members died.
unsafe fn unguard(members: &[*mut RcHeader]) -> usize {
    let mut collected = 0;
    for &m in members {
        if unsafe { ll_release(m) } {
            unsafe { crate::object::ll_entity_die(m) };
            collected += 1;
        }
    }

    collected
}

// --- The message drain (rc-walk build step 3) -------------------------------

/// Outcome of draining one posted component.
#[cfg(feature = "rc-walk")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DrainOutcome {
    /// Members torn down.
    pub collected: usize,
    /// The message was dropped — a corpse in the component, an
    /// exact-test mismatch, or a destructor resurrection. A drop
    /// leaves nothing behind to clean: acquittal carries no duties
    /// since the eager-death amendment (2026-07-27).
    pub acquitted: bool,
}

/// Drain one **confirmed** component posted by the collector — Phase 4
/// of `rfc/model/gc/rc-walk.md`, on the owning mutator thread, trusting
/// nothing it was told. The exact test opens with the corpse rule: a
/// member reading `rc 0` died ordinarily since the verdict was posted
/// (eager death — teardown complete, free parked) and drops the
/// message whole before any field is traced or guard written. A
/// destructor's release into a *different* posted component dies
/// ordinarily too; that component's own drain then drops on the
/// corpse — one epoch of latency, the collector's currency.
///
/// # Safety
/// Members must be entities of one posted component, on their owning
/// thread; no other drain may hold guards on them.
#[cfg(feature = "rc-walk")]
pub(crate) unsafe fn drain_confirmed(members: &[*mut RcHeader]) -> DrainOutcome {
    // The exact test first (corpse rule included), against current
    // fields, race-free on this thread. Any mismatch drops the message
    // whole; a drop does nothing else — there are no bytes to clear
    // and no deferred deaths to tear.
    if !unsafe { exact_test(members, 0) } {
        return DrainOutcome {
            collected: 0,
            acquitted: true,
        };
    }

    // Confirmed: the members are ours — the equality just proved no
    // reference from outside the component exists.
    //
    // Header accesses throughout the drain go through the relaxed
    // helpers like every other post-publish access, although the drain
    // window is provably free of collector interference
    // (rfc/model/gc/drain-window.md, TLC-checked): the rule stays
    // absolute so no reader needs the proof to trust the site.
    for &m in members {
        // The guard.
        unsafe { crate::refcount::mutator_guard_retain(m) };
    }

    // Weak cells nulled before any destructor — same obligation and
    // ordering as `collect_cycles` (`rfc/model/weak-references.md`).
    unsafe { crate::weak::notify_members(members) };
    let mut any_destructor_ran = false;
    for &m in members {
        if is_object(unsafe { crate::refcount::header_flags(m) }) {
            any_destructor_ran |= unsafe { crate::object::run_pre_destructor(m as *mut Object) };
        }
    }

    // Guard-discounted re-verify (finding F1), skipped when no
    // destructor ran — same reasoning as in `collect_cycles`.
    if any_destructor_ran && !unsafe { exact_test(members, 1) } {
        // Resurrection: guards come off through `ll_release`, survivors
        // keep true counts, destructors are behind them.
        return DrainOutcome {
            collected: unsafe { unguard(members) },
            acquitted: true,
        };
    }

    let external = unsafe { sever_component(members) };
    let collected = unsafe { unguard(members) };
    for child in external {
        unsafe { crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, child) };
    }

    DrainOutcome {
        collected,
        acquitted: false,
    }
}

#[cfg(test)]
mod tests;
