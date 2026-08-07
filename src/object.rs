//! Object creation and three-phase teardown
//! (`rfc/runtime/object-lifecycle.md`).
//!
//! `ll_object_new` is the out-of-line allocation path — the compiler
//! inlines the bump-pointer version when class and category are
//! statically known, but both perform the same steps. `ll_object_die`
//! is the teardown entry every strategy funnels into: it dispatches to
//! the class's `dispose` (pre-destructor with resurrection check, then
//! drop of counted children), then frees the memory by category.
//! `dispose` is a descriptor pointer — [`ll_default_dispose`] is the
//! generic stand-in a class carries until the compiler generates one
//! specialized to its layout (`dev/DECISIONS.md`, 2026-07-25).

use crate::class::{Class, NO_DESTRUCT_SLOT};
use crate::memory::context::{LLContext, resolve_arena};
use crate::refcount::{DESTRUCTOR_PENDING, DESTRUCTOR_RAN, MemoryCategory, RcHeader};
use crate::value::Value;

/// Object layout (`rfc/model/classes.md`): header, class pointer, then
/// machine-typed property slots at the offsets in `prop_layout` — a raw
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
    #[inline]
    pub fn class(&self) -> &Class {
        unsafe { &*self.class }
    }

    /// The property slot at a `prop_layout` offset.
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
        unsafe { (*class).flags } & crate::class::CLASS_TEMPLATE == 0,
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
        // store, relaxed atomic under rc-walk). Until it lands the slot
        // reads refcount 0, so a walker classifies it as free rather
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
/// (`rfc/model/memory/bulk-operations.md`): the epoch checkpoint is
/// split around the run (amendment 2026-07-28) — the ack at entry,
/// before any death, so every free the batch performs observes an
/// in-flight epoch in program order; the full pickup after the last
/// release, when the run's transients are back at their true counts
/// (the phase-lock argument — [`crate::epoch`]'s module doc). Each
/// entry is a batched release; destructors run in vector order. In an
/// rc-trace build this is plain releases behind one call boundary.
///
/// # Safety
/// Every element must point to a live heap entity beginning with
/// `RcHeader`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_release_vector(entities: *const *mut RcHeader, count: usize) {
    unsafe { crate::gc::ll_gc_checkpoint_ack() };
    for i in 0..count {
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
/// what makes reset run the pre-destructor.
///
/// **False when the record could not be written.** The caller raises
/// memory-exhausted at the creation site, and that is deliberately the
/// same outcome as a constructor that threw: our teardown runs, the user
/// destructor does not — which is exactly right, since the object never
/// finished being constructed as far as anyone can observe.
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
    if unsafe { (*obj).rc.memory_category() } == MemoryCategory::RequestArena
        && !unsafe { (*resolve_arena(ctx)).track_destructor(obj as *mut RcHeader) }
    {
        return false;
    }
    #[cfg(not(feature = "rc-walk"))]
    unsafe {
        (*obj).rc.flags |= DESTRUCTOR_PENDING
    };
    // Post-publish header write: races the collector's byte stores
    // under a live epoch, so it goes through the relaxed word helper.
    #[cfg(feature = "rc-walk")]
    unsafe {
        crate::refcount::mutator_update_flags(obj as *mut RcHeader, |f| f | DESTRUCTOR_PENDING)
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

/// Visit every live counted child of an object — the shared walk over
/// `traced_runs` for GC tracing, teardown, promotion and escape-release
/// (`rfc/model/gc/strategies.md` §4). Pointer runs (stride 8) skip a `NULL`
/// slot; Box runs (stride 16) skip a clear refcounted flag. Each child is
/// yielded once as a non-null `*mut RcHeader`; the slot lvalue is not
/// exposed, since a store goes through the barrier (which knows the slot
/// kind statically), not through here.
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
        for_each_counted_cell::<crate::walk::PlainCells>(obj as *mut u8, cls, |cell| {
            visit(cell.child)
        })
    };
}

/// The object layout's counted cells — **the one place that knows where
/// an object keeps children**, and the stride every operation over them
/// goes through: tracing, severing, the arena reset's walk, and the
/// concurrent collector alike.
///
/// It existed in three copies until 2026-08-06, and the copies cost real
/// defects rather than tidiness: when the interpolated template moved its
/// value count from the class to the instance, three walkers had to learn
/// it and the third was found by review after the fact. The difference
/// between those copies was never the stride — it was how the memory is
/// read — so that is what `R` is (`crate::walk::CellReader`).
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
/// readable — under `R = RelaxedCells` they may be concurrently written.
#[inline]
pub(crate) unsafe fn for_each_counted_cell<R: crate::walk::CellReader>(
    base: *mut u8,
    cls: *const crate::class::Class,
    mut visit: impl FnMut(crate::walk::Cell),
) {
    let flags = unsafe { (*cls).flags };

    // A template's children are counted by its shape, because one class
    // serves every interpolation site and the runs would have to differ
    // per instance (`crate::template`).
    if flags & crate::class::CLASS_TEMPLATE != 0 {
        let n = unsafe { crate::template::value_count_at::<R>(base) };
        for i in 0..n {
            let at = unsafe { base.add(crate::template::VALUES_OFFSET + i * 16) };
            // A Box payload is read as an **integer**, not as a pointer,
            // and that is `Value`'s doing rather than a shortcut here:
            // `Value::entity` stores the address as a `u64`, so the bytes
            // in the slot carry no provenance and reading them back as a
            // pointer yields one Miri rejects on first use. `entity_ptr`
            // has always recovered it by the same cast. Found by Miri.
            let child = unsafe { R::word(at) } as *mut RcHeader;
            if Value::refcounted_in_meta_word(unsafe { R::word(at.add(8)) }) {
                visit(crate::walk::Cell {
                    addr: at as usize,
                    raw: child as u64,
                    child,
                    shape: crate::walk::CellShape::Box,
                });
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
                visit(crate::walk::Cell {
                    addr: at as usize,
                    raw: child as u64,
                    child,
                    shape: crate::walk::CellShape::Pointer,
                });
            }
        }
    }
    // Box runs: 16-byte Values, empty is the refcounted flag clear.
    for run in unsafe { (*cls).box_runs() } {
        for i in 0..run.count {
            let at = unsafe { base.add((run.offset + i * 16) as usize) };
            // An integer read — see the template arm above.
            let child = unsafe { R::word(at) } as *mut RcHeader;
            if Value::refcounted_in_meta_word(unsafe { R::word(at.add(8)) }) {
                visit(crate::walk::Cell {
                    addr: at as usize,
                    raw: child as u64,
                    child,
                    shape: crate::walk::CellShape::Box,
                });
            }
        }
    }
}

/// Sever the counted slots of a region laid out by `cls`: empty each cell
/// and collect its former occupant into `displaced`, **without dropping
/// it** — the caller owes one drop per entry (`walk::sever_cells`, which
/// is the dispatch this serves).
///
/// Takes a base and a descriptor rather than an entity because a static
/// block carries no header to read a class from (A6).
///
/// Three callers — object teardown, the drain's sever, and the
/// thread-exit pass — and the third is why this takes a base and a
/// descriptor rather than an entity: a static block has no header to read
/// a class from.
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
        for_each_counted_cell::<crate::walk::PlainCells>(base, cls, |cell| {
            crate::walk::empty_cell(cell);
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
pub(crate) unsafe fn run_pre_destructor(obj: *mut Object) -> bool {
    let cls = unsafe { (*obj).class() };
    // The header flag, not the class: a class may declare `__destruct`
    // while this particular object never finished construction, and such
    // an object must not run it (`rfc/runtime/object-lifecycle.md`).
    #[cfg(not(feature = "rc-walk"))]
    {
        if unsafe { (*obj).rc.flags } & DESTRUCTOR_PENDING == 0
            || unsafe { (*obj).rc.flags } & DESTRUCTOR_RAN != 0
        {
            return false;
        }
        unsafe { (*obj).rc.flags |= DESTRUCTOR_RAN };
    }
    #[cfg(feature = "rc-walk")]
    {
        let (_, flags) = unsafe { crate::refcount::mutator_load_header(obj as *const RcHeader) };
        if flags & DESTRUCTOR_PENDING == 0 || flags & DESTRUCTOR_RAN != 0 {
            return false;
        }
        unsafe {
            crate::refcount::mutator_update_flags(obj as *mut RcHeader, |f| f | DESTRUCTOR_RAN)
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
/// (`rfc/model/classes.md`, "dispose — the internal destructor"), as a
/// function-pointer type. It runs the user `__destruct` under the
/// resurrection guard and releases the object's counted children, then
/// returns whether teardown completed — `true` to free the object,
/// `false` when a resurrection kept it alive. The object's own memory free
/// is the caller's ([`ll_object_die`]).
pub type DisposeFn = unsafe extern "C" fn(*mut Object) -> bool;

/// The default `dispose`: the generic stand-in a class carries until the
/// compiler emits one specialized to its layout (`dev/DECISIONS.md`,
/// 2026-07-25). It reads `traced_runs` (via [`for_each_counted_child`]) to
/// release children; a generated `dispose` would unroll the releases with no
/// map read, to identical effect — so a test may install its own.
///
/// Runs phases 1–2 (the resurrection-guarded `__destruct` and the child
/// releases); phase 3, the memory free, is [`ll_object_die`]'s. Returns
/// `true` to proceed to the free, `false` on resurrection.
///
/// # Safety
/// `obj` a live object whose count just reached zero (or a collector owns).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_default_dispose(obj: *mut Object) -> bool {
    // Phase 1 — pre-destructor: exactly once, resurrection-aware.
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
    #[cfg(not(feature = "rc-walk"))]
    {
        let counted = unsafe { (*obj).rc.lifetime_counted() };
        if counted {
            unsafe { (*obj).rc.refcount += 1 };
        }
        let ran = unsafe { run_pre_destructor(obj) };
        if counted {
            unsafe { (*obj).rc.refcount -= 1 };
        }
        if ran && unsafe { (*obj).rc.refcount } > 0 {
            return false; // resurrected: __destruct stored $this somewhere lasting
        }
    }
    #[cfg(feature = "rc-walk")]
    {
        // Header traffic through the relaxed word helpers: the walker
        // reads this header concurrently, and the guard's transient
        // `rc 0 → 1 → 0` is visible to it (a phantom row at worst —
        // repaired by Phases 3–4). No candidate buffer exists to leave.
        let (_, flags) = unsafe { crate::refcount::mutator_load_header(obj as *const RcHeader) };
        let counted = MemoryCategory::from_flags(flags) == MemoryCategory::GcHeap;
        if counted {
            unsafe { crate::refcount::mutator_guard_retain(obj as *mut RcHeader) };
        }
        let ran = unsafe { run_pre_destructor(obj) };
        if counted {
            // Eager death (2026-07-27): a condemnation landing while
            // the destructor ran changes nothing — teardown always
            // finishes, and the component's drain drops on the corpse.
            let refcount =
                unsafe { crate::refcount::mutator_unguard_release(obj as *mut RcHeader) };
            if ran && refcount > 0 {
                return false; // resurrected
            }
        } else if ran && unsafe { (*obj).rc.refcount } > 0 {
            return false;
        }
    }

    // Phase 2, first act — weak notification, BEFORE any child release:
    // the drops below cascade into user code, and a child's `__destruct`
    // calling `get()` on this refcount-zero object would receive a strong
    // reference that outlives the free (`rfc/model/weak-references.md`;
    // regression: `weak::tests::own_destructor_still_sees_the_object_...`).
    // Read the flags fresh: the `__destruct` above may itself have created
    // the weak state.
    if unsafe { crate::refcount::header_flags(obj as *const RcHeader) }
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
    let owner_cat = unsafe { (*obj).rc.memory_category() };
    unsafe {
        for_each_counted_child(obj, |child| {
            crate::memory::barrier::drop_ref(owner_cat, child);
        });
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
    // Teardown bracket (rc-walk): no epoch-message pickup between the
    // death's committing store and this dispose's completion — a drain
    // destructor's `WeakRef::get` could reach the committed-dead
    // entity (review finding 2026-07-27, `rfc/model/gc/rc-walk.md`).
    // The exit of the outermost bracket runs the full checkpoint.
    #[cfg(feature = "rc-walk")]
    crate::epoch::teardown_enter();
    // Teardown bracket (rc-trace): while it is held no fire point
    // collects, so the user code `dispose` runs cannot reach a
    // collection that would judge this refcount-zero object garbage
    // (`gc::teardown_enter`).
    #[cfg(not(feature = "rc-walk"))]
    crate::gc::teardown_enter();

    let dispose: DisposeFn = unsafe { std::mem::transmute((*(*obj).class).dispose) };
    if unsafe { dispose(obj) } {
        // Leave the candidate buffer before the free, or the buffer
        // keeps a root pointing at memory about to be reused. It has to
        // be *here* rather than before `dispose`, because `__destruct`
        // can buffer the object afresh: a transient `$this` taken inside
        // it is a retain and a release, and that release is a non-zero
        // decrement. `dispose` cannot own the duty — it is class code,
        // and the compiler emits one per class.
        #[cfg(not(feature = "rc-walk"))]
        unsafe {
            crate::gc::forget_candidate(obj as *mut RcHeader)
        };

        // Phase 3 — memory, by category. Arenas reclaim at reset; the
        // long-lived policy is TBD; only the GC heap frees here.
        if unsafe { header_category(obj as *const RcHeader) } == MemoryCategory::GcHeap {
            unsafe { crate::memory::stdapi::ll_free(obj as *mut u8) };
        }
    } // else resurrected: kept alive, not freed

    #[cfg(feature = "rc-walk")]
    crate::epoch::teardown_exit();
    #[cfg(not(feature = "rc-walk"))]
    crate::gc::teardown_exit();
}

/// The category of a possibly-walked header: a relaxed read under
/// `rc-walk` (the collector's byte stores race every plain header
/// access during an epoch), a plain read otherwise.
#[inline]
pub(crate) unsafe fn header_category(header: *const RcHeader) -> MemoryCategory {
    MemoryCategory::from_flags(unsafe { crate::refcount::header_flags(header) })
}

/// Teardown for a **bare entity pointer**: the kind field selects the
/// free routine (`rfc/model/classes.md`, "Entity kind and non-object
/// teardown") — one flags load and a small switch. Object and lazy
/// carry a class pointer and dispatch through its `dispose`; a
/// reference box releases its one Value and frees; a weak cell
/// unregisters from the weak table; a string and an array free the
/// body they own outside their own slot. Box gains its arm when the
/// crate can produce one (FFI), and reaching it today is a bug, not a
/// leak policy; that arm owes the bit-7 weak-notify test — a Box is a
/// legal `WeakReference` target (`rfc/model/weak-references.md`,
/// "every entity kind honours bit 7").
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
    const ARRAY: u32 = EntityKind::Array as u32;
    let flags = unsafe { crate::refcount::header_flags(entity) };
    let kind = (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT;
    // The bare-pointer door leaves the candidate buffer for every kind
    // the gate admits, so a kind that gains counted slots later inherits
    // it without a call site of its own (`refcount::CANDIDATE_KINDS`).
    // An array reaches teardown here and nowhere else — it runs no
    // `dispose`, which is where an object does this on its way past the
    // free. The bit is tested from flags already in a register.
    #[cfg(not(feature = "rc-walk"))]
    if flags & crate::refcount::CYCLE_COLLECTOR_BUFFERED != 0 {
        unsafe { crate::gc::forget_candidate(entity) };
    }
    // Teardown bracket (rc-walk) — see `ll_object_die`; nesting is
    // fine, the depth is a counter and only the outermost exit picks
    // up messages.
    #[cfg(feature = "rc-walk")]
    crate::epoch::teardown_enter();
    #[cfg(not(feature = "rc-walk"))]
    crate::gc::teardown_enter();
    match kind {
        OBJECT | LAZY => unsafe { ll_object_die(entity as *mut Object) },
        REFERENCE => unsafe {
            crate::reference::reference_die(entity as *mut crate::reference::LLReference)
        },
        WEAKREF => unsafe { crate::weak::weakref_die(entity as *mut crate::weak::LLWeakRef) },
        STRING => unsafe { crate::string::string_die(entity as *mut crate::string::LLString) },
        ARRAY => unsafe {
            crate::array::entity::array_die(entity as *mut crate::array::entity::LLArray)
        },
        _ => debug_assert!(
            false,
            "teardown for an entity kind the crate cannot produce yet"
        ),
    }
    #[cfg(feature = "rc-walk")]
    crate::epoch::teardown_exit();
    #[cfg(not(feature = "rc-walk"))]
    crate::gc::teardown_exit();
}

/// The copy-on-write write barrier: takes the pointer a holder has,
/// returns the pointer it must store before writing
/// (`rfc/model/values.md`, "Copy-on-Write Protocol"). A no-op unless
/// [`crate::refcount::cow_separation_needed`] fires, in which case the
/// entity is copied and the copy comes back.
///
/// **A separated copy comes back at +1, owned by the caller**, and the
/// full composition — store, drop the displaced original, release the
/// creation reference — is written out in [`crate::string::separate`].
/// Getting it wrong does not merely leak: a copy left at two reads as
/// shared on every later write and separates forever.
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
/// Strings are the only COW entity the crate produces so far; arrays
/// join with Phase C.
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
    let flags = unsafe { crate::refcount::header_flags(entity) };
    let count = unsafe { crate::refcount::header_refcount(entity) };
    if !crate::refcount::cow_separation_needed(flags, count) {
        return entity;
    }
    const STRING: u32 = EntityKind::String as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    match (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT {
        STRING => unsafe {
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
    let flags = unsafe { crate::refcount::header_flags(entity) };
    const STRING: u32 = EntityKind::String as u32;
    const ARRAY: u32 = EntityKind::Array as u32;
    match (flags & ENTITY_KIND_MASK) >> ENTITY_KIND_SHIFT {
        STRING => unsafe {
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
mod tests {
    use super::*;
    use crate::class::ClassBuilder;
    use crate::memory::arena::Arena;
    use crate::refcount::{ll_release, ll_retain};
    use crate::value::Tag;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    static RESURRECT_INTO: AtomicUsize = AtomicUsize::new(0);
    static TRANSIENT_DEATHS: AtomicUsize = AtomicUsize::new(0);
    static DISPOSE_DISPATCHED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    }

    /// A stand-in for a compiler-generated specialized `dispose`: it marks
    /// that the descriptor's pointer was dispatched to, then delegates the
    /// real teardown to the default so the effects are unchanged.
    unsafe extern "C" fn counting_dispose(obj: *mut Object) -> bool {
        DISPOSE_DISPATCHED.fetch_add(1, Ordering::Relaxed);
        unsafe { ll_default_dispose(obj) }
    }

    unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
        unsafe { ll_retain(obj as *mut RcHeader) };
        RESURRECT_INTO.store(obj as usize, Ordering::Relaxed);
    }

    /// `$x = $this;` then `$x` leaves scope: a transient retain + release.
    /// Under the destructor guard the release must NOT report death — a
    /// reported death here re-enters teardown and double-frees `obj`.
    unsafe extern "C" fn transient_this_destructor(obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
        unsafe { ll_retain(obj as *mut RcHeader) };
        if unsafe { ll_release(obj as *mut RcHeader) } {
            TRANSIENT_DEATHS.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn with_ctx<R>(f: impl FnOnce(*mut LLContext) -> R) -> R {
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let r = f(&mut ctx);
        arena.reset(|_| {});
        r
    }

    #[test]
    fn new_stamps_header_class_and_null_props() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Plain").prop("x", true).build();

        with_ctx(|ctx| {
            let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::RequestArena) };
            let o = unsafe { &mut *obj };
            assert_eq!(o.rc.refcount, 1);
            assert_eq!(o.rc.memory_category(), MemoryCategory::RequestArena);
            assert_eq!(o.class, cls);
            assert_eq!(o.rc.flags & DESTRUCTOR_PENDING, 0, "no destructor declared");
            let x = unsafe { Object::prop_at(obj, 16).read() };
            assert_eq!(x.tag(), Tag::Null);
        });
    }

    /// A5: a defaultless `mixed` Box slot starts *undefined* — the factory
    /// stamps `VALUE_UNDEF` from the descriptor's undef runs after the
    /// zero-fill — while a defaulted one starts `null`. Undef is invisible
    /// to the trace walk (the refcounted flag is clear), any store clears
    /// it (the barrier writes all 16 bytes), and `unset()` is the
    /// undef-store + `drop_ref` composition, which restores the state and
    /// releases the displaced entity.
    #[test]
    fn defaultless_box_slot_lives_the_undef_lifecycle() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("A5Undef")
            .prop("defaulted", true) // Boxed with a default: starts null
            .prop_boxed_without_default("bare") // starts undef
            .build();
        let child_cls = ClassBuilder::new("A5Child").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        unsafe {
            let obj = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
            let defaulted = Object::prop_at(obj, 16);
            let bare = Object::prop_at(obj, 32);

            assert!(
                !defaulted.read().is_undef(),
                "a default means never tracked"
            );
            assert_eq!(defaulted.read().tag(), Tag::Null);
            assert!(bare.read().is_undef(), "stamped by the factory");

            // Undef is not traced: the walk sees no children yet.
            let mut children = 0;
            for_each_counted_child(obj, |_| children += 1);
            assert_eq!(children, 0, "an undef slot must not be walked");

            // Any store clears undef — the whole 16 bytes are written.
            let child = new_constructed(&mut ctx, child_cls, MemoryCategory::GcHeap);
            assert!(crate::memory::barrier::ref_store(
                &mut arena,
                obj as *mut RcHeader,
                bare,
                std::ptr::null_mut(),
                Value::entity(crate::value::Tag::Object, child as *mut RcHeader),
            ));
            assert!(!bare.read().is_undef());
            assert_eq!((*child).rc.refcount, 2, "creation + the slot");
            let mut children = 0;
            for_each_counted_child(obj, |_| children += 1);
            assert_eq!(children, 1, "a stored entity is walked again");

            // `unset($obj->bare)`: store undef back, drop the displaced
            // entity — the same publish-then-release order as any
            // overwriting store.
            assert!(crate::memory::barrier::ref_store(
                &mut arena,
                obj as *mut RcHeader,
                bare,
                child as *mut RcHeader,
                Value::undef(),
            ));
            assert!(bare.read().is_undef(), "unset returns the slot to undef");
            assert_eq!((*child).rc.refcount, 1, "the slot's reference released");
            let mut children = 0;
            for_each_counted_child(obj, |_| children += 1);
            assert_eq!(children, 0);

            // Teardown strides the same runs: the undef slot releases
            // nothing, and both die cleanly.
            for entity in [child as *mut RcHeader, obj as *mut RcHeader] {
                assert!(crate::refcount::ll_release(entity));
                ll_entity_die(entity);
            }
        }
        arena.reset(|_| {});
    }

    /// A5 commit 2: raw slots with no marker of their own — a defaultless
    /// `?T` pointer (`NULL` is PHP null there) and a defaultless scalar —
    /// are tracked by the init bitmap in the byte block. The factory's
    /// zero-fill starts every bit clear (uninitialized); a write sets the
    /// bit beside the value store; `unset()` clears it, for the pointer
    /// slot together with the NULL store + drop of the displaced entity.
    #[test]
    fn bitmap_tracked_raw_slots_live_the_init_lifecycle() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("A5Bitmap")
            .prop_nullable_pointer_without_default("p") // @16, run member
            .prop_scalar_without_default("n") // @24; block byte 32
            .build();
        let child_cls = ClassBuilder::new("A5BitmapChild").build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        unsafe {
            let obj = new_constructed(&mut ctx, cls, MemoryCategory::GcHeap);
            let p_bit = (*cls)
                .find_prop(crate::intern::intern_str("p"))
                .unwrap()
                .init_bit;
            let n_bit = (*cls)
                .find_prop(crate::intern::intern_str("n"))
                .unwrap()
                .init_bit;

            // The zero-fill made both uninitialized, no explicit store.
            assert!(!Object::init_bit_test(obj, p_bit));
            assert!(!Object::init_bit_test(obj, n_bit));

            // $obj->n = 42: the value store plus the bit set.
            let n_slot = (obj as *mut u8).add(24) as *mut i64;
            n_slot.write(42);
            Object::init_bit_set(obj, n_bit);
            assert!(Object::init_bit_test(obj, n_bit));
            assert!(!Object::init_bit_test(obj, p_bit), "bits are independent");

            // $obj->p = $child: the barrier's pointer store + the bit set.
            let child = new_constructed(&mut ctx, child_cls, MemoryCategory::GcHeap);
            let p_slot = (obj as *mut u8).add(16) as *mut *mut RcHeader;
            assert!(crate::memory::barrier::store_ptr(
                &mut arena,
                MemoryCategory::GcHeap,
                p_slot,
                child as *mut RcHeader,
            ));
            Object::init_bit_set(obj, p_bit);
            assert_eq!((*child).rc.refcount, 2, "creation + the slot");

            // A walked child now; the bitmap never affects the trace.
            let mut children = 0;
            for_each_counted_child(obj, |_| children += 1);
            assert_eq!(children, 1);

            // $obj->p = null: a real null for `?T` — the slot goes back to
            // NULL, the displaced child is dropped, and the bit STAYS set:
            // the bit, not the pointer, answers isset.
            assert!(crate::memory::barrier::store_ptr(
                &mut arena,
                MemoryCategory::GcHeap,
                p_slot,
                std::ptr::null_mut(),
            ));
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, child as *mut RcHeader);
            assert!(
                Object::init_bit_test(obj, p_bit),
                "null is a value, still initialized"
            );
            assert_eq!((*child).rc.refcount, 1, "the slot's reference released");

            // unset($obj->p) / unset($obj->n): back to uninitialized. The
            // pointer slot is already NULL; a raw slot has only the bit.
            Object::init_bit_clear(obj, p_bit);
            Object::init_bit_clear(obj, n_bit);
            assert!(!Object::init_bit_test(obj, p_bit));
            assert!(!Object::init_bit_test(obj, n_bit));

            for entity in [child as *mut RcHeader, obj as *mut RcHeader] {
                assert!(crate::refcount::ll_release(entity));
                ll_entity_die(entity);
            }
        }
        arena.reset(|_| {});
    }

    /// The construct-into-a-reserved-cell path shares `stamp_into`, so it
    /// stamps undef the same way the allocating factory does.
    #[test]
    fn object_new_in_a_reserved_cell_stamps_undef_too() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("A5UndefInCell")
            .prop_boxed_without_default("bare")
            .build();

        unsafe {
            let mut cell: *mut u8 = std::ptr::null_mut();
            let mut contiguous = 0usize;
            let got = crate::memory::heap::ll_entity_reserve(
                (*cls).object_size as usize,
                1,
                &mut cell,
                &mut contiguous,
            );
            assert_eq!(got, 1, "one cell for the test object");
            let obj = ll_object_new_in(cell, cls);
            assert!(Object::prop_at(obj, 16).read().is_undef());
            assert!(crate::refcount::ll_release(obj as *mut RcHeader));
            ll_entity_die(obj as *mut RcHeader);
        }
    }

    #[test]
    fn arena_object_with_destructor_is_tracked_and_reset_delivers_it() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("WithDtor")
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { new_constructed(&mut ctx, cls, MemoryCategory::RequestArena) };
        assert_ne!(unsafe { (*obj).rc.flags } & DESTRUCTOR_PENDING, 0);

        let mut delivered = Vec::new();
        arena.reset(|o| delivered.push(o));
        assert_eq!(delivered, vec![obj as *mut RcHeader]);
    }

    /// The factory does not owe a `__destruct`; the completed user
    /// constructor does. An object that never got past the factory —
    /// because `__construct` threw, or because registering the record was
    /// refused — must not appear in the arena's destructor log and must
    /// not run its `__destruct` on teardown.
    #[test]
    fn an_unconstructed_object_owes_no_destructor() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("ThrewInCtor")
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        // The factory alone: no `object_constructed` call, as for a
        // constructor that raised.
        let obj = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };
        assert_eq!(unsafe { (*obj).rc.flags } & DESTRUCTOR_PENDING, 0);

        let mut delivered = Vec::new();
        arena.reset(|o| delivered.push(o));
        assert!(delivered.is_empty(), "nothing was registered");
        assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 0, "and nothing ran");

        // Same rule on the refcounted path, where teardown dispatches on
        // the header rather than on a log: a heap object that never
        // completed construction dies without its `__destruct`.
        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let heap_obj = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::GcHeap) };
        // Through the count first, as generated code would: `ll_release`
        // reports the death, and the caller performs the teardown.
        assert!(unsafe { crate::refcount::ll_release(heap_obj as *mut RcHeader) });
        unsafe { ll_object_die(heap_obj) };
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            0,
            "teardown must dispatch on the object's own flag, not on the class"
        );
        arena.reset(|_| {});
    }

    #[test]
    fn die_runs_three_phases_and_cascades_to_children() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);

        let child_cls = ClassBuilder::new("Child")
            .destructor(counting_destructor as *const ())
            .build();
        let parent_cls = ClassBuilder::new("Parent")
            .prop("child", true)
            .destructor(counting_destructor as *const ())
            .build();

        with_ctx(|ctx| {
            let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
            let parent = unsafe { new_constructed(ctx, parent_cls, MemoryCategory::GcHeap) };
            unsafe {
                Object::prop_at(parent, 16)
                    .write(Value::entity(Tag::Object, child as *mut RcHeader));
            }
            // The slot owns the child's initial reference: count stays 1.

            // Parent's last reference dies.
            assert!(unsafe { ll_release(parent as *mut RcHeader) });
            unsafe { ll_object_die(parent) };

            assert_eq!(
                DESTRUCTS.load(Ordering::Relaxed),
                2,
                "parent and child pre-destructors both ran"
            );
        });
    }

    /// The same cascade, but through a **bare-pointer** slot (`prop_pointer`)
    /// rather than a Box — this is what exercises `for_each_counted_child`'s
    /// pointer-run branch (stride 8, skip `NULL`). Without it the child's
    /// release never happens and its destructor does not run.
    #[test]
    fn teardown_cascades_through_a_bare_pointer_slot() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);

        let child_cls = ClassBuilder::new("PtrChild")
            .destructor(counting_destructor as *const ())
            .build();
        let parent_cls = ClassBuilder::new("PtrParent")
            .prop_pointer("child")
            .destructor(counting_destructor as *const ())
            .build();

        with_ctx(|ctx| {
            let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
            let parent = unsafe { new_constructed(ctx, parent_cls, MemoryCategory::GcHeap) };
            // Store a class-typed reference into the 8-byte pointer slot at
            // +16; the slot takes over the child's initial reference (count
            // stays 1), as the Box cascade above does. The store barrier's
            // pointer form is A4 — here the raw write models generated code.
            unsafe {
                let slot = (parent as *mut u8).add(16) as *mut *mut RcHeader;
                slot.write(child as *mut RcHeader);
            }

            assert!(unsafe { ll_release(parent as *mut RcHeader) });
            unsafe { ll_object_die(parent) };

            assert_eq!(
                DESTRUCTS.load(Ordering::Relaxed),
                2,
                "parent and its pointer-slot child both destructed"
            );
        });
    }

    /// Teardown dispatches through the class's `dispose` pointer, not a
    /// hardcoded path: a class carrying a custom `dispose` sees it invoked,
    /// and the real teardown still runs (here via delegation). This is the
    /// hook A3 opens for the compiler's specialized `dispose`.
    #[test]
    fn teardown_dispatches_through_the_class_dispose_pointer() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        DISPOSE_DISPATCHED.store(0, Ordering::Relaxed);

        let child_cls = ClassBuilder::new("DispChild")
            .destructor(counting_destructor as *const ())
            .build();
        let parent_cls = ClassBuilder::new("DispParent")
            .prop_pointer("child")
            .destructor(counting_destructor as *const ())
            .dispose(counting_dispose as *const ())
            .build();

        with_ctx(|ctx| {
            let child = unsafe { new_constructed(ctx, child_cls, MemoryCategory::GcHeap) };
            let parent = unsafe { new_constructed(ctx, parent_cls, MemoryCategory::GcHeap) };
            unsafe {
                let slot = (parent as *mut u8).add(16) as *mut *mut RcHeader;
                slot.write(child as *mut RcHeader);
            }

            assert!(unsafe { ll_release(parent as *mut RcHeader) });
            unsafe { ll_object_die(parent) };

            assert_eq!(
                DISPOSE_DISPATCHED.load(Ordering::Relaxed),
                1,
                "teardown went through the descriptor's dispose (the parent's only)"
            );
            assert_eq!(
                DESTRUCTS.load(Ordering::Relaxed),
                2,
                "parent + child still destructed via the custom dispose"
            );
        });
    }

    #[test]
    fn resurrection_aborts_teardown_and_destructor_never_reruns() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);

        let cls = ClassBuilder::new("Lazarus")
            .destructor(resurrecting_destructor as *const ())
            .build();

        with_ctx(|ctx| {
            let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };

            assert!(unsafe { ll_release(obj as *mut RcHeader) });
            unsafe { ll_object_die(obj) };
            assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1);
            assert_eq!(
                unsafe { (*obj).rc.refcount },
                1,
                "resurrected: the destructor's reference keeps it alive"
            );

            // The resurrection reference dies too. Phase 1 is skipped
            // (DESTRUCTOR_RAN bit), phases 2-3 proceed.
            assert!(unsafe { ll_release(obj as *mut RcHeader) });
            unsafe { ll_object_die(obj) };
            assert_eq!(
                DESTRUCTS.load(Ordering::Relaxed),
                1,
                "__destruct runs exactly once per object"
            );
        });
    }

    #[test]
    fn transient_this_reference_in_destructor_does_not_reenter_teardown() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);
        TRANSIENT_DEATHS.store(0, Ordering::Relaxed);

        let cls = ClassBuilder::new("Fleeting")
            .destructor(transient_this_destructor as *const ())
            .build();

        with_ctx(|ctx| {
            let obj = unsafe { new_constructed(ctx, cls, MemoryCategory::GcHeap) };

            // Last reference dies; teardown runs the destructor, which takes
            // and drops a transient $this reference.
            assert!(unsafe { ll_release(obj as *mut RcHeader) });
            unsafe { ll_object_die(obj) };

            assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1, "destructor ran once");
            assert_eq!(
                TRANSIENT_DEATHS.load(Ordering::Relaxed),
                0,
                "a transient $this release must not report death: without the \
                 guard it re-enters teardown and double-frees obj"
            );
        });
    }

    #[test]
    fn abi_object_new_takes_the_category_as_u32() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("Plain").build();
        with_ctx(|ctx| {
            // As generated code passes it: the category is a raw u32.
            let obj = unsafe { ll_object_new_abi(ctx, cls, MemoryCategory::RequestArena as u32) };
            assert_eq!(
                unsafe { (*obj).rc.memory_category() },
                MemoryCategory::RequestArena
            );
        });
    }

    #[test]
    fn instanceof_covers_classes_and_interfaces() {
        let _g = crate::memory::block_pool::test_guard();
        extern "C" fn noop() {}

        let interface = ClassBuilder::interface("Speaks");
        let animal = ClassBuilder::new("Animal")
            .method("speak", noop as *const ())
            .implement(unsafe { &*interface }, vec![0])
            .build();
        let dog = ClassBuilder::new("Dog").parent(animal).build();
        let rock = ClassBuilder::new("Rock").build();

        with_ctx(|ctx| {
            let d = unsafe { new_constructed(ctx, dog, MemoryCategory::RequestArena) };
            let r = unsafe { new_constructed(ctx, rock, MemoryCategory::RequestArena) };
            unsafe {
                assert!(ll_instanceof(d, animal));
                assert!(ll_instanceof(d, dog));
                assert!(
                    ll_instanceof(d, interface),
                    "interface via inherited itable"
                );
                assert!(!ll_instanceof(r, animal));
                assert!(!ll_instanceof(r, interface));
            }
        });
    }
}
