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
    /// # Safety
    /// `offset` must come from this object's class layout.
    #[inline]
    pub unsafe fn prop_at(&mut self, offset: u32) -> *mut Value {
        unsafe { (self as *mut Object as *mut u8).add(offset as usize) as *mut Value }
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
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`]; `class` must
/// be a linked descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_object_new(
    ctx: *mut LLContext,
    class: *const Class,
    category: MemoryCategory,
) -> *mut Object {
    let cls = unsafe { &*class };
    let size = cls.object_size as usize;

    let mem = match category {
        MemoryCategory::RequestArena => resolve_arena(ctx).alloc(size),
        // Stand-in until the GC strategy owns the GcHeap allocator and
        // the long-lived arena gets its reclamation policy: both route
        // through the standard path (small → thread heap).
        MemoryCategory::GcHeap | MemoryCategory::LongLived => unsafe {
            crate::memory::stdapi::ll_alloc(size, 16)
        },
        MemoryCategory::Immortal => immortal_alloc(size),
    };
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
            (*obj).prop_at(slot.offset).write(Value::null());
        }
    }

    // Arena objects with a PHP destructor must be tracked: reset runs
    // their pre-destructors first (rfc/model/memory/arenas.md).
    if category == MemoryCategory::RequestArena && cls.has_destructor() {
        resolve_arena(ctx).track_destructor(obj as *mut RcHeader);
    }
    obj
}

/// The entity-holding Values currently sitting in an object's
/// refcounted property slots — the children a tracer or teardown
/// walks. Consumes `prop_layout.refcounted_slots()`, the metadata
/// contract of `rfc/model/gc/strategies.md` §4.
pub(crate) unsafe fn ref_child_values(obj: *mut Object) -> Vec<Value> {
    let cls = unsafe { (*obj).class() };
    cls.refcounted_slots()
        .map(|offset| unsafe { (*obj).prop_at(offset).read() })
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
    let code = cls.vtbl()[cls.destruct_slot as usize];
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
    if unsafe { run_pre_destructor(obj) } && unsafe { (*obj).rc.refcount } > 0 {
        return; // resurrected: __destruct stored $this somewhere
    }
    let cls = unsafe { (*obj).class() };

    // Phase 2 — drop: release counted children, cascading.
    for offset in cls.refcounted_slots() {
        let v = unsafe { (*obj).prop_at(offset).read() };
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
        unsafe {
            // The cycle collector must not keep a root into memory
            // about to be reused.
            crate::gc::forget_candidate(obj as *mut RcHeader);
            crate::memory::stdapi::ll_free(obj as *mut u8);
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

    unsafe extern "C" fn counting_destructor(_obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
    }

    unsafe extern "C" fn resurrecting_destructor(obj: *mut Object) {
        DESTRUCTS.fetch_add(1, Ordering::Relaxed);
        unsafe { ll_retain(obj as *mut RcHeader) };
        RESURRECT_INTO.store(obj as usize, Ordering::Relaxed);
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
            let x = unsafe { o.prop_at(16).read() };
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
                (*parent)
                    .prop_at(16)
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
