//! A survivor the reset's own drain kills is read no further by the
//! passes that follow, and the count of what it held comes out right
//! anyway. The two halves are one subject: skipping a corpse without
//! carrying its edges across settles a live entity at zero, and carrying
//! them without skipping leans on a corpse's slots staying readable
//! (`dev/DECISIONS.md`, "the reset reads no corpse").

use super::*;

/// The shape every test here is built on: an entity promoted by the
/// reset and killed by the same reset's release drain.
///
/// A heap reference box takes the entity — that is the escape, so it is
/// promoted — and the box goes into an arena slot, which is what logs the
/// box's release against the reset. The test drops the box's creation
/// reference, so the logged release is its last and the drain is what
/// kills both it and the entity behind it.
///
/// # Safety
/// `arena` is the live arena, `corpse_slot_owner` an arena object with a
/// traced property at offset 16, `victim` a live arena entity.
unsafe fn killed_by_the_drain(
    arena: *mut Arena,
    corpse_slot_owner: *mut Object,
    victim: *mut RcHeader,
    victim_tag: Tag,
) {
    unsafe { killed_by_the_drain_at(arena, corpse_slot_owner, 16, victim, victim_tag) }
}

/// [`killed_by_the_drain`] with the slot named, for a test that needs two
/// kills and therefore two records in one owner.
///
/// # Safety
/// As [`killed_by_the_drain`], with `offset` a traced property of the
/// owner.
unsafe fn killed_by_the_drain_at(
    arena: *mut Arena,
    corpse_slot_owner: *mut Object,
    offset: u32,
    victim: *mut RcHeader,
    victim_tag: Tag,
) {
    unsafe {
        let boxed = crate::reference::ll_reference_new();
        assert!(ref_store(
            arena,
            boxed as *mut RcHeader,
            &raw mut (*boxed).value,
            std::ptr::null_mut(),
            Value::entity(victim_tag, victim),
        ));
        let slot = Object::prop_at(corpse_slot_owner, offset);
        assert!(ref_store(
            arena,
            corpse_slot_owner as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Reference, boxed as *mut RcHeader),
        ));
        assert!(!crate::refcount::ll_release(boxed as *mut RcHeader));
    }
}

/// The second arena of the two nested-reset tests below, and the box
/// whose release it drains. Statics because the only code that touches
/// them is a destructor, which is reached by the runtime and takes no
/// arguments of the test's; the test bracket serializes the suite, so one
/// set serves both.
static INNER_ARENA: AtomicUsize = AtomicUsize::new(0);
static INNER_CONTEXT: AtomicUsize = AtomicUsize::new(0);
static INNER_SLOT_CLASS: AtomicUsize = AtomicUsize::new(0);
static HELD_BOX: AtomicUsize = AtomicUsize::new(0);

/// A second arena for a nested reset, and a heap box holding `victim` so
/// that `victim` escapes and is promoted by the **outer** reset. The box
/// keeps its creation reference, so nothing the outer arena's own log
/// drains can kill what it holds.
///
/// Give the arena back with [`close_the_second_arena`] once the outer
/// reset is over.
///
/// # Safety
/// `arena` is the outer arena, `slot_cls` a class with a traced property
/// at offset 16, and `victim` a live arena entity.
unsafe fn open_the_second_arena(
    arena: *mut Arena,
    slot_cls: *const crate::class::Class,
    victim: *mut RcHeader,
    victim_tag: Tag,
) {
    let inner_arena = Box::into_raw(Box::new(Arena::new()));
    let inner_context = Box::into_raw(Box::new(LLContext { arena: inner_arena }));
    let held = crate::reference::ll_reference_new();
    unsafe {
        assert!(ref_store(
            arena,
            held as *mut RcHeader,
            &raw mut (*held).value,
            std::ptr::null_mut(),
            Value::entity(victim_tag, victim),
        ));
    }

    INNER_ARENA.store(inner_arena as usize, Ordering::Relaxed);
    INNER_CONTEXT.store(inner_context as usize, Ordering::Relaxed);
    INNER_SLOT_CLASS.store(slot_cls as usize, Ordering::Relaxed);
    HELD_BOX.store(held as usize, Ordering::Relaxed);
}

/// Give the second arena and its context back.
///
/// # Safety
/// The nested reset has run and no pointer into that arena is held.
unsafe fn close_the_second_arena() {
    let context = INNER_CONTEXT.swap(0, Ordering::Relaxed) as *mut LLContext;
    let arena = INNER_ARENA.swap(0, Ordering::Relaxed) as *mut Arena;
    unsafe {
        drop(Box::from_raw(context));
        drop(Box::from_raw(arena));
    }
}

/// Run by the outer reset's drain, which is past the promotion of
/// everything that reset found. It hands the box holding an outer
/// survivor to a slot of the second arena and resets that arena, so the
/// release that kills the survivor is drained one window deeper.
unsafe extern "C" fn reset_the_second_arena(_o: *mut Object) {
    let inner = INNER_ARENA.load(Ordering::Relaxed) as *mut Arena;
    let inner_context = INNER_CONTEXT.load(Ordering::Relaxed) as *mut LLContext;
    let slot_cls = INNER_SLOT_CLASS.load(Ordering::Relaxed) as *const crate::class::Class;
    let held = HELD_BOX.load(Ordering::Relaxed) as *mut crate::reference::LLReference;
    unsafe {
        let owner = new_constructed(inner_context, slot_cls, MemoryCategory::RequestArena);
        let slot = Object::prop_at(owner, 16);
        assert!(ref_store(
            inner,
            owner as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Reference, held as *mut RcHeader),
        ));
        assert!(!crate::refcount::ll_release(held as *mut RcHeader));
        arena_reset_full(inner);
    }
}

/// Two arena objects hold the same COW array; the drain kills one of
/// them. The array must come out of the reset held by exactly the one
/// that lived.
///
/// **The escrow is what makes this 1 rather than 0.** The walk does not
/// reach the corpse's slot — the skip takes it out — and the release its
/// teardown performed is inside the delta, so the promotion-time snapshot
/// is the only thing that puts the edge back
/// (`dev/DECISIONS.md`, "the reset reads no corpse").
#[test]
fn a_cow_child_of_a_holder_the_drain_killed_settles_to_its_live_holders() {
    use crate::array::entity::ll_array_new;
    let _g = crate::memory::block_pool::test_guard();

    let holder_cls = ClassBuilder::new("CorpseCowHolder")
        .prop("items", true)
        .build();
    let cache_cls = ClassBuilder::new("CorpseCowCache")
        .prop("kept", true)
        .build();
    let corpse_cls = ClassBuilder::new("CorpseCowSlot").prop("box", true).build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(&mut *context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let dying =
        unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let living =
        unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let corpse =
        unsafe { new_constructed(&mut *context_ptr, corpse_cls, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

    unsafe {
        assert!(crate::array::testing::push(array, Value::int(7)));

        // Both holders take the array, arena into arena, so it is shared
        // rather than copied and its count is the count of its holders.
        for holder in [dying, living] {
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
        }

        // The living holder survives by an ordinary escape into the heap.
        store_prop(arena_ptr, cache, 16, living);
        // The dying one survives too — and is then killed by the drain.
        killed_by_the_drain(arena_ptr, corpse, dying as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(&mut *arena_ptr) };
    set_current_context(std::ptr::null_mut());

    unsafe {
        assert_eq!(
            (*(array as *mut RcHeader)).memory_category(),
            MemoryCategory::GcHeap,
            "the array stayed behind in the dying arena"
        );
        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            1,
            "the array is held by the survivor that lived, and by nothing else"
        );

        // And the count is the truth rather than a number: the last
        // holder's death takes the array with it.
        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}

/// A survivor in a block of its own, killed by the drain, with a COW
/// survivor beside it so the reconciliation actually runs. Its run went
/// back to the system at its death, so every later reader of that address
/// reads memory the process no longer owns — which is what the reset
/// window holds it against.
///
/// Miri is the regression: the read passes `cargo test` by construction
/// (`dev/WORKFLOW.md`, Tests).
#[test]
fn a_large_survivor_killed_by_the_drain_is_not_read_by_the_reconcile() {
    use crate::array::entity::ll_array_new;
    use crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN;
    let _g = crate::memory::block_pool::test_guard();

    let wide = crate::test_support::wide_class("WideUnderTheReconcile", RUN_FILLERS, None);
    let cache_cls = ClassBuilder::new("WideReconcileCache")
        .prop("kept", true)
        .build();
    let holder_cls = ClassBuilder::new("WideReconcileHolder")
        .prop("items", true)
        .build();
    let corpse_cls = ClassBuilder::new("WideReconcileSlot")
        .prop("box", true)
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(&mut *context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let holder =
        unsafe { new_constructed(&mut *context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let corpse =
        unsafe { new_constructed(&mut *context_ptr, corpse_cls, MemoryCategory::RequestArena) };
    let wide_obj =
        unsafe { new_constructed(&mut *context_ptr, wide, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

    let run = BlockHeader::of_ptr(wide_obj as *const u8) as usize;
    assert_eq!(
        unsafe { block_kind(wide_obj as *const u8) },
        BLOCK_KIND_ENTITY_LARGE_RUN,
        "the entity fits in a shared block, so this test proves nothing"
    );

    unsafe {
        // The COW survivor: without one, `reconcile_cow_counts` returns
        // on its first line and reads nothing at all.
        assert!(crate::array::testing::push(array, Value::int(3)));
        let slot = Object::prop_at(holder, 16);
        assert!(ref_store(
            arena_ptr,
            holder as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));
        store_prop(arena_ptr, cache, 16, holder);

        killed_by_the_drain(arena_ptr, corpse, wide_obj as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(&mut *arena_ptr) };
    set_current_context(std::ptr::null_mut());

    assert!(
        !crate::memory::large_entity::snapshot().contains(&run),
        "the run outlived the entity it was allocated for, so the window \
         never flushed it"
    );
    unsafe {
        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            1,
            "the surviving holder is the array's one holder"
        );
        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}

/// A survivor whose external hold is dropped **after** it was marked: the
/// heap slot that held it is overwritten by a destructor of the same
/// fixpoint, so its hold-count falls to zero and it is promoted reading
/// refcount 0 in the GcHeap category — the two words a corpse reads. Its
/// edges are live all the same, and a pass that decides death from those
/// two words settles its COW child one too low.
#[test]
fn a_survivor_promoted_at_refcount_zero_is_not_read_as_a_corpse() {
    use crate::array::entity::ll_array_new;
    let _g = crate::memory::block_pool::test_guard();

    static CACHE: AtomicUsize = AtomicUsize::new(0);

    /// `$cache->kept = null;` — the escape's **lose** event, run while the
    /// entity it releases is already in the reset's survivor list.
    unsafe extern "C" fn drop_the_only_hold(_o: *mut Object) {
        let cache = CACHE.load(Ordering::Relaxed) as *mut Object;
        unsafe {
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            store_prop(arena, cache, 16, std::ptr::null_mut());
        }
    }

    let holder_cls = ClassBuilder::new("ZeroHolder").prop("items", true).build();
    let cache_cls = ClassBuilder::new("ZeroCache").prop("kept", true).build();
    let trigger_cls = ClassBuilder::new("ZeroTrigger")
        .destructor(drop_the_only_hold as *const ())
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let holder = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let _trigger =
        unsafe { new_constructed(context_ptr, trigger_cls, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

    CACHE.store(cache as usize, Ordering::Relaxed);

    unsafe {
        assert!(crate::array::testing::push(array, Value::int(13)));
        let slot = Object::prop_at(holder, 16);
        assert!(ref_store(
            arena_ptr,
            holder as *mut RcHeader,
            slot,
            std::ptr::null_mut(),
            Value::entity(Tag::Array, array as *mut RcHeader),
        ));

        // The escape that makes it a survivor, and the destructor that
        // takes that escape back inside the same fixpoint.
        store_prop(arena_ptr, cache, 16, holder);
    }

    unsafe { arena_reset_full(arena_ptr) };
    set_current_context(std::ptr::null_mut());

    unsafe {
        assert_eq!(
            (*holder).rc.memory_category(),
            MemoryCategory::GcHeap,
            "the marked survivor was promoted whatever its count says"
        );
        assert_eq!(
            (*holder).rc.refcount,
            0,
            "the hold this survivor was promoted for is the one the \
             destructor dropped, so this test proves nothing otherwise"
        );
        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            1,
            "the array is held by the survivor's slot"
        );

        // No slot names the survivor after the reset, so its teardown —
        // and through it the array's — is the test's own.
        ll_object_die(holder);
        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}

/// Two holders of a COW child, both reached in a **later** round than the
/// child itself. The counting pass hands an already-promoted child one
/// compensating retain per holder, and the edge behind each of those
/// retains is walked by the reconciliation as well — so without the credit
/// the child leaves the reset owing two references nobody holds.
#[test]
fn a_cow_child_counted_again_in_a_later_round_credits_each_retain() {
    use crate::array::entity::ll_array_new;
    let _g = crate::memory::block_pool::test_guard();

    static CACHE: AtomicUsize = AtomicUsize::new(0);
    static LATE_HOLDERS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];

    /// Runs inside the release drain, which is past the counting pass of
    /// the round that promoted the array: what it escapes here is counted
    /// next round, against a child already carrying its new category.
    unsafe extern "C" fn escape_the_late_holders(_o: *mut Object) {
        let cache = CACHE.load(Ordering::Relaxed) as *mut Object;
        unsafe {
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            for (i, holder) in LATE_HOLDERS.iter().enumerate() {
                let holder = holder.load(Ordering::Relaxed) as *mut Object;
                store_prop(arena, cache, 32 + 16 * i as u32, holder);
            }
        }
    }

    let holder_cls = ClassBuilder::new("LateHolder").prop("items", true).build();
    let cache_cls = ClassBuilder::new("LateCache")
        .prop("first", true)
        .prop("second", true)
        .prop("third", true)
        .build();
    let trigger_cls = ClassBuilder::new("LateTrigger")
        .destructor(escape_the_late_holders as *const ())
        .build();
    let corpse_cls = ClassBuilder::new("LateSlot").prop("box", true).build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let early = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let late_a = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let late_b = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let trigger =
        unsafe { new_constructed(context_ptr, trigger_cls, MemoryCategory::RequestArena) };
    let corpse = unsafe { new_constructed(context_ptr, corpse_cls, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

    CACHE.store(cache as usize, Ordering::Relaxed);
    LATE_HOLDERS[0].store(late_a as usize, Ordering::Relaxed);
    LATE_HOLDERS[1].store(late_b as usize, Ordering::Relaxed);

    unsafe {
        assert!(crate::array::testing::push(array, Value::int(11)));
        // All three edges exist before the reset, so each is an edge the
        // walk finds and none of them is a store the delta accounts for.
        for holder in [early, late_a, late_b] {
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
        }

        // The first holder escapes now and is promoted with the array;
        // the other two escape from a destructor the drain runs.
        store_prop(arena_ptr, cache, 16, early);
        killed_by_the_drain(arena_ptr, corpse, trigger as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(arena_ptr) };
    set_current_context(std::ptr::null_mut());

    unsafe {
        for holder in [early, late_a, late_b] {
            assert_eq!(
                (*holder).rc.memory_category(),
                MemoryCategory::GcHeap,
                "a holder of the array stayed behind in the dying arena"
            );
        }

        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            3,
            "the array is held by its three holders, once each"
        );

        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}

/// The other reader of the survivor list, and the round that reaches it:
/// a survivor in a block of its own is killed by the drain, and a
/// destructor of the **next** round allocates, which is what puts
/// `retrace_survivors` over the whole list again.
///
/// **What the skip does is invisible in a count**, and this is the pass
/// where that matters most: a corpse's stale edge is +1 in the walk and
/// its teardown's release is -1 in the delta, so an ordinary run comes
/// out the same either way. So the test reads the counter the passes keep
/// of the corpses they walked (`reset_window::take_counters`), and reads
/// the recorded deaths beside it, since a reset that killed nothing would
/// satisfy the first count trivially.
///
/// Miri is the second half of it, and it takes both halves of the repair
/// to see: with the corpse skipped nothing reads the address at all, and
/// with the window parking the run the address is still mapped when it is
/// read. Remove the skip on a build whose window parks nothing and Miri
/// reports the read of a returned run (`dev/WORKFLOW.md`, Tests).
#[test]
fn a_large_survivor_killed_by_the_drain_is_not_read_by_the_retrace() {
    use crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN;
    let _g = crate::memory::block_pool::test_guard();

    static TAIL_CLASS: AtomicUsize = AtomicUsize::new(0);
    static NODE_CLASS: AtomicUsize = AtomicUsize::new(0);
    static CACHE: AtomicUsize = AtomicUsize::new(0);
    static LATE: AtomicUsize = AtomicUsize::new(0);
    static NODE: AtomicUsize = AtomicUsize::new(0);

    /// The dying survivor's own destructor, run by the drain. It leaves
    /// the next round two things to do: a destructor to run, and an
    /// escapee to mark.
    unsafe extern "C" fn leave_the_next_round_its_work(_o: *mut Object) {
        let tail = TAIL_CLASS.load(Ordering::Relaxed) as *const crate::class::Class;
        let cache = CACHE.load(Ordering::Relaxed) as *mut Object;
        let late = LATE.load(Ordering::Relaxed) as *mut Object;
        unsafe {
            new_constructed(std::ptr::null_mut(), tail, MemoryCategory::RequestArena);
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            store_prop(arena, cache, 16, late);
        }
    }

    /// Run by the next round's settle loop, after that round marked
    /// `late`: `$late->child = new Node();`, an arena→arena store into a
    /// survivor that is not promoted yet. The allocation moves the bump
    /// cursor, which is what the re-trace is conditioned on, and the
    /// child it leaves behind is what the re-trace has to find.
    unsafe extern "C" fn store_into_the_late_survivor(_o: *mut Object) {
        let node_cls = NODE_CLASS.load(Ordering::Relaxed) as *const crate::class::Class;
        let late = LATE.load(Ordering::Relaxed) as *mut Object;
        unsafe {
            let node =
                new_constructed(std::ptr::null_mut(), node_cls, MemoryCategory::RequestArena);
            NODE.store(node as usize, Ordering::Relaxed);
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            store_prop(arena, late, 16, node);
        }
    }

    let node_cls = ClassBuilder::new("RetraceNode").prop("next", true).build();
    let tail_cls = ClassBuilder::new("RetraceTail")
        .destructor(store_into_the_late_survivor as *const ())
        .build();
    let cache_cls = ClassBuilder::new("RetraceCache").prop("kept", true).build();
    let wide = crate::test_support::wide_class(
        "WideUnderTheRetrace",
        RUN_FILLERS,
        Some(leave_the_next_round_its_work as *const ()),
    );
    let corpse_cls = ClassBuilder::new("RetraceSlot").prop("box", true).build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let late = unsafe { new_constructed(context_ptr, node_cls, MemoryCategory::RequestArena) };
    let corpse = unsafe { new_constructed(context_ptr, corpse_cls, MemoryCategory::RequestArena) };
    let wide_obj = unsafe { new_constructed(context_ptr, wide, MemoryCategory::RequestArena) };

    TAIL_CLASS.store(tail_cls as usize, Ordering::Relaxed);
    NODE_CLASS.store(node_cls as usize, Ordering::Relaxed);
    CACHE.store(cache as usize, Ordering::Relaxed);
    LATE.store(late as usize, Ordering::Relaxed);
    NODE.store(0, Ordering::Relaxed);
    crate::memory::reset_window::take_counters();

    let run = BlockHeader::of_ptr(wide_obj as *const u8) as usize;
    assert_eq!(
        unsafe { block_kind(wide_obj as *const u8) },
        BLOCK_KIND_ENTITY_LARGE_RUN,
        "the entity fits in a shared block, so this test proves nothing"
    );

    unsafe { killed_by_the_drain(arena_ptr, corpse, wide_obj as *mut RcHeader, Tag::Object) };
    unsafe { arena_reset_full(arena_ptr) };
    set_current_context(std::ptr::null_mut());

    let (deaths, corpse_walks) = crate::memory::reset_window::take_counters();
    assert!(
        deaths > 0,
        "no teardown completed inside the reset, so no pass had a corpse \
         to skip"
    );
    assert_eq!(
        corpse_walks, 0,
        "a pass after the fixpoint walked an entity whose teardown had \
         completed"
    );

    let node = NODE.load(Ordering::Relaxed) as *mut Object;
    assert!(!node.is_null(), "the next round's destructor never ran");
    unsafe {
        assert_eq!(
            (*node).rc.memory_category(),
            MemoryCategory::GcHeap,
            "the child stored into a survivor mid-round was not traced, so \
             this test never reached the pass it is about"
        );
    }

    assert!(
        !crate::memory::large_entity::snapshot().contains(&run),
        "the run outlived the entity it was allocated for, so the window \
         never flushed it"
    );

    unsafe {
        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}

/// A survivor of one reset dying inside another. A destructor the outer
/// reset runs resolves a second arena and resets it, so the two windows
/// nest and the release that kills the outer reset's survivor is drained
/// by the inner one.
///
/// Two things have to reach across the nesting for the count to come out
/// right: the death, which the outer reset's passes read to skip the
/// corpse, and the corpse's promotion-time edges, which were snapshotted
/// by the **outer** window and must be paid into that same window's
/// escrow. Paid into the innermost one instead they are dropped with it,
/// and the live COW child settles one too low.
#[test]
fn a_survivor_dying_inside_a_nested_reset_pays_its_edges_to_the_outer_one() {
    use crate::array::entity::ll_array_new;
    let _g = crate::memory::block_pool::test_guard();

    /// Windows open when the outer reset's survivor died, read where the
    /// death happens. Zero means it never ran.
    static DEPTH_AT_DEATH: AtomicUsize = AtomicUsize::new(0);

    /// `$this->items = null;` before the death completes, and the reason
    /// it is here rather than left to the default teardown: a teardown
    /// that releases a child without nulling its slot leaves an edge the
    /// walk still finds, and that stale edge covers for the escrow. With
    /// the slot nulled, the corpse's edges reach the count only through
    /// the escrow, so the escrow's absence is a failure rather than a
    /// figure that happens to agree.
    unsafe extern "C" fn drop_the_edge_and_record_the_depth(obj: *mut Object) {
        DEPTH_AT_DEATH.store(crate::memory::reset_window::depth(), Ordering::Relaxed);
        unsafe {
            let slot = Object::prop_at(obj, 16);
            let held = entity_checked(&*slot);
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            assert!(ref_store(
                arena,
                obj as *mut RcHeader,
                slot,
                held,
                Value::null(),
            ));
        }
    }

    let holder_cls = ClassBuilder::new("NestedHolder")
        .prop("items", true)
        .build();
    let dying_cls = ClassBuilder::new("NestedDyingHolder")
        .prop("items", true)
        .destructor(drop_the_edge_and_record_the_depth as *const ())
        .build();
    let cache_cls = ClassBuilder::new("NestedCache").prop("kept", true).build();
    let slot_cls = ClassBuilder::new("NestedSlot").prop("box", true).build();
    let trigger_cls = ClassBuilder::new("NestedTrigger")
        .destructor(reset_the_second_arena as *const ())
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let dying = unsafe { new_constructed(context_ptr, dying_cls, MemoryCategory::RequestArena) };
    let living = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let corpse = unsafe { new_constructed(context_ptr, slot_cls, MemoryCategory::RequestArena) };
    let trigger =
        unsafe { new_constructed(context_ptr, trigger_cls, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

    unsafe {
        assert!(crate::array::testing::push(array, Value::int(17)));
        for holder in [dying, living] {
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
        }

        store_prop(arena_ptr, cache, 16, living);
        open_the_second_arena(arena_ptr, slot_cls, dying as *mut RcHeader, Tag::Object);
        killed_by_the_drain(arena_ptr, corpse, trigger as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(arena_ptr) };
    set_current_context(std::ptr::null_mut());

    assert_eq!(
        DEPTH_AT_DEATH.load(Ordering::Relaxed),
        2,
        "the survivor died beside the second reset rather than inside it, \
         so the nesting this test is about never happened"
    );

    unsafe {
        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            1,
            "the array is held by the survivor that lived, and by nothing else"
        );

        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
        close_the_second_arena();
    }
}

/// A survivor the drain releases to zero whose `__destruct` puts it back
/// into a heap slot: teardown does not complete, so there is no death for
/// the reset to record and nothing of what it holds is the reset's to
/// take over.
///
/// **The count agrees either way here**, which is why the test reads the
/// record instead: the entity holds at its would-be death exactly the
/// edges of its promotion snapshot, so an escrowed edge replaces exactly
/// the edge a wrongly-recorded death removes from the walk. What differs
/// is everything else the record decides — the passes stop reading a live
/// entity, and its snapshot is spent, so the death it really dies later
/// pays nothing.
#[test]
fn a_survivor_that_resurrects_itself_records_no_death() {
    use crate::array::entity::ll_array_new;
    let _g = crate::memory::block_pool::test_guard();

    static CACHE: AtomicUsize = AtomicUsize::new(0);
    static RESURRECTED: AtomicUsize = AtomicUsize::new(0);
    /// What the reset held about the resurrected entity, read from the
    /// drain that resurrected it: 0 never ran, 1 no death recorded, 2 a
    /// death recorded.
    static RECORD: AtomicUsize = AtomicUsize::new(0);

    /// `$cache->second = $this;` — the store that takes the object back
    /// off the teardown path.
    unsafe extern "C" fn take_myself_back(obj: *mut Object) {
        let cache = CACHE.load(Ordering::Relaxed) as *mut Object;
        unsafe {
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            store_prop(arena, cache, 32, obj);
        }
    }

    /// A second death, drained after the first, which is where the
    /// question can be asked: the record lives for as long as the reset
    /// does and no longer.
    ///
    /// The order is the release log's, and the log's order is a property
    /// of one segment rather than a contract, so the resurrection is
    /// asserted here rather than assumed. Drained the other way round the
    /// answer would be "no death recorded" for the trivial reason that
    /// the death has not happened yet, and the test would pass under the
    /// defect it exists to catch.
    unsafe extern "C" fn read_the_record(_o: *mut Object) {
        let cache = CACHE.load(Ordering::Relaxed) as *mut Object;
        let resurrected = RESURRECTED.load(Ordering::Relaxed) as *mut RcHeader;
        unsafe {
            let slot = Object::prop_at(cache, 32);
            assert_eq!(
                entity_checked(&*slot),
                resurrected,
                "this record drained before the resurrection it is here to \
                 observe"
            );
        }

        let died = crate::memory::reset_window::has_died(resurrected);
        RECORD.store(1 + died as usize, Ordering::Relaxed);
    }

    let holder_cls = ClassBuilder::new("ResurrectedHolder")
        .prop("items", true)
        .destructor(take_myself_back as *const ())
        .build();
    let probe_cls = ClassBuilder::new("ResurrectionProbe")
        .destructor(read_the_record as *const ())
        .build();
    let cache_cls = ClassBuilder::new("ResurrectionCache")
        .prop("kept", true)
        .prop("second", true)
        .build();
    let slots_cls = ClassBuilder::new("ResurrectionSlots")
        .prop("first", true)
        .prop("second", true)
        .build();
    let living_cls = ClassBuilder::new("ResurrectionLiving")
        .prop("items", true)
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let dying = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let living = unsafe { new_constructed(context_ptr, living_cls, MemoryCategory::RequestArena) };
    let probe = unsafe { new_constructed(context_ptr, probe_cls, MemoryCategory::RequestArena) };
    let slots = unsafe { new_constructed(context_ptr, slots_cls, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };

    CACHE.store(cache as usize, Ordering::Relaxed);
    RESURRECTED.store(dying as usize, Ordering::Relaxed);
    RECORD.store(0, Ordering::Relaxed);

    unsafe {
        assert!(crate::array::testing::push(array, Value::int(19)));
        for holder in [dying, living] {
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
        }

        store_prop(arena_ptr, cache, 16, living);
        // Two records in one owner, drained in the order they were
        // written: the resurrection first, the question after it.
        killed_by_the_drain_at(arena_ptr, slots, 16, dying as *mut RcHeader, Tag::Object);
        killed_by_the_drain_at(arena_ptr, slots, 32, probe as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(arena_ptr) };
    set_current_context(std::ptr::null_mut());

    assert_eq!(
        RECORD.load(Ordering::Relaxed),
        1,
        "a teardown that did not complete was recorded as a death"
    );

    unsafe {
        assert_eq!(
            (*dying).rc.refcount,
            1,
            "the resurrected object is held by the slot its destructor \
             stored it into"
        );
        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            2,
            "both holders came out of the reset alive"
        );

        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}

/// The same nesting, with the corpse in a block of its own and a weak
/// reference on it. The inner reset is what frees the run, and the outer
/// reset's weak walk reads one header word of every entity its log names
/// — the corpse among them, because a promoted survivor keeps the log
/// entry it was given as an arena entity.
///
/// So the run has to outlive the reset that parked it: the inner close
/// hands its parked bodies outward, and only the outermost close frees
/// anything. Miri is the regression — with nothing parked it reports the
/// weak walk's read of a returned run (`dev/WORKFLOW.md`, Tests).
#[test]
fn a_run_parked_by_a_nested_reset_outlives_the_reset_that_parked_it() {
    use crate::memory::block_pool::BLOCK_KIND_ENTITY_LARGE_RUN;
    let _g = crate::memory::block_pool::test_guard();

    let wide = crate::test_support::wide_class("WideUnderTheNesting", RUN_FILLERS, None);
    let slot_cls = ClassBuilder::new("NestedRunSlot").prop("box", true).build();
    let trigger_cls = ClassBuilder::new("NestedRunTrigger")
        .destructor(reset_the_second_arena as *const ())
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let corpse = unsafe { new_constructed(context_ptr, slot_cls, MemoryCategory::RequestArena) };
    let trigger =
        unsafe { new_constructed(context_ptr, trigger_cls, MemoryCategory::RequestArena) };
    let wide_obj = unsafe { new_constructed(context_ptr, wide, MemoryCategory::RequestArena) };

    let run = BlockHeader::of_ptr(wide_obj as *const u8) as usize;
    assert_eq!(
        unsafe { block_kind(wide_obj as *const u8) },
        BLOCK_KIND_ENTITY_LARGE_RUN,
        "the entity fits in a shared block, so this test proves nothing"
    );

    // The weak cell is what puts the entity in the arena's weak log, and
    // the log is what the outer reset reads it out of after the inner one
    // has freed it.
    let weak = unsafe { crate::weak::ll_weakref_create(context_ptr, wide_obj as *mut RcHeader) };
    assert!(!weak.is_null(), "the weak table refused the cell");

    unsafe {
        open_the_second_arena(arena_ptr, slot_cls, wide_obj as *mut RcHeader, Tag::Object);
        killed_by_the_drain(arena_ptr, corpse, trigger as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(arena_ptr) };
    set_current_context(std::ptr::null_mut());

    assert!(
        unsafe { crate::weak::ll_weakref_get(weak) }.is_null(),
        "the weak cell still names an entity that died inside the second \
         reset"
    );
    assert!(
        !crate::memory::large_entity::snapshot().contains(&run),
        "the run outlived the reset chain that parked it"
    );

    unsafe {
        assert!(crate::refcount::ll_release(weak as *mut RcHeader));
        crate::weak::weakref_die(weak);
        close_the_second_arena();
    }
}

/// A corpse of this reset, in a block it shares with a survivor that
/// lived, freed while an epoch is in flight. Its free is the reset's to
/// absorb: the index built afterwards declines to count an occupant whose
/// header reads zero, so there is no count for that death to spend.
///
/// Parked instead, the free replays after the epoch against exactly that
/// index — one occupant short of what it was built from — and the block
/// goes to the pool with a living survivor inside it.
#[test]
fn a_corpse_freed_under_an_epoch_is_absorbed_rather_than_parked() {
    use crate::memory::block_pool::BLOCK_KIND_RETAINED;
    use crate::memory::deferred_free;
    let _g = crate::memory::block_pool::test_guard();

    let cache_cls = ClassBuilder::new("EpochCache")
        .prop("kept", true)
        .prop("second", true)
        .build();
    let holder_cls = ClassBuilder::new("EpochHolder").prop("items", true).build();
    let corpse_cls = ClassBuilder::new("EpochSlot").prop("box", true).build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let living = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let dying = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let corpse = unsafe { new_constructed(context_ptr, corpse_cls, MemoryCategory::RequestArena) };

    let block = BlockHeader::of_ptr(living as *const u8) as usize;
    assert_eq!(
        BlockHeader::of_ptr(dying as *const u8) as usize,
        block,
        "the two survivors took different blocks, so no index counts them \
         together"
    );

    unsafe {
        store_prop(arena_ptr, cache, 16, living);
        killed_by_the_drain(arena_ptr, corpse, dying as *mut RcHeader, Tag::Object);
    }

    // The bit alone, without the collector above it: what the reset meets
    // is a free path that parks, and nothing else of the epoch protocol
    // is in play (`memory::deferred_free`).
    deferred_free::begin_epoch();
    unsafe { arena_reset_full(arena_ptr) };
    deferred_free::end_epoch();
    unsafe { deferred_free::flush() };
    set_current_context(std::ptr::null_mut());

    assert_eq!(
        unsafe { block_kind(living as *const u8) },
        BLOCK_KIND_RETAINED,
        "the block went back to the pool under a living survivor"
    );
    assert!(
        crate::memory::retained::has_occupant_index(block),
        "the block's occupant index is gone while an occupant is alive"
    );

    unsafe {
        assert_eq!(
            (*living).rc.memory_category(),
            MemoryCategory::GcHeap,
            "the surviving occupant was not promoted"
        );

        // The last occupant's death is what returns the block, and it
        // arrives here rather than during the reset.
        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }

    assert!(
        !crate::memory::retained::has_occupant_index(block),
        "the index outlived the last occupant it was built for"
    );
}

/// The same absorb, in the block a refused carry pinned. The pin makes
/// the block known to the registry before any index exists, so a corpse
/// of this reset can be freed in a block the registry already answers
/// for — and the answer the absorb needs is whether the block has an
/// occupant index, not whether it has a registry entry.
///
/// Under an epoch the difference is the block: the free parks, the index
/// is built without the corpse, and the flush spends a count the index
/// never took, leaving the block one short of its true occupancy and
/// ready to go home under the two survivors still living in it.
#[test]
fn a_corpse_in_a_block_pinned_for_bytes_is_absorbed_like_any_other() {
    use crate::memory::block_pool::{BLOCK_KIND_FREE, BLOCK_KIND_RETAINED};
    use crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED;
    use crate::memory::deferred_free;
    use crate::test_support::outside_block;
    use std::sync::atomic::Ordering;
    let _g = crate::memory::block_pool::test_guard();

    let holder_cls = ClassBuilder::new("PinnedAbsorbHolder")
        .prop("kept", true)
        .build();
    let neighbour_cls = ClassBuilder::new("PinnedAbsorbNeighbour").build();
    let corpse_cls = ClassBuilder::new("PinnedAbsorbSlot")
        .prop("box", true)
        .build();
    let waker_cls = outside_block::class("WakerFreedUnderAnEpoch");

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    // Two heap holders rather than one, so the two survivors of the
    // block can be killed one at a time: what a spent count costs is a
    // block handed over while the **second** of them is still alive.
    let holder = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let keeper = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::GcHeap) };
    let first =
        unsafe { new_constructed(context_ptr, neighbour_cls, MemoryCategory::RequestArena) };
    let second =
        unsafe { new_constructed(context_ptr, neighbour_cls, MemoryCategory::RequestArena) };
    let waker = unsafe { new_constructed(context_ptr, waker_cls, MemoryCategory::RequestArena) };
    let corpse = unsafe { new_constructed(context_ptr, corpse_cls, MemoryCategory::RequestArena) };

    let storage = unsafe { outside_block::install_block(context_ptr, waker) };
    let block = BlockHeader::of_ptr(storage) as usize;
    for (entity, name) in [
        (first as *const u8, "first"),
        (second as *const u8, "second"),
        (waker as *const u8, "the waker"),
    ] {
        assert_eq!(
            BlockHeader::of_ptr(entity) as usize,
            block,
            "{name} was bumped into another block, so the pin and the \
             corpse do not meet in one block"
        );
    }

    unsafe {
        // The two neighbours are what the block must still be held for.
        store_prop(arena_ptr, holder, 16, first);
        store_prop(arena_ptr, keeper, 16, second);
        killed_by_the_drain(arena_ptr, corpse, waker as *mut RcHeader, Tag::Object);
    }

    let refusals_before = crate::memory::buffer_arena::refusals();
    FORCE_REFUSE_LONGLIVED.store(true, Ordering::Relaxed);
    deferred_free::begin_epoch();
    unsafe { arena_reset_full(arena_ptr) };
    deferred_free::end_epoch();
    unsafe { deferred_free::flush() };
    FORCE_REFUSE_LONGLIVED.store(false, Ordering::Relaxed);
    set_current_context(std::ptr::null_mut());

    assert_eq!(
        crate::memory::buffer_arena::refusals() - refusals_before,
        1,
        "the carry was not refused, so nothing pinned this block and the \
         test proves nothing"
    );
    assert_eq!(
        unsafe { block_kind(block as *const u8) },
        BLOCK_KIND_RETAINED,
        "the block went home while two survivors were still living in it"
    );

    unsafe {
        assert!(crate::refcount::ll_release(holder as *mut RcHeader));
        ll_object_die(holder);
    }

    assert_eq!(
        unsafe { block_kind(block as *const u8) },
        BLOCK_KIND_RETAINED,
        "the first occupant's death emptied the block, so a count was \
         spent that the index never took"
    );

    unsafe {
        assert!(crate::refcount::ll_release(keeper as *mut RcHeader));
        ll_object_die(keeper);
    }

    assert_eq!(
        unsafe { *(block as *const u32) },
        BLOCK_KIND_FREE,
        "the block outlived the last of its occupants"
    );
}

/// A COW child whose edge is **unset** before its holder dies. The
/// release the unset performed is inside the child's delta, and the
/// holder that owed the compensation is gone by the time the count is
/// settled — so without the promotion-time snapshot the same event is
/// subtracted twice and the child settles at zero, which
/// `retained::register` reads as an empty slot under a living holder.
#[test]
fn an_edge_unset_before_its_holder_dies_still_settles_its_child() {
    use crate::array::entity::ll_array_new;
    let _g = crate::memory::block_pool::test_guard();

    static DOOMED_HOLDER: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn unset_the_edge(_o: *mut Object) {
        let holder = DOOMED_HOLDER.load(Ordering::Relaxed) as *mut Object;
        unsafe {
            let arena = crate::memory::context::resolve_arena(std::ptr::null_mut());
            store_prop(arena, holder, 16, std::ptr::null_mut());
        }
    }

    let holder_cls = ClassBuilder::new("UnsetHolder").prop("items", true).build();
    let cache_cls = ClassBuilder::new("UnsetCache").prop("kept", true).build();
    let slots_cls = ClassBuilder::new("UnsetSlots")
        .prop("first", true)
        .prop("second", true)
        .build();
    let trigger_cls = ClassBuilder::new("UnsetTrigger")
        .destructor(unset_the_edge as *const ())
        .build();

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let mut context = LLContext { arena: arena_ptr };
    let context_ptr: *mut LLContext = &mut context;
    set_current_context(context_ptr);

    let cache = unsafe { new_constructed(context_ptr, cache_cls, MemoryCategory::GcHeap) };
    let slots = unsafe { new_constructed(context_ptr, slots_cls, MemoryCategory::RequestArena) };
    let doomed = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let living = unsafe { new_constructed(context_ptr, holder_cls, MemoryCategory::RequestArena) };
    let trigger =
        unsafe { new_constructed(context_ptr, trigger_cls, MemoryCategory::RequestArena) };
    let array = unsafe { ll_array_new(MemoryCategory::RequestArena) };
    DOOMED_HOLDER.store(doomed as usize, Ordering::Relaxed);

    unsafe {
        assert!(crate::array::testing::push(array, Value::int(5)));
        for holder in [doomed, living] {
            let slot = Object::prop_at(holder, 16);
            assert!(ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, array as *mut RcHeader),
            ));
        }

        store_prop(arena_ptr, cache, 16, living);
        // The trigger dies first and unsets the edge; the holder dies
        // after it, holding nothing. Order is the release log's, which is
        // the order these two records were written in.
        killed_by_the_drain_at(arena_ptr, slots, 16, trigger as *mut RcHeader, Tag::Object);
        killed_by_the_drain_at(arena_ptr, slots, 32, doomed as *mut RcHeader, Tag::Object);
    }

    unsafe { arena_reset_full(&mut *arena_ptr) };
    set_current_context(std::ptr::null_mut());

    unsafe {
        assert_eq!(
            (*(array as *mut RcHeader)).refcount,
            1,
            "the array is held by the survivor that lived, and by nothing else"
        );
        assert!(crate::refcount::ll_release(cache as *mut RcHeader));
        ll_object_die(cache);
    }
}
