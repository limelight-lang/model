//! Object creation and the three-phase teardown of an *object*
//! (`rfc/runtime/object-lifecycle.md`). Cycle finalization and arena reset
//! order the same acts differently, and each owns its own numbering.
//!
//! `ll_object_new` is the out-of-line allocation path — the compiler
//! inlines the bump-pointer version when class and category are
//! statically known, but both perform the same steps. `ll_object_die`
//! is the teardown entry every strategy funnels into: it dispatches to
//! the class's `dispose` (user destructor with resurrection check, then
//! drop of counted children), then frees the memory by category.
//! `dispose` is a descriptor pointer — [`ll_default_dispose`] is the
//! generic stand-in a class carries until the compiler generates one
//! specialized to its layout (`dev/DECISIONS.md`, "a generated lifecycle
//! body unrolls small, loops large").

use crate::class::{Class, NO_DESTRUCT_SLOT};
use crate::journal::kinds::journal_event;
use crate::memory::context::{LLContext, resolve_arena};
use crate::refcount::{DESTRUCTOR_PENDING, DESTRUCTOR_RAN, MemoryCategory, RcHeader};
use crate::value::Value;

/// Object layout (`rfc/model/classes.md`): header, class pointer, then
/// machine-typed property slots at the offsets `Class::props` assigns — a raw
/// `i64`/`f64` scalar, a bare pointer, or a 16-byte Box, each the
/// representation of its declared type.
#[repr(C)]
pub struct Object {
    pub rc: RcHeader,
    pub class: *const Class,
    // property slots follow at +16
}

/// `__destruct` through its vtable slot: an ordinary virtual method.
pub type DestructorFn = unsafe extern "C" fn(*mut Object);

impl Object {
    /// The object's class descriptor: the class word, stamped by the
    /// factory before the header is published and never written again.
    /// Covers the descriptor's fixed fields only — the trailing vtable is
    /// reached through the raw pointer ([`Class::vtbl`]).
    #[inline]
    pub fn class(&self) -> &Class {
        unsafe { &*self.class }
    }

    /// The property slot at a `PropSlot::offset`.
    ///
    /// Takes the object as a raw pointer rather than `&mut self` on
    /// purpose: slots live *past* `size_of::<Object>()`, and a reference
    /// only carries provenance over the 16-byte header it points to.
    /// Deriving a slot pointer from `&mut self` therefore puts every slot
    /// access outside the borrow it came from — real UB under
    /// Stacked/Tree Borrows, which Miri reports on the first store
    /// (audit `class.rs:115`). The caller's raw pointer spans the whole
    /// allocation, so deriving from it keeps the access in bounds.
    ///
    /// # Safety
    /// `obj` must point to a live object allocated with its class's
    /// `object_size`, and `offset` must come from that class's layout.
    #[inline]
    pub unsafe fn prop_at(obj: *mut Object, offset: u32) -> *mut Value {
        unsafe { (obj as *mut u8).add(offset as usize) as *mut Value }
    }

    /// Test the init-bitmap bit of a bitmap-tracked raw slot: clear =
    /// uninitialized (`rfc/model/values.md`, "Uninitialized properties").
    /// `init_bit` is the slot's [`crate::class::PropSlot::init_bit`], an
    /// absolute bit position in the object's byte block. Generated code
    /// emits these beside the slot access: a read tests and throws on
    /// clear, a write stores the value and sets the bit, `unset` clears
    /// it (a pointer slot's `unset` also stores `NULL` through the
    /// barrier). Plain byte accesses are sound: the byte block is
    /// mutator-only data — no walker reads outside the traced runs.
    ///
    /// # Safety
    /// `obj` per [`Self::prop_at`]; `init_bit` must come from the class's
    /// layout (never [`crate::class::NO_INIT_BIT`]).
    #[inline]
    pub unsafe fn init_bit_test(obj: *const Object, init_bit: u32) -> bool {
        debug_assert_ne!(init_bit, crate::class::NO_INIT_BIT);
        let byte = unsafe { (obj as *const u8).add((init_bit / 8) as usize) };
        (unsafe { byte.read() } & (1u8 << (init_bit % 8))) != 0
    }

    /// Set the bit: the slot was written. See [`Self::init_bit_test`].
    ///
    /// # Safety
    /// As [`Self::init_bit_test`], with the object writable.
    #[inline]
    pub unsafe fn init_bit_set(obj: *mut Object, init_bit: u32) {
        debug_assert_ne!(init_bit, crate::class::NO_INIT_BIT);
        let byte = unsafe { (obj as *mut u8).add((init_bit / 8) as usize) };
        unsafe { byte.write(byte.read() | 1u8 << (init_bit % 8)) };
    }

    /// Clear the bit: `unset()` returns the slot to uninitialized. See
    /// [`Self::init_bit_test`].
    ///
    /// # Safety
    /// As [`Self::init_bit_set`].
    #[inline]
    pub unsafe fn init_bit_clear(obj: *mut Object, init_bit: u32) {
        debug_assert_ne!(init_bit, crate::class::NO_INIT_BIT);
        let byte = unsafe { (obj as *mut u8).add((init_bit / 8) as usize) };
        unsafe { byte.write(byte.read() & !(1u8 << (init_bit % 8))) };
    }

    /// Stable for the object's lifetime (non-moving heap), so the id
    /// is derived from the address; retained arena survivors keep it,
    /// evacuated ones get the lazy stored id (`arena-reset.md`).
    #[inline]
    pub fn object_id(&self) -> usize {
        self as *const Object as usize
    }
}

/// Allocate and initialize an instance of `class` in `category`.
/// The `__construct` call is emitted by the compiler at the call site.
///
/// This is the typed Rust entry; the C ABI symbol `ll_object_new` is the
/// [`ll_object_new_abi`] wrapper, which takes the category as a plain
/// `u32` so a bad value from generated code is not an invalid-enum UB.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`]; `class` must
/// be a linked descriptor.
pub unsafe fn ll_object_new(
    ctx: *mut LLContext,
    class: *const Class,
    category: MemoryCategory,
) -> *mut Object {
    let cls = unsafe { &*class };
    let size = cls.object_size as usize;

    let mem = unsafe { crate::memory::routing::entity_alloc_in(ctx, category, size) };
    // Out of memory. The caller raises; nothing here is half-built,
    // because nothing was built (`rfc/runtime/exceptions.md`: the Rust
    // core reports, a Limelight frame raises).
    if mem.is_null() {
        return std::ptr::null_mut();
    }

    // The destructor is NOT registered here. The factory only produces
    // the object; `__destruct` is owed only once the user constructor has
    // returned successfully (`rfc/runtime/object-lifecycle.md`, "Two
    // constructors"), and registering earlier would demand a `__destruct`
    // for exactly the objects whose constructor threw.
    unsafe { stamp_into(mem, class, size, category) }
}

/// The factory's initialization half, shared with the construct-into-cell
/// path (`rfc/model/memory/bulk-operations.md`): zero the body, set the
/// class word, publish the header last.
unsafe fn stamp_into(
    mem: *mut u8,
    class: *const Class,
    size: usize,
    category: MemoryCategory,
) -> *mut Object {
    // A template's size comes from its shape, so this factory would
    // allocate a body of 16 bytes and every walker would then read the
    // shape word past the end of it (`crate::template::ll_template_new`
    // is the one that builds these).
    debug_assert!(
        unsafe { crate::class::Class::flags_of(class) } & crate::class::CLASS_TEMPLATE == 0,
        "a template is built by its own factory, not by the object one"
    );
    let obj = mem as *mut Object;

    // No `DESTRUCTOR_PENDING` here: the flag is set by `object_constructed`.
    // Object is the zero kind field, so this contributes no bits; it is
    // written out to keep the factory's produced kind explicit.
    let extra = crate::refcount::EntityKind::Object.to_flags();
    unsafe {
        // Zero-fill the property region in one pass: a null pointer is
        // uninitialized, an all-zero Box is `null` (tag 0, refcounted
        // clear), a zero scalar is 0, a clear bool is false, a clear
        // init-bitmap bit is uninitialized — every slot's correct start
        // at once.
        // `size >= size_of::<Object>()` always (16-byte header).
        let body = (obj as *mut u8).add(size_of::<Object>());
        core::ptr::write_bytes(body, 0, size - size_of::<Object>());
        // A defaultless `mixed`/untyped Box slot starts *undefined*, and
        // an all-zero Box is `null` — stamp those few slots from the
        // descriptor's undef runs (`rfc/model/values.md`, Construction).
        // Non-zero property defaults stay the compiler's explicit stores
        // at the `new` site; this generic factory is the out-of-line path
        // and reads the descriptor. Plain stores are sound here: the
        // header is not yet published, so no walker reads these slots.
        for run in (*class).undef_runs() {
            for i in 0..run.count {
                let slot = (obj as *mut u8).add((run.offset + i * 16) as usize) as *mut Value;
                slot.write(Value::undef());
            }
        }

        (*obj).class = class;
        // The header is published LAST (`publish_header`: one 8-byte
        // relaxed-atomic store). Until it lands the slot reads refcount
        // 0, so a trace crossing the block classifies it as free rather
        // than reading a half-built entity.
        crate::refcount::publish_header(obj as *mut RcHeader, RcHeader::new(category, extra));
    }

    obj
}

/// Construct an instance of `class` into a reserved entity cell
/// (`rfc/model/memory/bulk-operations.md`): the cell came from
/// `ll_entity_reserve` for this class's object size, so there is no
/// allocation here — zero-fill, class word, header published last, the
/// same steps as [`ll_object_new`]. Cells are GcHeap entities by
/// construction; a cell is single-use.
///
/// # Safety
/// `cell` must be an unconsumed cell reserved for at least
/// `class.object_size` bytes; `class` must be a linked descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_object_new_in(cell: *mut u8, class: *const Class) -> *mut Object {
    let size = unsafe { (*class).object_size } as usize;
    unsafe { stamp_into(cell, class, size, MemoryCategory::GcHeap) }
}

/// Release a vector of references in one call
/// (`rfc/model/memory/bulk-operations.md`): the safepoint is split
/// around the run — the acknowledgement at entry, before any death, and
/// the full poll after the last release, where the run's transients are
/// back at their true counts. Each entry is a batched release;
/// destructors run in vector order. Both halves have empty bodies while
/// no collector is wired (`gc.rs`).
///
/// # Safety
/// Every element must point to a live heap entity beginning with
/// `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_release_vector(entities: *const *mut RcHeader, count: usize) {
    unsafe { crate::gc::ll_gc_checkpoint_ack() };
    for i in 0..count {
        // The poll contract binds this loop too, and it is the runtime's
        // to keep here: `count` is the caller's, the compiler emits its
        // poll only after the call, and every iteration can register a
        // candidate. Without this the queue's funding is never refilled
        // mid-run and a large enough clear reaches the abort below the
        // overflow buffer with memory free (`rfc/dev/DECISIONS.md`,
        // "a runtime loop carries the poll contract it broke").
        //
        // On the backedge, where refcounts and edges agree: iteration
        // `i - 1` has fully returned, its death and destructor with it,
        // and `entities[i]` has not been read
        // (`rfc/model/gc/strategies.md`, "Collection requests and triggers").
        if i != 0 && i % crate::cycle::queue::POLL_STRIDE == 0 {
            unsafe { crate::gc::ll_gc_maybe_collect() };
        }

        let entity = unsafe { *entities.add(i) };
        if unsafe { crate::refcount::ll_release_batch(entity) } {
            unsafe { ll_entity_die(entity) };
        }
    }

    unsafe { crate::gc::ll_gc_checkpoint() };
}

/// The user constructor returned successfully: from here on the object
/// owes a `__destruct`. Sets the header flag teardown dispatches on, and
/// for an arena object records it in the arena's destructor log, which is
/// what makes reset run the user destructor.
///
/// **False when the record could not be written.** The caller raises
/// memory-exhausted at the creation site, and the outcome is identical to
/// a constructor that threw (`rfc/runtime/object-lifecycle.md`).
///
/// A class without a destructor needs no call; generated code emits it
/// only where the class has one.
///
/// # Safety
/// `obj` must be a live object whose constructor has just returned.
#[must_use]
pub unsafe fn object_constructed(ctx: *mut LLContext, obj: *mut Object) -> bool {
    let cls = unsafe { (*obj).class() };
    if !cls.has_destructor() {
        return true;
    }

    if unsafe { header_category(obj as *const RcHeader) } == MemoryCategory::RequestArena
        && !unsafe { (*resolve_arena(ctx)).track_destructor(obj as *mut RcHeader) }
    {
        return false;
    }

    // Post-publish header write: races a collector's byte stores into
    // the same word, so it goes through the relaxed flags helper.
    unsafe {
        crate::refcount::update_header_flags(obj as *mut RcHeader, |f| f | DESTRUCTOR_PENDING)
    };

    true
}

/// C ABI for [`object_constructed`].
///
/// # Safety
/// As [`object_constructed`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_object_constructed(ctx: *mut LLContext, obj: *mut Object) -> bool {
    unsafe { object_constructed(ctx, obj) }
}

/// Test helper: create an object and complete its construction the way
/// generated code does — factory, then `object_constructed` once the
/// user constructor would have returned. Tests model generated code, so
/// they must take both steps.
#[cfg(test)]
pub(crate) unsafe fn new_constructed(
    ctx: *mut LLContext,
    class: *const Class,
    category: MemoryCategory,
) -> *mut Object {
    let obj = unsafe { ll_object_new(ctx, class, category) };
    assert!(!obj.is_null(), "allocation refused in a test");
    assert!(
        unsafe { object_constructed(ctx, obj) },
        "destructor registration refused in a test"
    );
    obj
}

/// C ABI entry for [`ll_object_new`]. The category crosses the boundary as
/// a plain `u32` — the wire representation of the `#[repr(u32)]` enum, but a
/// type that accepts any bit pattern. A value outside `0..=3` from generated
/// code is therefore caught (debug) or masked to a valid category (release)
/// rather than being instant UB the moment it is materialized as a
/// `MemoryCategory`.
///
/// # Safety
/// As [`ll_object_new`]; `category` must be a valid `MemoryCategory` code
/// (`0..=3`) per the codegen contract.
#[unsafe(export_name = "ll_object_new")]
pub unsafe extern "C" fn ll_object_new_abi(
    ctx: *mut LLContext,
    class: *const Class,
    category: u32,
) -> *mut Object {
    debug_assert!(
        category <= MemoryCategory::Immortal as u32,
        "MemoryCategory out of range across the ABI: {category}"
    );
    // `from_flags` masks to the 2-bit category field, so the enum value is
    // always in range — no invalid-discriminant load.
    unsafe { ll_object_new(ctx, class, MemoryCategory::from_flags(category)) }
}

/// Visit every live counted child of an object — [`for_each_counted_cell`]
/// over the object's own body, for GC tracing, teardown, promotion and
/// escape-release (`rfc/model/gc/strategies.md` §4), so a template
/// instance's values are walked from its shape like any other child. Each
/// child is yielded once as a non-null `*mut RcHeader`; the slot lvalue is
/// not exposed, since a store goes through the barrier (which knows the
/// slot kind statically), not through here.
///
/// Generic over the visitor and `#[inline]` so each caller monomorphizes to
/// a bare stride with no per-child indirect call (`rfc/model/classes.md`,
/// "Why tracing stays data").
///
/// # Safety
/// `obj` must point to a live object whose slots are still readable.
#[inline]
pub(crate) unsafe fn for_each_counted_child(
    obj: *mut Object,
    mut visit: impl FnMut(*mut RcHeader),
) {
    let cls = unsafe { (*obj).class() };
    unsafe {
        for_each_counted_cell::<crate::cells::PlainCells>(obj as *mut u8, cls, |cell| {
            visit(cell.child)
        })
    };
}

/// The object layout's counted cells — **the one place that knows where
/// an object keeps children**, and the stride every operation over them
/// goes through: tracing, severing, the arena reset's walk, and the
/// concurrent collector alike.
///
/// `R` is how the memory is read (`crate::cells::CellReader`).
///
/// Keyed on `(base, cls)` rather than on an entity pointer, because one
/// caller has no entity to read a class from: a static block is a
/// headerless region laid out by a descriptor (A6,
/// `crate::static_block`).
///
/// The **descriptor and the shape are read plainly under either reader**.
/// They are immortal static data no mutator writes; what goes through the
/// reader is the entity's own word that points at them.
///
/// Generic over the visitor and `#[inline]`, so every instantiation
/// monomorphizes to a bare stride with no indirect call per child — the
/// contract `rfc/model/classes.md` states as "Why tracing stays data".
///
/// # Safety
/// `base` addresses a live region laid out by `cls`, and its cells are
/// readable — under a concurrent reader `R` they may be written while
/// this runs.
#[inline]
pub(crate) unsafe fn for_each_counted_cell<R: crate::cells::CellReader>(
    base: *mut u8,
    cls: *const crate::class::Class,
    mut visit: impl FnMut(crate::cells::Cell),
) {
    unsafe { for_each_body_cell::<R>(base, cls, &mut visit) };

    // The outside cells come after the body's, never instead of them: a
    // subclass declares properties of its own and those live in the runs,
    // so replacing the stride would leave them untraced — a computed root
    // and a ring that never collects.
    let Some(group) = (unsafe { crate::class::Class::outside_cells(cls) }) else {
        return;
    };
    unsafe { R::walk_outside(group, base, cls, &mut visit) }
}

/// The cells inside the entity's own body, and none outside it: the
/// template's shape-counted values, or the two run kinds in order.
///
/// Separate from [`for_each_counted_cell`] because the sever needs
/// exactly this half — `cells::empty_cell` writes a whole `Value` or a
/// bare `NULL`, which is right for a cell in the body and wrong for
/// whatever a class keeps outside it.
///
/// # Safety
/// As [`for_each_counted_cell`].
#[inline]
pub(crate) unsafe fn for_each_body_cell<R: crate::cells::CellReader>(
    base: *mut u8,
    cls: *const crate::class::Class,
    visit: &mut impl FnMut(crate::cells::Cell),
) {
    // A template's children are counted by its shape, because one class
    // serves every interpolation site and the runs would have to differ
    // per instance (`crate::template`).
    if unsafe { crate::class::Class::flags_of(cls) } & crate::class::CLASS_TEMPLATE != 0 {
        let n = unsafe { crate::template::value_count_at::<R>(base) };
        for i in 0..n {
            let at = unsafe { base.add(crate::template::VALUES_OFFSET + i * 16) };
            if let Some(cell) = unsafe { crate::cells::counted_box_cell::<R>(at) } {
                visit(cell);
            }
        }

        return;
    }

    // Pointer runs: bare 8-byte pointers, `NULL` is empty.
    for run in unsafe { (*cls).ptr_runs() } {
        for i in 0..run.count {
            let at = unsafe { base.add((run.offset + i * 8) as usize) };
            let child = unsafe { R::ptr(at) } as *mut RcHeader;
            if !child.is_null() {
                visit(crate::cells::Cell {
                    addr: at as usize,
                    child,
                    shape: crate::cells::CellShape::Pointer,
                });
            }
        }
    }

    // Box runs: 16-byte Values, empty is the refcounted flag clear.
    for run in unsafe { (*cls).box_runs() } {
        for i in 0..run.count {
            let at = unsafe { base.add((run.offset + i * 16) as usize) };
            if let Some(cell) = unsafe { crate::cells::counted_box_cell::<R>(at) } {
                visit(cell);
            }
        }
    }
}

/// Sever the counted slots of a region laid out by `cls`: empty each cell
/// and collect its former occupant into `displaced`, **without dropping
/// it** — the caller owes one drop per entry (`cells::sever_cells`, which
/// is the dispatch this serves).
///
/// Takes a base and a descriptor rather than an entity because a static
/// block carries no header to read a class from (A6) — which is also why
/// it strides the body alone: a class's outside cells are severed through
/// the group, and the group takes an entity. A static block's layout may
/// not carry [`crate::class::CLASS_OUTSIDE_CELLS`], and
/// `ll_static_block_register` says so.
///
/// One caller, the thread-exit pass over static blocks. The drain severs
/// an entity through `cells::sever_cells`, which reaches the group.
///
/// # Safety
/// `base` must address a live region laid out by `cls`, with its slots
/// readable and writable.
pub(crate) unsafe fn sever_counted_slots(
    base: *mut u8,
    cls: &crate::class::Class,
    displaced: &mut Vec<*mut RcHeader>,
) {
    unsafe {
        for_each_body_cell::<crate::cells::PlainCells>(base, cls, &mut |cell| {
            crate::cells::empty_cell(cell);
            displaced.push(cell.child);
        })
    };
}

/// Phase 1 alone: run `__destruct` exactly once (sets the guard bit).
/// Returns `false` when there was nothing to run. Arena reset uses
/// this directly — dying arena objects get only phase 1, their memory
/// and children die with the arena.
///
/// # Safety
/// `obj` must be a live object.
pub(crate) unsafe fn run_user_destructor(obj: *mut Object) -> bool {
    let cls = unsafe { (*obj).class() };
    // The header flag, not the class: a class may declare `__destruct`
    // while this particular object never finished construction, and such
    // an object must not run it (`rfc/runtime/object-lifecycle.md`).

    {
        let flags = unsafe { crate::refcount::mutator_flags(obj as *const RcHeader) };
        if flags & DESTRUCTOR_PENDING == 0 || flags & DESTRUCTOR_RAN != 0 {
            return false;
        }

        unsafe {
            crate::refcount::update_header_flags(obj as *mut RcHeader, |f| f | DESTRUCTOR_RAN)
        };
    }

    debug_assert_ne!(cls.destruct_slot, NO_DESTRUCT_SLOT);
    // Through the raw class pointer, not `cls`: the vtable trails the
    // descriptor's fixed fields, which a `&Class` does not cover.
    let code = unsafe { Class::vtbl((*obj).class) }[cls.destruct_slot as usize];
    let destruct: DestructorFn = unsafe { std::mem::transmute(code) };
    unsafe { destruct(obj) };
    true
}

/// The class's internal destructor `dispose(obj)`
/// (`rfc/model/classes.md`, "`dispose` — the runtime teardown entry"), as a
/// function-pointer type. It runs the user `__destruct` under the
/// resurrection guard and releases the object's counted children, then
/// returns whether teardown completed — `true` to free the object,
/// `false` when a resurrection kept it alive. The object's own memory free
/// is the caller's ([`ll_object_die`]).
pub type DisposeFn = unsafe extern "C" fn(*mut Object) -> bool;

/// The default `dispose`: the generic stand-in a class carries until the
/// compiler emits one specialized to its layout (`dev/DECISIONS.md`, "a
/// generated lifecycle body unrolls small, loops large"). It reads the
/// trace map (via [`for_each_counted_child`]) to
/// release children; a generated `dispose` would unroll the releases with no
/// map read, to identical effect — so a test may install its own.
///
/// Runs this protocol's phases 1–2 (the resurrection-guarded `__destruct`
/// and the child releases); phase 3, the memory free, is
/// [`ll_object_die`]'s. Returns
/// `true` to proceed to the free, `false` on resurrection.
///
/// # Safety
/// `obj` a live object whose count just reached zero (or a collector owns).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_default_dispose(obj: *mut Object) -> bool {
    // Phase 1 — user destructor: exactly once, resurrection-aware.
    //
    // Guard the destructor with one extra reference so a *transient* $this
    // reference taken inside it (`$x = $this;` then `$x` leaves scope: a
    // retain followed by a release) cannot drive the count to zero and
    // re-enter teardown while we are still in it — that would free `obj`
    // here and again below (double free). A genuine resurrection (a
    // reference that outlives the destructor) leaves the count above the
    // guard, and is detected after it is dropped. The guard is only
    // meaningful for lifetime-counted (GcHeap) objects; arena objects are
    // not counted, so a $this reference there is a no-op anyway.

    {
        // Header traffic through the relaxed header helpers: a collector
        // reads this header concurrently, and the guard's transient
        // `rc 0 → 1 → 0` is visible to it. A count read high is the safe
        // direction — the entity reads as externally referenced and
        // survives one collection.
        let flags = unsafe { crate::refcount::mutator_flags(obj as *const RcHeader) };
        let counted = MemoryCategory::from_flags(flags) == MemoryCategory::GcHeap;
        if counted {
            unsafe { crate::refcount::mutator_guard_retain(obj as *mut RcHeader) };
        }

        let ran = unsafe { run_user_destructor(obj) };
        if counted {
            // Eager death (2026-07-27): a collection reading this entity
            // as garbage while the destructor ran changes nothing —
            // teardown always finishes, and the zero-count rule drops a
            // component holding a member already at zero
            // (`rfc/model/gc/rc-cycle.md`, "Cycle finalization and reclamation", step 1).
            let refcount =
                unsafe { crate::refcount::mutator_unguard_release(obj as *mut RcHeader) };
            if ran && refcount > 0 {
                return false; // resurrected
            }
        } else if ran && unsafe { crate::refcount::header_refcount(obj as *const RcHeader) } > 0 {
            return false;
        }
    }

    // Phase 2, first act — weak notification, BEFORE any child release:
    // the drops below cascade into user code, and a child's `__destruct`
    // calling `get()` on this refcount-zero object would receive a strong
    // reference that outlives the free (`rfc/model/weak-references.md`;
    // regression: `weak::tests::when_the_notification_arrives::
    // own_destructor_still_sees_the_object_but_a_child_destructor_sees_null`).
    // Read the flags fresh: the `__destruct` above may itself have created
    // the weak state.
    if unsafe { crate::refcount::mutator_flags(obj as *const RcHeader) }
        & crate::refcount::HAS_WEAK_REFERENCES
        != 0
    {
        unsafe { crate::weak::notify_death(obj as *mut RcHeader) };
    }

    // Phase 2 — drop each counted child through the barrier's `drop`
    // micro-op: escape-lose for a held arena escapee, release + cascade
    // otherwise. `owner_cat` is this object's category — always GcHeap on
    // this path (only GcHeap objects reach full teardown; arena objects get
    // phase 1 only at reset), and passing it makes teardown's drop identical
    // to the store barrier's.
    let owner_cat = unsafe { header_category(obj as *const RcHeader) };
    unsafe {
        for_each_counted_child(obj, |child| {
            crate::memory::barrier::drop_ref(owner_cat, child);
        });
    }

    // Phase 2, last act — the storage a class keeps outside its own body,
    // after its cells have been released and never before. Here rather
    // than left to a specialized `dispose`, because this is the only body
    // a class inherits without writing one: a subclass of a class with
    // outside cells declares properties of its own, gets this default,
    // and would otherwise leave the parent's storage behind on every
    // death (`dev/DECISIONS.md`, "a class with cells outside itself
    // carries one flag and one group of five"). A class that installs its
    // own `dispose` owes the same call.
    let cls = unsafe { (*obj).class };
    if let Some(group) = unsafe { crate::class::Class::outside_cells(cls) } {
        unsafe { (group.free)(obj as *mut RcHeader) };
    }

    true
}

/// Teardown entry: dispatch to the class's `dispose` (phases 1–2), then
/// free the object's own memory if it completed (phase 3). Called when the
/// refcount reaches zero or a collector proves the object garbage.
///
/// The teardown body lives in `obj->class->dispose` — one indirect call
/// into per-class code, the collector holding only `obj`
/// (`rfc/model/classes.md`). This crate installs [`ll_default_dispose`] as
/// the stand-in; the memory free stays here, generic and by category.
///
/// # Safety
/// `obj` a live object whose count just reached zero (or a collector owns).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_object_die(obj: *mut Object) {
    // Objects and lazies alike, and the kind is read rather than assumed:
    // this entry point takes both, and a reader hunting one of them cannot tell
    // them apart by address. Before `dispose`, because a `__destruct`
    // body's own records belong after the death that caused them.
    journal_event!(
        crate::journal::kinds::KIND_ENTITY_DEATH,
        obj as u64,
        ((unsafe { crate::refcount::mutator_flags(obj as *const RcHeader) }
            & crate::refcount::ENTITY_KIND_MASK)
            >> crate::refcount::ENTITY_KIND_SHIFT) as u64,
        0
    );
    // The teardown bracket that guarded this window died with the two
    // collectors it belonged to, and `rc-cycle` guards it by another
    // mechanism: a slot that dies while a queue entry names it is
    // withheld from the allocator by the free below, and only the
    // retirement of that entry returns it (`rfc/model/gc/rc-cycle.md`,
    // "Zero-count entities pending slot reuse"; `memory::stdapi::ll_free`). So the body a
    // reader may still reach through the entry stays where the death
    // left it.

    let dispose: DisposeFn = unsafe { std::mem::transmute((*(*obj).class).dispose) };
    if unsafe { dispose(obj) } {
        // Teardown completed — the arm a resurrection never reaches — so
        // this is where a reset in flight learns of the death and takes
        // over what the entity held (`memory::reset_window`).
        crate::memory::reset_window::record_death(obj as *mut RcHeader);
        // Under `rc-trace` the object left the candidate buffer here, after
        // `dispose` rather than before it, because `__destruct` can make the
        // object a candidate afresh — a transient `$this` inside it is a retain
        // and a release, and that release is a non-zero decrement. `rc-cycle`
        // cannot withdraw a queue entry at all and pays the same fact by
        // withholding the slot instead, which the free in phase 3 below does.
        // The ordering the buffer needed is kept by where that free stands:
        // after `dispose`, never before.

        // Phase 3 — memory, by category. Arenas reclaim at reset; the
        // long-lived policy is TBD; only the GC heap frees here.
        if unsafe { header_category(obj as *const RcHeader) } == MemoryCategory::GcHeap {
            unsafe { crate::memory::stdapi::ll_free(obj as *mut u8) };
        }
    } // else resurrected: kept alive, not freed
}

/// The category of a published header, through the relaxed read every
/// header access uses: a collector's byte stores race a plain one
/// ([`crate::refcount::mutator_flags`]).
#[inline]
pub(crate) unsafe fn header_category(header: *const RcHeader) -> MemoryCategory {
    MemoryCategory::from_flags(unsafe { crate::refcount::mutator_flags(header) })
}

/// Teardown for a **bare entity pointer**: the kind field selects the
/// free routine (`rfc/model/classes.md`, "Entity kind and non-object
/// teardown") — one flags load and a small switch. Object and lazy
/// carry a class pointer and dispatch through its `dispose`; a
/// reference box releases its one Value and frees; a weak cell
/// unregisters from the weak table; a string and an array free the
/// body they own outside their own slot. Box gains its arm when the
/// crate can produce one (FFI), and reaching it today is a bug, not a
/// leak policy; that arm owes the `HAS_WEAK_REFERENCES` weak-notify
/// test — a Box is a legal `WeakReference` target
/// (`rfc/model/weak-references.md`, "Death notification").
///
/// # Safety
/// `entity` must be a live entity whose count just reached zero (or a
/// collector owns it).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_entity_die(entity: *mut RcHeader) {
    use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind};
    const OBJECT: u32 = EntityKind::Object as u32;
    const LAZY: u32 = EntityKind::Lazy as u32;
    const REFERENCE: u32 = EntityKind::Reference as u32;
    const WEAKREF: u32 = EntityKind::WeakRef as u32;
    const STRING: u32 = EntityKind::String as u32;
    const STRING_DYNAMIC: u32 = EntityKind::StringDynamic as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    let flags = unsafe { crate::refcount::mutator_flags(entity) };
    let kind = (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
    // What a dying registered slot owes a collector is done here, for every
    // kind the gate admits, so a kind that gains counted slots later inherits
    // it without a call site of its own
    // (`refcount::EntityKind::closes_a_ring`). An array runs no `dispose`,
    // which is where an object would do it on its way past the free, so this
    // entry point is the array's too — with one exception owing the same duty
    // at its own site: a **nested** array is torn down by `array_die`'s drain
    // and never passes here again (`array::entity::array_die` and
    // `release_children_in_order`). A duty added here has to be added there as
    // well until the two entry points are one. The one duty a dying registered
    // slot has today is owed by neither: it is the free's, and every route
    // reaches the same free (`memory::stdapi::ll_free`, the withholding).

    match kind {
        OBJECT | LAZY => unsafe { ll_object_die(entity as *mut Object) },
        REFERENCE => unsafe {
            crate::reference::reference_die(entity as *mut crate::reference::LLReference)
        },
        WEAKREF => unsafe { crate::weak::weakref_die(entity as *mut crate::weak::LLWeakRef) },
        STRING | STRING_DYNAMIC => unsafe {
            crate::string::string_die(entity as *mut crate::string::LLString)
        },
        ARRAY => unsafe {
            crate::array::entity::array_die(entity as *mut crate::array::entity::LLArray)
        },
        _ => debug_assert!(
            false,
            "teardown for an entity kind the crate cannot produce yet"
        ),
    }
}

/// Tear an entity at count one down that no slot has ever named —
/// children given back, out-of-line memory returned, the cell freed.
/// Callers are the refusal branches of the operations that build an
/// entity before publishing it: the copy `element::write_through` could
/// not finish, the box `element::box_element` could not fill, the
/// destination `array::entity::separate` could not complete, and — the
/// one that hands over an entity the caller's own destination does not
/// name — the nested copy `array::entity::element_for_destination` built
/// and could not record on the work list.
///
/// [`ll_entity_die`] rather than a kind's own body, so that the teardown
/// bracket and the candidate-buffer duty this entry point carries are paid
/// here too, and it runs **unconditionally** after the count is dropped,
/// because the release verdict answers a narrower question than the caller is
/// asking: an arena entity reports no death at any count, its cell being the
/// reset's, and a refusal branch that waited for `true` left every reference
/// the replay published — an arena COW child's count, a heap child's log
/// record's +1 — held by a zero-count member until the reset. On the GC heap
/// the verdict *is* death, which is all the assertion pins: the callers differ
/// in the category they can arrive with, never in what they owe.
///
/// # Safety
/// `entity` is a live entity at count 1 that no slot has ever named.
pub(crate) unsafe fn destroy_unpublished(entity: *mut RcHeader) {
    unsafe {
        let died = crate::refcount::ll_release(entity);
        debug_assert!(
            died || header_category(entity) != MemoryCategory::GcHeap,
            "a heap entity at one dies when its only count goes"
        );
        ll_entity_die(entity);
    }
}

/// The copy-on-write write barrier: takes the pointer a holder has,
/// returns the pointer it must store before writing
/// (`rfc/model/values.md`, "Copy-on-Write Protocol"). A no-op unless
/// [`crate::refcount::cow_separation_needed`] fires, in which case the
/// entity is copied and the copy comes back.
///
/// **A separated copy comes back at +1, owned by the caller**, and the
/// store site owes the rest: store, release the creation reference, drop
/// the displaced original. [`crate::string::separate`] writes that
/// composition out with the counts (`dev/DECISIONS.md`, "the creation
/// reference is spent before the displaced original is dropped").
///
/// `owner_cat` is the **holder's** category, supplied by the compiler as
/// it is to every other store-side barrier (`memory/barrier.rs`), and
/// here it decides where the copy lives. A holder that outlives the
/// request cannot be handed an arena copy.
///
/// An FFI handle cannot perform the write-back at all, which is why a
/// borrowed `const char*` into string bytes is invalidated by any
/// mutation of that string (`rfc/model/memory/ffi.md`).
///
/// Kind-dispatched, like [`ll_entity_die`]: whether to separate is a
/// property of the header, how to copy is a property of the layout.
/// Strings and arrays are the COW kinds the crate produces.
///
/// **Null on allocation failure** — see [`crate::string::separate`].
///
/// # Safety
/// `entity` must be live; `ctx` per
/// [`crate::memory::context::ll_arena_alloc`].
pub unsafe fn ll_cow_separate(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    entity: *mut RcHeader,
) -> *mut RcHeader {
    use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind};
    let (count, flags) = unsafe { crate::refcount::header_pair(entity) };
    if !crate::refcount::cow_separation_needed(flags, count) {
        return entity;
    }

    const STRING: u32 = EntityKind::String as u32;
    const STRING_DYNAMIC: u32 = EntityKind::StringDynamic as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    match (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT {
        // Both codes, because a string is out of line by size as well as
        // by a proof of single ownership, and the first sort is
        // copy-on-write like any other; `string::separate` reads it
        // through the layout-agnostic accessor.
        STRING | STRING_DYNAMIC => unsafe {
            crate::string::separate(ctx, owner_cat, entity as *mut crate::string::LLString)
                as *mut RcHeader
        },
        // Returning the original here is what a missing arm did, and in
        // release that is a *shared* array written in place: the
        // language's value semantics broken with no signal at all.
        ARRAY => unsafe {
            let arena = crate::memory::context::resolve_arena(ctx);
            crate::array::entity::separate(
                entity as *mut crate::array::entity::LLArray,
                crate::refcount::separation_category(owner_cat),
                arena,
                crate::array::entity::CopyReason::Duplication,
            ) as *mut RcHeader
        },
        _ => {
            debug_assert!(false, "no COW copy for this entity kind yet");
            entity
        }
    }
}

/// Copy a COW entity **out of the arena** because a longer-lived holder
/// is taking it: the deep copy `rfc/model/memory/arenas.md` names for
/// value-like data, built into the store barrier
/// (`memory/barrier::store_category_barrier`).
///
/// Unconditional where [`ll_cow_separate`] is conditional — the caller
/// has already established that an arena COW entity is crossing into a
/// longer-lived slot, and the sharing test has nothing to say about it:
/// the copy is owed even when the count is 1, because the holder outlives
/// the arena and not because the value is shared.
///
/// The copy lands by [`crate::refcount::separation_category`], which for
/// every `owner_cat` this path admits is the GC heap. It arrives at `+1`,
/// which is the reference the slot takes.
///
/// **Null on allocation failure**, which the barrier turns into a refused
/// store. No context is needed: the destination is never the arena.
///
/// # Safety
/// `entity` is a live COW entity in the request arena, and `owner_cat`
/// belongs to a holder that outlives it.
pub(crate) unsafe fn escape_copy(
    arena: *mut crate::memory::arena::Arena,
    owner_cat: MemoryCategory,
    entity: *mut RcHeader,
) -> *mut RcHeader {
    use crate::refcount::{ENTITY_KIND_MASK, ENTITY_KIND_SHIFT, EntityKind};
    debug_assert_ne!(owner_cat, MemoryCategory::RequestArena);
    let flags = unsafe { crate::refcount::mutator_flags(entity) };
    const STRING: u32 = EntityKind::String as u32;
    const STRING_DYNAMIC: u32 = EntityKind::StringDynamic as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    match (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT {
        STRING | STRING_DYNAMIC => unsafe {
            crate::string::separate(
                std::ptr::null_mut(),
                owner_cat,
                entity as *mut crate::string::LLString,
            ) as *mut RcHeader
        },
        // The same body as the shallow separation, and the destination
        // category is the whole difference: with a longer-lived
        // destination over an arena source, every arena COW child is
        // copied in turn by the barrier each element is published
        // through, which is clause for clause the deep copy of
        // `rfc/model/arrays-hashtable.md`.
        ARRAY => unsafe {
            crate::array::entity::separate(
                entity as *mut crate::array::entity::LLArray,
                crate::refcount::separation_category(owner_cat),
                arena,
                crate::array::entity::CopyReason::Escape,
            ) as *mut RcHeader
        },
        _ => {
            // Null is how this function says "out of memory", and an
            // unimplemented kind is not that. There is nothing safe to
            // return, and nothing safe to continue into: the caller would
            // store a hold on arena memory into a longer-lived slot.
            unreachable!("no COW copy for this entity kind yet");
        }
    }
}

/// `instanceof`: Cohen display for classes, itable presence for
/// interfaces (`rfc/model/lowering.md`).
///
/// # Safety
/// `obj` live object, `target` linked descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_instanceof(obj: *const Object, target: *const Class) -> bool {
    let cls = unsafe { (*obj).class() };
    let target = unsafe { &*target };
    if target.flags & crate::class::CLASS_INTERFACE != 0 {
        cls.find_interface(target.interface_id).is_some()
    } else {
        cls.instance_of_class(target)
    }
}

#[cfg(test)]
mod tests;
