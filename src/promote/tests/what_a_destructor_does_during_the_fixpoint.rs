//! Destructors run inside the settling loop, so the graph moves
//! under it: a store into an already-traced survivor is arena to
//! arena and escapes nothing, which is why the reset watches the
//! bump cursor and re-reads the survivors' children; an escapee
//! created there survives although it has already run its own
//! `__destruct`; and a release log grown during its own drain is
//! drained again. A COW survivor's count stays readable throughout
//! and is settled once at the end, from the edges that remain plus
//! the holders acquired after promotion.

use super::*;

#[test]
fn destructor_created_escape_survives_already_destructed() {
    let _g = crate::memory::block_pool::test_guard();
    static HOLDER_SLOT: AtomicUsize = AtomicUsize::new(0);
    static DTORS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn escaping_dtor(obj: *mut Object) {
        DTORS.fetch_add(1, Ordering::Relaxed);
        // `$GLOBALS['x'] = $this;` — through the real barrier, with
        // the TLS context (as generated destructor code would).
        let holder = HOLDER_SLOT.load(Ordering::Relaxed) as *mut Object;
        unsafe {
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                arena,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Object, obj as *mut RcHeader),
            ));
        }
    }

    let holder_cls = ClassBuilder::new("Globals").prop("x", true).build();
    let cls = ClassBuilder::new("LastWill")
        .destructor(escaping_dtor as *const ())
        .build();

    // One raw pointer per entity, reused — the shape generated code
    // actually has (an `LLContext*` in a register). Taking a fresh
    // `&mut arena`/`&mut ctx` per call would retag, invalidating the
    // pointer `set_current_context` parked in TLS.
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let ctx_ptr: *mut LLContext = &mut ctx;
    set_current_context(ctx_ptr);

    let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
    HOLDER_SLOT.store(holder as usize, Ordering::Relaxed);
    let obj = unsafe { new_constructed(ctx_ptr, cls, MemoryCategory::RequestArena) };

    unsafe { arena_reset_full(arena_ptr) };
    set_current_context(std::ptr::null_mut());

    assert_eq!(DTORS.load(Ordering::Relaxed), 1);
    unsafe {
        assert_eq!(
            (*obj).rc.memory_category(),
            MemoryCategory::GcHeap,
            "the destructor-created escape was caught by the fixpoint"
        );
        assert_eq!((*obj).rc.refcount, 1);
        assert_ne!(
            (*obj).rc.flags & DESTRUCTOR_RAN,
            0,
            "survives already-destructed"
        );
        assert_ne!((*obj).rc.flags & DESTRUCTOR_PENDING, 0);
    }
}

/// Regression for H2: a "dirty" destructor stores a *fresh* arena object
/// into an already-traced survivor. That store is arena→arena, so the
/// barrier does not escape it; without re-tracing the survivor after a
/// dirty destructor, the new child is never marked and dangles once the
/// survivor is promoted. The reset watches the arena bump cursor to know
/// a destructor allocated, then re-reads the survivors' children.
#[test]
fn dirty_destructor_storing_into_a_survivor_traces_the_new_child() {
    let _g = crate::memory::block_pool::test_guard();

    static SURVIVOR: AtomicUsize = AtomicUsize::new(0);
    static NODE_CLS: AtomicUsize = AtomicUsize::new(0);
    static NEW_CHILD: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn mutate_survivor_dtor(_o: *mut Object) {
        let node_cls = NODE_CLS.load(Ordering::Relaxed) as *const crate::class::Class;
        let s = SURVIVOR.load(Ordering::Relaxed) as *mut Object;
        // `$s->next = new Node();` — a fresh arena object stored into an
        // already-traced survivor (arena→arena: not an escape).
        let node = unsafe {
            new_constructed(std::ptr::null_mut(), node_cls, MemoryCategory::RequestArena)
        };

        NEW_CHILD.store(node as usize, Ordering::Relaxed);
        unsafe {
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            store_prop(arena, s, 16, node);
        }
    }

    let node_cls = ClassBuilder::new("Node").prop("next", true).build();
    let holder_cls = ClassBuilder::new("Cache").prop("keep", true).build();
    let trigger_cls = ClassBuilder::new("Trigger")
        .destructor(mutate_survivor_dtor as *const ())
        .build();

    // One raw pointer each, reused (see the note in
    // `destructor_created_escape_survives_already_destructed`): the
    // destructor reenters and resolves this same arena, so the reset
    // must be handed the very pointer the context holds.
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let ctx_ptr: *mut LLContext = &mut ctx;
    set_current_context(ctx_ptr);

    let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
    let s = unsafe { new_constructed(ctx_ptr, node_cls, MemoryCategory::RequestArena) };
    let _trigger = unsafe { new_constructed(ctx_ptr, trigger_cls, MemoryCategory::RequestArena) };

    NODE_CLS.store(node_cls as usize, Ordering::Relaxed);
    SURVIVOR.store(s as usize, Ordering::Relaxed);
    NEW_CHILD.store(0, Ordering::Relaxed);

    unsafe {
        // S escapes into the heap holder → it is a survivor.
        store_prop(arena_ptr, holder, 16, s);
        // Trigger is unheld with a destructor (tracked); at reset its
        // destructor stores a fresh Node into survivor S.
        arena_reset_full(arena_ptr);
    }

    set_current_context(std::ptr::null_mut());

    let node = NEW_CHILD.load(Ordering::Relaxed) as *mut Object;
    assert!(!node.is_null(), "the destructor created the child");
    unsafe {
        assert_eq!(
            (*s).rc.memory_category(),
            MemoryCategory::GcHeap,
            "survivor promoted"
        );
        assert_eq!(
            (*node).rc.memory_category(),
            MemoryCategory::GcHeap,
            "the destructor-added child was traced and promoted, not left to die with the arena"
        );
        assert_eq!((*node).rc.refcount, 1, "held once, by the survivor's slot");

        // Teardown cascades holder → s → node with no dangling.
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }
}

/// A COW entity's count is a value, and it stays readable through the
/// whole fixpoint. Marking a survivor used to zero it, so a destructor
/// releasing the same string — an ordinary `unset` — decremented from
/// zero and underflowed inside the reset. The count is settled once
/// instead, after the last destructor, from the edges that remain.
#[test]
fn a_destructor_may_release_a_cow_survivor_during_the_fixpoint() {
    let _g = crate::memory::block_pool::test_guard();

    static DROPPER: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn unset_the_string_dtor(o: *mut Object) {
        // `unset($this->s)` — the store barrier releases the string
        // this object holds, while the reset is still settling.
        unsafe {
            let slot = Object::prop_at(o, 16);
            let old = entity_checked(&*slot);
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            assert!(ref_store(
                arena,
                o as *mut RcHeader,
                slot,
                old,
                Value::null()
            ));
        }
    }

    let keeper_cls = ClassBuilder::new("Keeper").prop("s", true).build();
    let holder_cls = ClassBuilder::new("Cache").prop("keep", true).build();
    let dropper_cls = ClassBuilder::new("Dropper")
        .prop("s", true)
        .destructor(unset_the_string_dtor as *const ())
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let ctx_ptr: *mut LLContext = &mut ctx;
    set_current_context(ctx_ptr);

    let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
    let keeper = unsafe { new_constructed(ctx_ptr, keeper_cls, MemoryCategory::RequestArena) };
    let dropper = unsafe { new_constructed(ctx_ptr, dropper_cls, MemoryCategory::RequestArena) };
    DROPPER.store(dropper as usize, Ordering::Relaxed);

    let s =
        unsafe { crate::string::ll_string_new(ctx_ptr, MemoryCategory::RequestArena, b"shared") }
            as *mut RcHeader;

    unsafe {
        for owner in [keeper, dropper] {
            let slot = Object::prop_at(owner, 16);
            assert!(ref_store(
                arena_ptr,
                owner as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::String, s),
            ));
        }

        // The creation reference goes, as it would at the end of the
        // statement that built the string.
        assert!(!crate::refcount::ll_release(s));
        assert_eq!((*s).refcount, 2, "both holders, counted as COW demands");

        // Keeper escapes: it survives, and the string with it. Dropper
        // is unheld, so its destructor runs during the fixpoint.
        store_prop(arena_ptr, holder, 16, keeper);
        arena_reset_full(arena_ptr);
    }

    set_current_context(std::ptr::null_mut());

    unsafe {
        assert_eq!(
            (*s).memory_category(),
            MemoryCategory::GcHeap,
            "the string survived with its keeper"
        );
        assert_eq!(
            (*s).refcount,
            1,
            "one surviving holder: the dead one never released twice"
        );
    }
}

/// A holder acquired **after** the survivor was promoted must survive
/// the reconciliation. Promotion happens inside the settling loop and
/// the release-log drain runs user destructors after it, so a
/// destructor can store an already-promoted string into a heap object
/// that outlives the request — a legitimate `+1` that no edge between
/// survivors accounts for. Assigning the count from those edges alone
/// erased it, which left the string with one count and two holders.
#[test]
fn a_holder_acquired_after_promotion_keeps_its_count() {
    let _g = crate::memory::block_pool::test_guard();
    static CACHE: AtomicUsize = AtomicUsize::new(0);
    static STRING: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn cache_the_string_dtor(_o: *mut Object) {
        // A dying heap entity, torn down by the release drain, puts the
        // string into a heap object: `Cache::$last = $s`.
        let cache = CACHE.load(Ordering::Relaxed) as *mut Object;
        let s = STRING.load(Ordering::Relaxed) as *mut RcHeader;
        unsafe {
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            let slot = Object::prop_at(cache, 16);
            assert!(ref_store(
                arena,
                cache as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::String, s),
            ));
        }
    }

    let keeper_cls = ClassBuilder::new("Keeper").prop("s", true).build();
    let holder_cls = ClassBuilder::new("Holder").prop("keep", true).build();
    let cache_cls = ClassBuilder::new("Cache").prop("last", true).build();
    let dying_cls = ClassBuilder::new("Dying")
        .destructor(cache_the_string_dtor as *const ())
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let ctx_ptr: *mut LLContext = &mut ctx;
    set_current_context(ctx_ptr);

    let holder = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::GcHeap) };
    let cache = unsafe { new_constructed(ctx_ptr, cache_cls, MemoryCategory::GcHeap) };
    let keeper = unsafe { new_constructed(ctx_ptr, keeper_cls, MemoryCategory::RequestArena) };
    let container = unsafe { new_constructed(ctx_ptr, holder_cls, MemoryCategory::RequestArena) };
    let dying = unsafe { new_constructed(ctx_ptr, dying_cls, MemoryCategory::GcHeap) };
    CACHE.store(cache as usize, Ordering::Relaxed);

    let s =
        unsafe { crate::string::ll_string_new(ctx_ptr, MemoryCategory::RequestArena, b"cached") }
            as *mut RcHeader;
    STRING.store(s as usize, Ordering::Relaxed);

    unsafe {
        // The keeper holds the string and escapes, so both survive.
        let slot = Object::prop_at(keeper, 16);
        assert!(ref_store(
            arena_ptr,
            keeper as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::String, s),
        ));
        assert!(!crate::refcount::ll_release(s), "the creation reference");
        store_prop(arena_ptr, holder, 16, keeper);

        // The dying heap entity sits in an arena container, so the
        // release log tears it down — after the promotion pass.
        store_prop(arena_ptr, container, 16, dying);
        assert!(!crate::refcount::ll_release(dying as *mut RcHeader));

        arena_reset_full(arena_ptr);
    }

    set_current_context(std::ptr::null_mut());

    unsafe {
        assert_eq!(
            (*s).refcount,
            2,
            "the keeper's slot and the one the destructor added"
        );
        assert_eq!((*s).memory_category(), MemoryCategory::GcHeap);
    }
}

/// Regression for H7: a release-log entity's `__destruct` runs during
/// the release drain and appends a *new* release-log entry (it stores a
/// heap reference into a still-alive arena container). The single-pass
/// reset drained the log once and dropped that late entry, tripping
/// finish_reset's "logs drained" assert; the settling loop re-drains it.
#[test]
fn release_log_grown_during_the_drain_is_still_drained() {
    let _g = crate::memory::block_pool::test_guard();
    static C2_PTR: AtomicUsize = AtomicUsize::new(0);
    static B_PTR: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn a_dtor(_o: *mut Object) {
        // A, dying, stores heap B into the arena container C2 → appends
        // a release-log entry *while the log is being drained*.
        let c2 = C2_PTR.load(Ordering::Relaxed) as *mut Object;
        let b = B_PTR.load(Ordering::Relaxed) as *mut Object;
        unsafe {
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            store_prop(arena, c2, 16, b);
        }
    }

    let cont_cls = ClassBuilder::new("Container").prop("x", true).build();
    let a_cls = ClassBuilder::new("A")
        .destructor(a_dtor as *const ())
        .build();
    let b_cls = ClassBuilder::new("B").build();

    // One raw pointer each, reused: `a_dtor` reenters and resolves
    // this same arena during the release drain.
    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut ctx = LLContext { arena: arena_ptr };
    let ctx_ptr: *mut LLContext = &mut ctx;
    set_current_context(ctx_ptr);

    let c1 = unsafe { new_constructed(ctx_ptr, cont_cls, MemoryCategory::RequestArena) };
    let c2 = unsafe { new_constructed(ctx_ptr, cont_cls, MemoryCategory::RequestArena) };
    let a = unsafe { new_constructed(ctx_ptr, a_cls, MemoryCategory::GcHeap) };
    let b = unsafe { new_constructed(ctx_ptr, b_cls, MemoryCategory::GcHeap) };

    C2_PTR.store(c2 as usize, Ordering::Relaxed);
    B_PTR.store(b as usize, Ordering::Relaxed);

    unsafe {
        // Heap A into arena container C1 → release-log entry, A retained.
        store_prop(arena_ptr, c1, 16, a);
        // A's only remaining reference is the log's (creator ref dropped).
        assert!(!crate::refcount::ll_release(a as *mut RcHeader));

        // Reset: releasing A runs a_dtor, which appends B's release-log
        // entry mid-drain; the loop must still drain it.
        arena_reset_full(arena_ptr);

        // B was retained by the store and released once by the re-drained
        // log: back to the creator's single reference (not leaked at 2).
        assert_eq!(
            (*b).rc.refcount,
            1,
            "B's late release-log entry was drained"
        );

        assert!(ll_release(b as *mut RcHeader));
        ll_object_die(b);
    }

    set_current_context(std::ptr::null_mut());
}
