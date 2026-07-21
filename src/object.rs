//! Object creation and three-phase teardown
//! (`rfc/runtime/object-lifecycle.md`).
//!
//! `ll_object_new` is the out-of-line allocation path — the compiler
//! inlines the bump-pointer version when class and category are
//! statically known, but both perform the same steps. `ll_object_die`
//! is the teardown entry every strategy funnels into: pre-destructor
//! with resurrection check, drop of counted children, memory release
//! by category.

use crate::class::{Class, NO_DESTRUCT_SLOT};
use crate::memory::context::{LLContext, resolve_arena};
use crate::memory::immortal::immortal_alloc;
use crate::refcount::{DESTRUCTED, HAS_DESTRUCTOR, MemoryCategory, RcHeader};
use crate::value::{Tag, Value, value_release};

/// Object layout (`rfc/model/classes.md`): header, class pointer,
/// then fixed 16-byte property slots at the offsets in `prop_layout`.
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

    let mem = match category {
        MemoryCategory::RequestArena => unsafe { (*resolve_arena(ctx)).alloc(size) },
        // Stand-in until the GC strategy owns the GcHeap allocator and
        // the long-lived arena gets its reclamation policy: both route
        // through the standard path (small → thread heap).
        MemoryCategory::GcHeap | MemoryCategory::LongLived => unsafe {
            crate::memory::stdapi::ll_alloc(size, 16)
        },
        MemoryCategory::Immortal => immortal_alloc(size),
    };
    // Out of memory. The caller raises; nothing here is half-built,
    // because nothing was built (`rfc/runtime/exceptions.md`: the Rust
    // core reports, a Limelight frame raises).
    if mem.is_null() {
        return std::ptr::null_mut();
    }
    let obj = mem as *mut Object;

    let extra = crate::refcount::ENTITY_OBJECT
        | if cls.has_destructor() {
            HAS_DESTRUCTOR
        } else {
            0
        };
    unsafe {
        (*obj).rc = RcHeader::new(category, extra);
        (*obj).class = class;
        // Defaults / UNINIT discriminants are a compiler contract
        // (definite assignment, init bitmap); the runtime default is
        // null in every Box slot.
        for slot in cls.props() {
            Object::prop_at(obj, slot.offset).write(Value::null());
        }
    }

    // Arena objects with a PHP destructor must be tracked: reset runs
    // their pre-destructors first (rfc/model/memory/arenas.md).
    if category == MemoryCategory::RequestArena && cls.has_destructor() {
        unsafe { (*resolve_arena(ctx)).track_destructor(obj as *mut RcHeader) };
    }
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

/// The entity-holding Values currently sitting in an object's
/// refcounted property slots — the children a tracer or teardown
/// walks. Consumes `prop_layout.refcounted_slots()`, the metadata
/// contract of `rfc/model/gc/strategies.md` §4.
pub(crate) unsafe fn ref_child_values(obj: *mut Object) -> Vec<Value> {
    let cls = unsafe { (*obj).class() };
    cls.refcounted_slots()
        .map(|offset| unsafe { Object::prop_at(obj, offset).read() })
        .filter(|v| v.is_refcounted())
        .collect()
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
    if !cls.has_destructor() || unsafe { (*obj).rc.flags } & DESTRUCTED != 0 {
        return false;
    }
    unsafe { (*obj).rc.flags |= DESTRUCTED };
    debug_assert_ne!(cls.destruct_slot, NO_DESTRUCT_SLOT);
    // Through the raw class pointer, not `cls`: the vtable trails the
    // descriptor's fixed fields, which a `&Class` does not cover.
    let code = unsafe { Class::vtbl((*obj).class) }[cls.destruct_slot as usize];
    let destruct: DestructorFn = unsafe { std::mem::transmute(code) };
    unsafe { destruct(obj) };
    true
}

/// Three-phase teardown. Called when the refcount reaches zero or a
/// collector proves the object garbage.
///
/// # Safety
/// `obj` must be a live object whose count just reached zero (or that
/// a collector owns).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_object_die(obj: *mut Object) {
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
    let counted = unsafe { (*obj).rc.lifetime_counted() };
    if counted {
        unsafe { (*obj).rc.refcount += 1 };
    }
    let ran = unsafe { run_pre_destructor(obj) };
    if counted {
        unsafe { (*obj).rc.refcount -= 1 };
    }
    if ran && unsafe { (*obj).rc.refcount } > 0 {
        return; // resurrected: __destruct stored $this somewhere lasting
    }
    let cls = unsafe { (*obj).class() };

    // Leave the cycle-collector candidate buffer before releasing any
    // child. This object's refcount is already 0; a child release below can
    // trip the candidate threshold and run a synchronous collection, which
    // would otherwise trace this still-buffered object as a root and free it
    // — then phase 3 frees it again (double free). No-op for entities that
    // were never buffered (non-GcHeap, or GcHeap that never decremented).
    unsafe { crate::gc::forget_candidate(obj as *mut RcHeader) };

    // Phase 2 — drop: release counted children, cascading.
    for offset in cls.refcounted_slots() {
        let v = unsafe { Object::prop_at(obj, offset).read() };

        // This longer-lived holder is being torn down: for each
        // request-arena escapee it held, drop the escape hold-count
        // (`lose`, `rfc/model/memory/arenas.md`). Same event as an
        // overwrite in the store barrier — the slot let go of the escapee.
        // Heap children fall through to the normal release + cascade below.
        if v.is_refcounted()
            && unsafe { (*v.entity_ptr()).memory_category() } == MemoryCategory::RequestArena
        {
            unsafe { crate::memory::barrier::escape_lose(v.entity_ptr()) };
        }

        if unsafe { value_release(&v) } {
            // TODO(strings/arrays): entity teardown for non-object
            // children when those entities exist.
            if v.tag() == Tag::Object {
                unsafe { ll_object_die(v.entity_ptr() as *mut Object) };
            }
        }
    }

    // Phase 3 — memory, by category. Arenas reclaim at reset; the
    // long-lived policy is TBD; only the GC heap frees here. The
    // deferred-free GC activity bit arrives with rc-satb.
    if unsafe { (*obj).rc.memory_category() } == MemoryCategory::GcHeap {
        // The candidate buffer was already cleared above, before phase 2.
        unsafe { crate::memory::stdapi::ll_free(obj as *mut u8) };
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
        cls.find_iface(target.iface_id).is_some()
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
    static RESURRECT_INTO: AtomicUsize = AtomicUsize::new(0);
    static TRANSIENT_DEATHS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
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
            let obj = unsafe { ll_object_new(ctx, cls, MemoryCategory::RequestArena) };
            let o = unsafe { &mut *obj };
            assert_eq!(o.rc.refcount, 1);
            assert_eq!(o.rc.memory_category(), MemoryCategory::RequestArena);
            assert_eq!(o.class, cls);
            assert_eq!(o.rc.flags & HAS_DESTRUCTOR, 0, "no destructor declared");
            let x = unsafe { Object::prop_at(obj, 16).read() };
            assert_eq!(x.tag(), Tag::Null);
        });
    }

    #[test]
    fn arena_object_with_destructor_is_tracked_and_reset_delivers_it() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("WithDtor")
            .destructor(counting_destructor as *const ())
            .build();

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let obj = unsafe { ll_object_new(&mut ctx, cls, MemoryCategory::RequestArena) };
        assert_ne!(unsafe { (*obj).rc.flags } & HAS_DESTRUCTOR, 0);

        let mut delivered = Vec::new();
        arena.reset(|o| delivered.push(o));
        assert_eq!(delivered, vec![obj as *mut RcHeader]);
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
            let child = unsafe { ll_object_new(ctx, child_cls, MemoryCategory::GcHeap) };
            let parent = unsafe { ll_object_new(ctx, parent_cls, MemoryCategory::GcHeap) };
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

    #[test]
    fn resurrection_aborts_teardown_and_destructor_never_reruns() {
        let _g = crate::memory::block_pool::test_guard();
        DESTRUCTS.store(0, Ordering::Relaxed);

        let cls = ClassBuilder::new("Lazarus")
            .destructor(resurrecting_destructor as *const ())
            .build();

        with_ctx(|ctx| {
            let obj = unsafe { ll_object_new(ctx, cls, MemoryCategory::GcHeap) };

            assert!(unsafe { ll_release(obj as *mut RcHeader) });
            unsafe { ll_object_die(obj) };
            assert_eq!(DESTRUCTS.load(Ordering::Relaxed), 1);
            assert_eq!(
                unsafe { (*obj).rc.refcount },
                1,
                "resurrected: the destructor's reference keeps it alive"
            );

            // The resurrection reference dies too. Phase 1 is skipped
            // (DESTRUCTED bit), phases 2-3 proceed.
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
            let obj = unsafe { ll_object_new(ctx, cls, MemoryCategory::GcHeap) };

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

        let iface = ClassBuilder::interface("Speaks");
        let animal = ClassBuilder::new("Animal")
            .method("speak", noop as *const ())
            .implement(unsafe { &*iface }, vec![0])
            .build();
        let dog = ClassBuilder::new("Dog").parent(animal).build();
        let rock = ClassBuilder::new("Rock").build();

        with_ctx(|ctx| {
            let d = unsafe { ll_object_new(ctx, dog, MemoryCategory::RequestArena) };
            let r = unsafe { ll_object_new(ctx, rock, MemoryCategory::RequestArena) };
            unsafe {
                assert!(ll_instanceof(d, animal));
                assert!(ll_instanceof(d, dog));
                assert!(ll_instanceof(d, iface), "interface via inherited itable");
                assert!(!ll_instanceof(r, animal));
                assert!(!ll_instanceof(r, iface));
            }
        });
    }
}
