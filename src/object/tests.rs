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

/// A fresh object carries its header, its class and one starting
/// state per slot: a defaultless Box slot starts undefined and a
/// bitmap-tracked raw slot starts with its bit clear, which is what
/// tells an uninitialized slot from one holding null. The
/// construct-into-a-reserved-cell door shares the stamp, and the C
/// entry point takes the category as a `u32`.
mod what_the_factory_stamps {
    use super::*;

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
}

/// The completed user constructor registers the record, not the
/// factory: an object whose `__construct` threw, or whose record the
/// arena refused, is in no destructor log and runs no `__destruct`
/// at the reset.
mod who_owes_the_destructor {
    use super::*;

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
}

/// Teardown dispatches through the class's `dispose` pointer and
/// releases every counted slot, a Box run and a bare-pointer run
/// alike. A `__destruct` that publishes `$this` again aborts the
/// teardown and is never run twice, and one that merely borrows it
/// must not re-enter: under the guard a transient release reports no
/// death.
mod the_three_phases_of_a_death {
    use super::*;

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
}

/// One entry point answers for a class and for an interface; the
/// display and the itable it reads are `class.rs`'s.
mod the_type_test {
    use super::*;

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
