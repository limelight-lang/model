use super::*;
use crate::array::table::Key;
use crate::refcount::ll_release;
use crate::string::{LLString, ll_string_new};

enum Shape {
    NestedThenObject,
    TwoNested,
    Mixed,
}

static DESTRUCTOR_ORDER: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// A class whose destructor appends one character to
/// [`DESTRUCTOR_ORDER`]. Each expansion is a distinct function item,
/// which is what makes the character identify the object.
macro_rules! recording_class {
    ($name:literal, $mark:literal) => {{
        unsafe extern "C" fn record(_o: *mut crate::object::Object) {
            DESTRUCTOR_ORDER.lock().unwrap().push($mark);
        }

        crate::class::ClassBuilder::new($name)
            .destructor(record as *const ())
            .build()
    }};
}

/// Build one of the shapes out of heap arrays and recording objects,
/// release the outermost array, and return what the destructors
/// wrote. Every entity is created at +1 and handed to the entry that
/// takes it, so the outermost array is the only holder.
fn destructor_order(shape: Shape) -> String {
    destructor_order_with(shape, false)
}

/// The same, with the drain's list refused for the teardown alone —
/// the flag is raised after the shape is built, because it also
/// refuses the storage every array here allocates.
fn destructor_order_with(shape: Shape, refuse_the_list: bool) -> String {
    let _g = crate::memory::block_pool::test_guard();
    DESTRUCTOR_ORDER.lock().unwrap().clear();
    let mut arena = crate::memory::arena::Arena::new();
    let mut ctx = crate::memory::context::LLContext {
        arena: &mut arena as *mut _,
    };

    let ctx_ptr: *mut crate::memory::context::LLContext = &mut ctx;
    // Mounted although every entity here is built with `ctx_ptr` in
    // hand: a `__destruct` is user code and resolves its own context
    // from TLS, which is the shape generated code has.
    crate::memory::context::set_current_context(ctx_ptr);

    let array = || unsafe { ll_array_new(MemoryCategory::GcHeap) };
    let put = |owner: *mut LLArray, key: i64, tag: crate::value::Tag, child: *mut RcHeader| unsafe {
        crate::array::testing::insert(owner, Key::Int(key), Value::entity(tag, child));
    };

    let object = |cls| unsafe {
        crate::object::new_constructed(ctx_ptr, cls, MemoryCategory::GcHeap) as *mut RcHeader
    };

    let root = array();
    match shape {
        Shape::NestedThenObject => {
            let nested = array();
            put(
                nested,
                0,
                crate::value::Tag::Object,
                object(recording_class!("B", 'B')),
            );
            put(root, 0, crate::value::Tag::Array, nested as *mut RcHeader);
            put(
                root,
                1,
                crate::value::Tag::Object,
                object(recording_class!("A", 'A')),
            );
        }
        Shape::TwoNested => {
            let first = array();
            put(
                first,
                0,
                crate::value::Tag::Object,
                object(recording_class!("B", 'B')),
            );
            let second = array();
            put(
                second,
                0,
                crate::value::Tag::Object,
                object(recording_class!("C", 'C')),
            );
            put(root, 0, crate::value::Tag::Array, first as *mut RcHeader);
            put(root, 1, crate::value::Tag::Array, second as *mut RcHeader);
        }
        Shape::Mixed => {
            let innermost = array();
            put(
                innermost,
                0,
                crate::value::Tag::Object,
                object(recording_class!("Three", '3')),
            );
            let middle = array();
            put(
                middle,
                0,
                crate::value::Tag::Object,
                object(recording_class!("Two", '2')),
            );
            put(
                middle,
                1,
                crate::value::Tag::Array,
                innermost as *mut RcHeader,
            );
            put(
                middle,
                2,
                crate::value::Tag::Object,
                object(recording_class!("Four", '4')),
            );
            put(
                root,
                0,
                crate::value::Tag::Object,
                object(recording_class!("One", '1')),
            );
            put(root, 1, crate::value::Tag::Array, middle as *mut RcHeader);
            put(
                root,
                2,
                crate::value::Tag::Object,
                object(recording_class!("Five", '5')),
            );
        }
    }

    if refuse_the_list {
        crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    unsafe {
        assert!(ll_release(root as *mut RcHeader), "the root had a holder");
        crate::object::ll_entity_die(root as *mut RcHeader);
    }

    crate::memory::buffer_arena::FORCE_REFUSE_LONGLIVED
        .store(false, std::sync::atomic::Ordering::Relaxed);
    crate::memory::context::set_current_context(std::ptr::null_mut());
    let written = DESTRUCTOR_ORDER.lock().unwrap().clone();
    written
}

fn mk(bytes: &[u8]) -> *mut LLString {
    unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) }
}

fn arr() -> *mut LLArray {
    unsafe { ll_array_new(MemoryCategory::GcHeap) }
}

/// The wrapper supplies the `RcHeader` and nothing else: no
/// per-instance class pointer, the same construction as a string,
/// because `array` is final and the entity kind already says what
/// this is.
mod the_entity_around_the_table {
    use super::*;

    #[test]
    fn a_fresh_array_is_a_cow_entity_of_the_array_kind_at_count_one() {
        let _g = crate::memory::block_pool::test_guard();
        let a = arr();
        assert!(!a.is_null());
        unsafe {
            assert_eq!((*a).rc.refcount, 1);
            assert_eq!((*a).rc.flags & crate::refcount::COW, crate::refcount::COW);
            assert_eq!(
                (*a).rc.flags & crate::refcount::ENTITY_KIND_MASK,
                EntityKind::Array.to_flags()
            );
            assert_eq!(category_of(a), MemoryCategory::GcHeap);
            assert!(crate::array::testing::table(a).is_empty());
            crate::array::entity::dispose_storage(a, category_of(a));
        }
    }

    /// The layout the design fixes: no per-instance class pointer, the
    /// same construction as a string. `array` is final, so the entity
    /// kind already says what this is.
    ///
    /// The head sits between the header and the representation, and that
    /// is what the 40 bytes between them are. It costs the entity nothing:
    /// the words were the table's before, so an array is the same 112
    /// bytes either way — which is the figure the placement was chosen
    /// against.
    #[test]
    fn an_array_carries_no_class_pointer() {
        assert_eq!(std::mem::offset_of!(LLArray, rc), 0);
        assert_eq!(
            std::mem::offset_of!(LLArray, head),
            8,
            "the walker's words start straight after the header"
        );
        assert_eq!(
            std::mem::offset_of!(LLArray, storage),
            8 + size_of::<StorageHead>(),
            "the representation follows the head — nothing between"
        );
        assert_eq!(size_of::<LLArray>(), 112);
    }
}

/// Where the head sits, expressed as the one thing that depends on it:
/// a collector thread reads the head's words while the owning thread is
/// mid-write in the representation beside them.
///
/// **`cargo test` cannot judge this.** Every word the walker reads is
/// atomic and the mutator's own writes go elsewhere, so a run reports
/// nothing whichever placement is in force; what the placement decides is
/// whether the mutator's `&mut` asserts uniqueness over the bytes the
/// walker is reading, and Miri is the only instrument that sees such a
/// claim (`dev/WORKFLOW.md`, and `dev/POSTMORTEM.md` 2026-08-10, where an
/// atomic field inside a borrowed struct was the same defect).
mod the_head_a_walker_reads {
    use super::*;

    /// A raw pointer handed to the collector's thread. The array outlives
    /// the walk: the join below is what makes that true, rather than a
    /// lifetime.
    struct Handed(*const StorageHead);
    unsafe impl Send for Handed {}

    /// The walker takes the head's address and reads through it while the
    /// owner inserts, which is the arrangement the whole bracket exists
    /// for. Nothing here asserts a moment: what a walker sees is any state
    /// the insert sequence passed through, and the coherence of it — a
    /// count no larger than the entries written, and the tag the array was
    /// stamped with — is all a reading can be judged by.
    #[test]
    fn a_walker_reads_the_head_while_the_mutator_writes_the_table() {
        const INSERTS: i64 = 32;
        /// More readings than there are inserts, so the walker is still
        /// reading after the last one rather than racing to finish first.
        /// A counted loop rather than a flag the mutator lowers: a walker
        /// that is asked to stop can be scheduled for the first time
        /// after the request and read nothing at all, which is a green run
        /// over an untouched head.
        const READINGS: usize = 128;
        let _g = crate::memory::block_pool::test_guard();
        let a = arr();
        let handed = Handed(unsafe { storage_head(a) });
        let walker = std::thread::spawn(move || {
            let handed = handed;
            let mut accepted = 0usize;
            let mut highest = 0usize;
            for _ in 0..READINGS {
                if let Some(view) = unsafe { StorageHead::coherent(handed.0) } {
                    assert_eq!(view.tag, StorageTag::Hash);
                    assert!(
                        view.used <= INSERTS as usize,
                        "a reading counted more entries than were ever inserted"
                    );
                    accepted += 1;
                    highest = highest.max(view.used);
                }

                // The yield is what lets Miri's scheduler put the mutator
                // between two readings; without it a spin loop can hold
                // the interpreter for the whole insert sequence.
                std::thread::yield_now();
            }

            (accepted, highest)
        });
        for i in 0..INSERTS {
            unsafe { crate::array::testing::insert(a, Key::Int(i), Value::int(i)) };
            std::thread::yield_now();
        }

        let (accepted, highest) = walker.join().unwrap();
        assert!(accepted > 0, "the walker accepted no reading at all");
        assert!(highest <= INSERTS as usize);
        unsafe {
            crate::array::entity::dispose_storage(a, category_of(a));
            (*a).rc.refcount = 0;
            crate::memory::stdapi::ll_free(a as *mut u8);
        }
    }
}

/// One body serves both doors with the destination category
/// supplying the depth: a shared array hands back a different array
/// with the same order and the children shared, while an arena array
/// taken by a longer-lived holder is copied out and its arena COW
/// children with it. The replay copies live entries only, so the
/// append cursor is carried rather than derived — a hole under the
/// highest key spent leaves no witness in the copy.
mod the_two_cow_doors {
    use super::*;

    /// The COW door. A shared array asked to separate must hand back a
    /// **different** array; returning the original is a write into a value
    /// two holders share, which in release happens with no signal at all.
    #[test]
    fn a_shared_array_separates_into_a_copy_of_its_own() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let mut ctx = crate::memory::context::LLContext { arena: &mut arena };

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let key = mk(b"k");
        let value = mk(b"v");
        unsafe {
            // `insert` writes the entry raw and leaves the counting to the
            // caller, so these are the source array's own references — and
            // they are taken first, because an entry a walker can reach
            // must already be backed by a count.
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(value as *mut RcHeader);
            crate::array::testing::insert(
                src,
                Key::Str(key),
                Value::entity(crate::value::Tag::String, value as *mut RcHeader),
            );
        }

        // A second holder is what makes the write a separation.
        unsafe { crate::refcount::ll_retain(src as *mut RcHeader) };

        let copy = unsafe {
            crate::object::ll_cow_separate(&mut ctx, MemoryCategory::GcHeap, src as *mut RcHeader)
        } as *mut LLArray;
        assert_ne!(copy, src, "the shared array was written in place");
        assert_eq!(
            unsafe { crate::array::testing::used(copy) },
            1,
            "the entry did not survive"
        );
        // Three each: this test, the source array, and the copy.
        assert_eq!(
            unsafe { (*(key as *mut RcHeader)).refcount },
            3,
            "the copy did not take a reference to the key"
        );
        assert_eq!(
            unsafe { (*(value as *mut RcHeader)).refcount },
            3,
            "the copy did not take a reference to the element"
        );

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(!ll_release(src as *mut RcHeader));
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
            assert!(ll_release(key as *mut RcHeader));
            crate::object::ll_entity_die(key as *mut RcHeader);
            assert!(ll_release(value as *mut RcHeader));
            crate::object::ll_entity_die(value as *mut RcHeader);
        }

        arena.reset(|_| {});
    }

    /// The rule reads the category before the count: a heap array at
    /// count 1 is exclusively owned and writes in place.
    #[test]
    fn separation_is_needed_only_when_the_array_is_shared() {
        let _g = crate::memory::block_pool::test_guard();
        let a = arr();
        unsafe {
            assert!(!(*a).needs_separation(), "count 1 writes in place");
            crate::refcount::ll_retain(a as *mut RcHeader);
            assert!((*a).needs_separation(), "a second holder forces a copy");
            crate::refcount::ll_release(a as *mut RcHeader);
            crate::array::entity::dispose_storage(a, category_of(a));
        }
    }

    #[test]
    fn separation_copies_the_order_and_shares_the_children() {
        let _g = crate::memory::block_pool::test_guard();
        let src = arr();
        let key = mk(b"shared");
        let child = mk(b"child-value");
        unsafe {
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(child as *mut RcHeader);
            crate::array::testing::insert(src, Key::Int(1), Value::int(10));
            crate::array::testing::insert(
                src,
                Key::Str(key),
                Value::entity(crate::value::Tag::String, child as *mut RcHeader),
            );
            crate::array::testing::insert(src, Key::Int(2), Value::int(20));
        }

        let before_key = unsafe { (*(key as *mut RcHeader)).refcount };
        let before_child = unsafe { (*(child as *mut RcHeader)).refcount };

        let dst = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                std::ptr::null_mut(),
                CopyReason::Duplication,
            )
        };

        assert!(!dst.is_null());

        unsafe {
            // Order survives.
            let order: Vec<i64> = crate::array::testing::iter(dst)
                .map(|e| {
                    if e.is_int_key() {
                        e.hash_or_key as i64
                    } else {
                        -1
                    }
                })
                .collect();
            assert_eq!(order, vec![1, -1, 2]);

            // The children are shared, each counted once more.
            assert_eq!((*(key as *mut RcHeader)).refcount, before_key + 1);
            assert_eq!((*(child as *mut RcHeader)).refcount, before_child + 1);

            // Writing the copy does not touch the source.
            crate::array::testing::insert(dst, Key::Int(1), Value::int(999));
            assert_eq!(
                crate::array::testing::get(src, Key::Int(1))
                    .unwrap()
                    .as_int(),
                10
            );
            assert_eq!(
                crate::array::testing::get(dst, Key::Int(1))
                    .unwrap()
                    .as_int(),
                999
            );

            release_children(dst);
            crate::array::entity::dispose_storage(dst, category_of(dst));
            release_children(src);
            crate::array::entity::dispose_storage(src, category_of(src));
        }
    }

    #[test]
    fn separation_carries_holes_away_rather_than_copying_them() {
        let _g = crate::memory::block_pool::test_guard();
        let src = arr();
        unsafe {
            for i in 0..10i64 {
                crate::array::testing::insert(src, Key::Int(i), Value::int(i));
            }

            for i in [2i64, 5, 8] {
                let _ = crate::array::testing::remove(src, Key::Int(i));
            }

            let dst = separate(
                src,
                MemoryCategory::GcHeap,
                std::ptr::null_mut(),
                CopyReason::Duplication,
            );
            assert!(!dst.is_null());
            assert_eq!(crate::array::testing::table(dst).len(), 7);
            assert_eq!(
                crate::array::testing::used(dst),
                7,
                "the copy starts dense: a hole is not worth copying"
            );
            let order: Vec<i64> = crate::array::testing::iter(dst)
                .map(|e| e.hash_or_key as i64)
                .collect();
            assert_eq!(order, vec![0, 1, 3, 4, 6, 7, 9]);

            crate::array::entity::dispose_storage(dst, category_of(dst));
            crate::array::entity::dispose_storage(src, category_of(src));
        }
    }

    /// The escape door. An arena array taken by a longer-lived holder is
    /// copied out, and its arena COW children are copied with it — a hold
    /// on arena memory in a heap slot dangles at the reset.
    #[test]
    fn an_arena_array_taken_by_a_heap_holder_is_copied_out_with_its_children() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        // `ll_array_new` takes no context and resolves this thread's, so
        // an arena array needs one mounted. One raw pointer, reused: a
        // fresh `&mut` per call retags and invalidates what TLS holds
        // (`dev/WORKFLOW.md`, Miri).
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let holder_class = crate::class::ClassBuilder::new("ArrayHolder")
            .prop("a", true)
            .build();
        let holder = unsafe {
            crate::object::new_constructed(context_ptr, holder_class, MemoryCategory::GcHeap)
        };

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let element =
            unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"in the arena") };
        unsafe {
            crate::refcount::ll_retain(element as *mut RcHeader);
            crate::array::testing::insert(
                src,
                Key::Int(1),
                Value::entity(crate::value::Tag::String, element as *mut RcHeader),
            );
        }

        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                holder as *mut RcHeader,
                crate::object::Object::prop_at(holder, 16),
                std::ptr::null_mut(),
                Value::entity(crate::value::Tag::Array, src as *mut RcHeader),
            ));
        }

        let stored =
            unsafe { (*crate::object::Object::prop_at(holder, 16)).entity_ptr() } as *mut LLArray;
        assert_ne!(stored, src, "the heap slot took the arena array itself");
        assert_eq!(
            unsafe { (*stored).rc.memory_category() },
            MemoryCategory::GcHeap,
            "the copy did not land in the heap"
        );
        let copied_element =
            unsafe { crate::array::testing::entry(stored, 0).value().entity_ptr() };
        assert_ne!(
            copied_element, element as *mut RcHeader,
            "the copy still holds the arena string"
        );
        assert_eq!(
            unsafe { crate::object::header_category(copied_element) },
            MemoryCategory::GcHeap,
            "the copied element did not leave the arena"
        );

        unsafe {
            assert!(ll_release(holder as *mut RcHeader));
            crate::object::ll_object_die(holder);
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// The append cursor survives the copy, where the replay cannot
    /// carry it: `fill_from` copies live entries only, so a hole under
    /// the highest key ever inserted has no witness in the copy — PHP
    /// appends `[9 => 'x']` minus its 9 at 10, and a copy that answered
    /// 0 would hand back keys the source already spent.
    #[test]
    fn a_copy_inherits_the_append_cursor_over_a_hole() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

        let src = arr();
        unsafe {
            crate::array::testing::insert(src, Key::Int(9), Value::int(1));
            let _ = crate::array::testing::remove(src, Key::Int(9));
            assert_eq!(crate::array::testing::table(src).append_key(), Some(10));
        }

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };

        assert!(!copy.is_null());
        unsafe {
            assert_eq!(
                crate::array::testing::table(copy).len(),
                0,
                "a hole is not worth copying"
            );
            assert_eq!(
                crate::array::testing::table(copy).append_key(),
                Some(10),
                "the copy rewound the append cursor past a removed key"
            );
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
        }
    }
}

/// A copy takes the source's rung before its first insert, the mode
/// deciding how a key is hashed and a table that adopts it later
/// having already indexed its entries the other way. All three
/// states carry: escalated stays escalated, a drawn salt is
/// inherited rather than redrawn, and an unsalted copy stays a full
/// citizen of the ladder.
mod the_flood_state_a_copy_inherits {
    use super::*;

    /// A copy of an attacked table is attacked. The mode is one-way on
    /// the source and `$b = $a` is the ordinary thing the language does,
    /// so a copy that starts weak hands the attacker an unescalated table
    /// whenever they want one.
    ///
    /// **The colliding set is removed before the copy, and that is the
    /// point.** While the whole set is still in the table the copy
    /// re-escalates on its own — the equal-hash trigger fires again on
    /// the ninth collider it re-inserts — so a copy made then proves
    /// nothing about carrying the state. `unset` is what makes the loss
    /// permanent: below the trigger's threshold nothing re-fires, and the
    /// table is back to the hash the attacker already knows, ready for
    /// the same flood again.
    ///
    /// Seen failing on `is_strong` for the copy.
    #[test]
    fn a_copy_of_an_escalated_table_is_escalated() {
        use crate::array::table::EQUAL_HASH_LIMIT;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

        let src = arr();
        let colliders: Vec<*mut LLString> = (0..EQUAL_HASH_LIMIT as usize + 4)
            .map(|i| {
                let s = mk(format!("collider-{i}").as_bytes());
                // Forged rather than found: constructing a set of equal
                // full hashes needs a break of the hash, and the code
                // path the attack reaches is this one.
                unsafe { (*s).hash = 0x0BAD_C0DE_0BAD_C0DE };
                unsafe {
                    crate::refcount::ll_retain(s as *mut RcHeader);
                    crate::array::testing::insert(src, Key::Str(s), Value::int(i as i64));
                }

                s
            })
            .collect();
        assert!(
            unsafe { crate::array::testing::table(src).is_strong() },
            "the forged set did not escalate the source, so this proves nothing"
        );

        // Leave one collider behind: far below the trigger, so nothing in
        // the copy can re-fire it.
        for s in &colliders[1..] {
            // `remove` hands the stored key back with the value — the
            // table's one reference per stored key — so the table's
            // reference is released through what came back and the
            // creation reference through the test's own pointer.
            let (_, key) = unsafe { crate::array::testing::remove(src, Key::Str(*s)) }.unwrap();
            assert_eq!(key, *s, "the entry held the inserted key entity");
            unsafe {
                assert!(!ll_release(key as *mut RcHeader), "the table's");
                assert!(ll_release(*s as *mut RcHeader), "and the test's");
                crate::object::ll_entity_die(*s as *mut RcHeader);
            }
        }

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };

        assert!(!copy.is_null());
        assert!(
            unsafe { crate::array::testing::table(copy).is_strong() },
            "the copy came back to the hash the attacker already knows"
        );
        assert_eq!(
            unsafe { crate::array::testing::get(copy, Key::Str(colliders[0])) }
                .unwrap()
                .as_int(),
            0,
            "a key was lost by the copy's own hashing"
        );

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
            assert!(ll_release(colliders[0] as *mut RcHeader));
            crate::object::ll_entity_die(colliders[0] as *mut RcHeader);
        }
    }

    /// A copy of a table whose salt the first rung drew indexes exactly
    /// as the source does. The bit without the number would mean
    /// `mix_int(k, 0)` — a mix every attacker computes offline — and a
    /// fresh draw would break the ladder's bound: a copy's second long
    /// chain must escalate, not rebuild again. Seen failing on the salt
    /// equality.
    #[test]
    fn a_copy_of_a_reseeded_table_inherits_the_drawn_salt() {
        use crate::array::table::CHAIN_LIMIT;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

        let src = arr();
        for i in 0..(CHAIN_LIMIT as i64 + 1) {
            unsafe {
                crate::array::testing::insert(src, Key::Int(i * 1024), Value::int(i));
            }
        }

        assert!(
            unsafe { crate::array::testing::table(src).is_reseeded() },
            "the stride flood did not fire the rung, so this proves nothing"
        );
        let drawn = unsafe { crate::array::testing::table(src).salt() };

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };

        assert!(!copy.is_null());
        assert!(unsafe { crate::array::testing::table(copy).is_reseeded() });
        assert_eq!(
            unsafe { crate::array::testing::table(copy).salt() },
            drawn,
            "the copy indexes under a salt of its own"
        );
        for i in 0..(CHAIN_LIMIT as i64 + 1) {
            assert_eq!(
                unsafe { crate::array::testing::get(copy, Key::Int(i * 1024)) }
                    .unwrap()
                    .as_int(),
                i
            );
        }

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
        }
    }

    /// The third state a copy can inherit: nothing. A copy of an
    /// unsalted source starts unsalted — by-value integer indexing, no
    /// mix — and stays a full citizen of the ladder: its own flood fires
    /// its own first rung, drawing a salt of its own.
    #[test]
    fn a_copy_of_an_unsalted_table_is_unsalted_and_climbs_its_own_ladder() {
        use crate::array::table::CHAIN_LIMIT;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;

        let src = arr();
        for i in 0..3i64 {
            unsafe {
                crate::array::testing::insert(src, Key::Int(i * 1024), Value::int(i));
            }
        }

        assert!(unsafe { !crate::array::testing::table(src).is_reseeded() });

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };

        assert!(!copy.is_null());
        assert!(
            unsafe { !crate::array::testing::table(copy).is_reseeded() },
            "a copy of an unsalted table drew a salt from nowhere"
        );

        for i in 3..(CHAIN_LIMIT as i64 + 1) {
            unsafe {
                crate::array::testing::insert(copy, Key::Int(i * 1024), Value::int(i));
            }
        }

        assert!(
            unsafe { crate::array::testing::table(copy).is_reseeded() },
            "the copy's own flood must fire the copy's own rung"
        );
        for i in 0..(CHAIN_LIMIT as i64 + 1) {
            assert_eq!(
                unsafe { crate::array::testing::get(copy, Key::Int(i * 1024)) }
                    .unwrap()
                    .as_int(),
                i
            );
        }

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert!(ll_release(src as *mut RcHeader));
            crate::object::ll_entity_die(src as *mut RcHeader);
        }
    }
}

/// The kind switch is the only door a bare entity pointer has, and
/// its Array arm walks elements and string keys once each, returns
/// the storage, and tears down a child the array held last rather
/// than only decrementing it: `ll_release`'s report is an
/// obligation, and dropping it leaves everything that child holds
/// unreachable.
mod what_a_death_gives_back {
    use super::*;

    #[test]
    fn releasing_children_walks_elements_and_string_keys_once_each() {
        let _g = crate::memory::block_pool::test_guard();
        let a = arr();
        let key = mk(b"k");
        let child = mk(b"v");
        unsafe {
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(child as *mut RcHeader);
            crate::array::testing::insert(
                a,
                Key::Str(key),
                Value::entity(crate::value::Tag::String, child as *mut RcHeader),
            );
            let k0 = (*(key as *mut RcHeader)).refcount;
            let c0 = (*(child as *mut RcHeader)).refcount;

            release_children(a);
            assert_eq!((*(key as *mut RcHeader)).refcount, k0 - 1);
            assert_eq!((*(child as *mut RcHeader)).refcount, c0 - 1);

            crate::array::entity::dispose_storage(a, category_of(a));
        }
    }

    /// Death through the kind switch, which is the only door a bare
    /// entity pointer has. Before the Array arm existed this reached a
    /// `debug_assert!(false)` and, in release, did nothing at all: the
    /// children kept the references the array owed them and the storage
    /// was never returned.
    #[test]
    fn dying_through_the_kind_switch_releases_the_children_and_the_storage() {
        use crate::memory::block_pool::{BLOCK_KIND_BUFFER, BLOCK_MASK};
        use crate::memory::buffer::{PressureMode, set_pressure_mode};
        use crate::memory::buffer_arena::with_buffer_arena;
        use crate::refcount::{ll_release, ll_retain};
        let _g = crate::memory::block_pool::test_guard();

        let a = arr();
        let key = mk(b"key");
        let value = mk(b"value");
        unsafe {
            // One reference each for the array, one for this test, so the
            // children outlive the array and can be read afterwards.
            ll_retain(key as *mut RcHeader);
            ll_retain(value as *mut RcHeader);
            crate::array::testing::insert(
                a,
                Key::Str(key),
                Value::entity(crate::value::Tag::String, value as *mut RcHeader),
            );
            let (storage, capacity) = crate::array::testing::storage_and_capacity(a);
            assert!(
                !storage.is_null(),
                "the insert allocated storage to release"
            );

            assert!(
                ll_release(a as *mut RcHeader),
                "the array was the last holder"
            );
            crate::object::ll_entity_die(a as *mut RcHeader);

            assert_eq!(
                (*(key as *mut RcHeader)).refcount,
                1,
                "the key's reference was not let go"
            );
            assert_eq!(
                (*(value as *mut RcHeader)).refcount,
                1,
                "the element's reference was not let go"
            );
            // The storage came back: in critical mode an allocation
            // searches the block's free list, so the same address
            // returning means teardown really disposed of the table
            // rather than only dropping the entity.
            let kind = *(((storage as usize) & !BLOCK_MASK) as *const u32);
            assert_eq!(
                kind, BLOCK_KIND_BUFFER,
                "the storage was not a buffer chunk"
            );
            set_pressure_mode(PressureMode::Critical);
            let (reused, _) = with_buffer_arena(|arena| arena.alloc(capacity));
            set_pressure_mode(PressureMode::Plenty);
            assert_eq!(reused, storage, "teardown left the storage unreturned");
            with_buffer_arena(|arena| arena.free(reused, capacity));

            // Released first: a slot freed while its header still reads
            // refcount 1 is enumerated as a live entity by every later
            // process-global walk (`PLAN.md`, the census flake).
            assert!(ll_release(key as *mut RcHeader));
            crate::object::ll_entity_die(key as *mut RcHeader);
            assert!(ll_release(value as *mut RcHeader));
            crate::object::ll_entity_die(value as *mut RcHeader);
        }
    }

    /// A child the array was the last holder of has to be torn down, not
    /// merely decremented. `ll_release` reports the death and the report
    /// is an obligation: dropping it leaves the child's own memory — and
    /// everything *it* holds — unreachable and unfreed. Observed through a
    /// nested array, whose storage is a buffer chunk that can be seen
    /// coming back.
    #[test]
    fn a_child_the_array_held_last_is_torn_down_and_not_only_released() {
        use crate::memory::buffer::{PressureMode, set_pressure_mode};
        use crate::memory::buffer_arena::with_buffer_arena;
        use crate::refcount::ll_release;
        let _g = crate::memory::block_pool::test_guard();

        let outer = arr();
        let inner = arr();
        unsafe {
            crate::array::testing::insert(inner, Key::Int(1), Value::int(1));
            let (storage, capacity) = crate::array::testing::storage_and_capacity(inner);
            assert!(!storage.is_null(), "the inner array has storage to reclaim");

            // The inner array's only reference is the outer array's
            // element, so the outer's death is the inner's death.
            crate::array::testing::insert(
                outer,
                Key::Int(0),
                Value::entity(crate::value::Tag::Array, inner as *mut RcHeader),
            );

            assert!(ll_release(outer as *mut RcHeader));
            crate::object::ll_entity_die(outer as *mut RcHeader);

            // Both tables are freed by this teardown, the outer one
            // first: the drain disposes a level before it takes the next
            // one off the list. Which of the two the free list hands back
            // first is the allocator's business, so the assertion takes
            // either — what it is here to catch is the inner storage
            // never coming back at all.
            set_pressure_mode(PressureMode::Critical);
            let first = with_buffer_arena(|arena| arena.alloc(capacity));
            let second = with_buffer_arena(|arena| arena.alloc(capacity));
            set_pressure_mode(PressureMode::Plenty);
            assert!(
                first.0 == storage || second.0 == storage,
                "the inner array was released but never torn down: its storage never came back"
            );
            with_buffer_arena(|arena| {
                arena.free(first.0, first.1);
                arena.free(second.0, second.1);
            });
        }
    }
}

/// Depth is the caller's input, so neither the copy nor the teardown
/// recurses: both drain a list held in a buffer-arena chunk, and each
/// runs against a 64 KiB stack, where a frame set per level ends on
/// the guard page with no unwinding and no record — the teardown at
/// 2 000 levels, the copy at 800, where it also pins that every level
/// left the arena. What separates them is what can be shown. The
/// teardown's list is forced to refuse, and a list that cannot grow
/// drops each child it could not take onto the recursive path, keeping
/// the outcome and losing only the bound; a copy whose list refuses
/// refuses the copy, so there is no recursive arm to force and its
/// bound stays arithmetic.
mod nesting_worked_through_a_list {
    use super::*;

    /// The deep copy's two halves, together because the pair is the
    /// measurement: the depth runs on a thread the spawn sizes, and the
    /// body that builds it is a function of its own, so a second copy of
    /// either number would drift from this one silently.
    const DEEP_COPY_DEPTH: usize = 800;
    const DEEP_COPY_STACK: usize = 64 * 1024;

    /// Nesting is worked through the list, so the copy of a deep arena
    /// array touches one stack frame per *call*, not one per level. The
    /// depth is well past `WorkList::FIRST`, so the chunk grows more than
    /// once on the way through.
    ///
    /// **On a stack that cannot hold the alternative**, which is what
    /// makes the depth mean anything: 64 KiB against 800 levels leaves
    /// 82 bytes a level, and the smallest frame set here is far above
    /// that. The ordinary 8 MiB stack holds a recursive copy at this
    /// depth, so the small stack is what makes the depth mean anything
    /// rather than the depth itself.
    ///
    /// Unlike the teardown below it, the bound is arithmetic rather than
    /// exhibited: a teardown has no channel to refuse through, so
    /// forcing its list to refuse returns it to the recursive path and
    /// kills the thread, while a copy whose list refuses **refuses the
    /// copy** — there is no recursive arm here to fall back to and none
    /// to force.
    #[test]
    fn a_deep_arena_array_is_copied_out_through_the_work_list() {
        std::thread::Builder::new()
            .stack_size(DEEP_COPY_STACK)
            .spawn(deep_copy_body)
            .expect("the probe thread")
            .join()
            .expect("the deep copy ran out of machine stack");
    }

    /// The body of the test above, on its own small stack.
    fn deep_copy_body() {
        const DEPTH: usize = DEEP_COPY_DEPTH;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        // `[[[…]]]`, built from the inside out: each array holds the one
        // below it under key 0, and the source stays entirely in the
        // arena, where COW children are shared rather than copied.
        let mut levels = Vec::with_capacity(DEPTH);
        let innermost = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        levels.push(innermost);
        for _ in 1..DEPTH {
            let outer = unsafe { ll_array_new(MemoryCategory::RequestArena) };
            let inner = *levels.last().unwrap();
            unsafe {
                crate::refcount::ll_retain(inner as *mut RcHeader);
                crate::array::testing::insert(
                    outer,
                    Key::Int(0),
                    Value::entity(crate::value::Tag::Array, inner as *mut RcHeader),
                );
            }

            levels.push(outer);
        }

        let src = *levels.last().unwrap();

        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };

        assert!(!copy.is_null(), "the deep copy was refused");

        // Every level is a heap array of its own, and none of them is the
        // arena array it was copied from.
        let mut level = copy;
        for depth in 0..DEPTH {
            assert_eq!(
                unsafe { (*level).rc.memory_category() },
                MemoryCategory::GcHeap,
                "level {depth} did not leave the arena"
            );
            assert_ne!(
                level,
                levels[DEPTH - 1 - depth],
                "level {depth} is the source array itself"
            );
            let entry = unsafe { crate::array::testing::get(level, Key::Int(0)) };
            if depth == DEPTH - 1 {
                assert!(entry.is_none(), "the innermost copy holds something");
                break;
            }

            level = entry
                .expect("the copy is shallower than its source")
                .entity_ptr() as *mut LLArray;
        }

        unsafe {
            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// Teardown of a deep array is a drain, not a recursion. The depth is
    /// the caller's input — `$deep = [[[…]]]`, then one release — and a
    /// frame set per level overflows the stack, which no arm of this
    /// crate can catch: the guard page kills the process with no
    /// unwinding and no record.
    ///
    /// Measured on a thread whose stack is deliberately small, because a
    /// depth that overflows the ordinary 8 MiB one costs minutes to build
    /// and dominates the Miri run. 64 KiB against 2 000 levels leaves
    /// under 33 bytes a level, and the smallest frame set here is far
    /// above that; what the drain spends is a fixed frame and one list
    /// entry, so the margin is total rather than per level. Verified by
    /// forcing the list to refuse (`FORCE_REFUSE_LONGLIVED`), which
    /// returns the teardown to the recursive path: the thread dies on the
    /// guard page at this depth.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "2000 levels of build and teardown is minutes under Miri, which would\
                  dominate the whole-suite run; the drain's UB surface is covered by the\
                  destructor-order tests and the deep-copy test, which Miri does run"
    )]
    fn a_deep_array_tears_down_without_the_machine_stack() {
        const DEPTH: usize = 2_000;
        const STACK: usize = 64 * 1024;

        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(|| {
                let _g = crate::memory::block_pool::test_guard();

                // `[[[…]]]`, built from the inside out and iteratively:
                // each level holds the one below it under key 0, so the
                // outermost is the chain's only holder. The creation
                // reference `ll_array_new` returns is what the entry
                // takes, so no level is retained twice and none is
                // released here.
                let mut level = unsafe { ll_array_new(MemoryCategory::GcHeap) };
                for _ in 1..DEPTH {
                    let outer = unsafe { ll_array_new(MemoryCategory::GcHeap) };
                    unsafe {
                        crate::array::testing::insert(
                            outer,
                            Key::Int(0),
                            Value::entity(crate::value::Tag::Array, level as *mut RcHeader),
                        );
                    }

                    level = outer;
                }

                // The chain is the thing under test, so its depth is
                // asserted rather than assumed: a refused insert would
                // leave a shallow array that tears down on any stack.
                let mut built = 1;
                let mut walk = level;
                while let Some(v) = unsafe { crate::array::testing::get(walk, Key::Int(0)) } {
                    walk = v.entity_ptr() as *mut LLArray;
                    built += 1;
                }

                assert_eq!(built, DEPTH, "the chain is shallower than it was built");

                unsafe {
                    assert!(
                        ll_release(level as *mut RcHeader),
                        "the outermost array had a second holder"
                    );
                    crate::object::ll_entity_die(level as *mut RcHeader);
                }
            })
            .expect("the small-stack thread did not start")
            .join()
            .expect("teardown of the deep array killed its thread");
    }

    /// The list grows past its first chunk and hands pairs back in
    /// reverse, which is all the copy asks of it. Growth copies what was
    /// already there — losing it would drop whole subtrees of a deep copy
    /// without any assertion firing.
    #[test]
    fn the_work_list_grows_and_keeps_what_it_held() {
        let _g = crate::memory::block_pool::test_guard();
        type Pairs = WorkList<(*mut LLArray, *mut LLArray)>;
        let n = Pairs::FIRST * 3;
        let mut list = Pairs::new();
        let pair = |i: usize| (i as *mut LLArray, (i + 1000) as *mut LLArray);
        for i in 0..n {
            assert!(list.push(pair(i)), "the list refused at {i}");
        }

        assert!(list.capacity >= n);
        for i in (0..n).rev() {
            assert_eq!(list.pop(), Some(pair(i)));
        }

        assert_eq!(list.pop(), None);
        list.dispose();
    }

    /// The refusal path, which is the exhaustion path: a list that cannot
    /// grow drops each child it could not take onto the recursive one.
    /// What must survive that is the outcome — everything is torn down,
    /// in Zend's order — and what does not is the bound on depth, which
    /// is why this shape is three levels rather than two thousand.
    ///
    /// It was untestable until `FORCE_REFUSE_LONGLIVED` existed for the
    /// refused carry (S4.2); the plan recorded it as owed on the strength
    /// of `FORCE_OOM` alone, which the buffer arena can go around.
    #[test]
    fn a_refused_list_still_tears_everything_down_in_order() {
        assert_eq!(destructor_order_with(Shape::Mixed, true), "12345");
    }
}

/// Zend's order, which a program observes because a `__destruct`
/// writes a log or closes a handle: depth first, and inside a level
/// the order the entries were inserted in. The drain reverses the
/// segment it is holding rather than the whole list, which is what
/// the mixed shape separates.
mod the_order_destructors_run_in {
    use super::*;

    /// The order `__destruct` bodies run in when a nested array dies is
    /// Zend's: depth first, and inside a level the order the entries were
    /// inserted in. The drain has to reproduce it, because a program
    /// observes it — a destructor writes a log, closes a handle, or reads
    /// another object that is about to die.
    ///
    /// `[[$b], $a]`: `$b` is one level down and first in the entry order,
    /// so it goes first. Seen failing as `AB` on the drain's first shape,
    /// which released `$a` where it found it and left the nested array
    /// for the pop.
    #[test]
    fn a_nested_destructor_runs_before_a_later_sibling() {
        assert_eq!(destructor_order(Shape::NestedThenObject), "BA");
    }

    /// Two nested arrays, so the reversal of the held segment is what is
    /// under test rather than the interleaving: `[[$b], [$c]]` runs `$b`
    /// before `$c`. Seen failing as `CB`, the LIFO order of the pushes.
    #[test]
    fn nested_siblings_run_their_destructors_in_entry_order() {
        assert_eq!(destructor_order(Shape::TwoNested), "BC");
    }

    /// The mixed case both of the above are corners of:
    /// `[$1, [$2, [$3], $4], $5]` runs `1 2 3 4 5`. It exercises a held
    /// segment inside a held segment, which is where a reversal that
    /// reversed the whole list rather than the segment would show.
    #[test]
    fn a_mixed_nesting_runs_its_destructors_in_zend_order() {
        assert_eq!(destructor_order(Shape::Mixed), "12345");
    }
}

/// Storing a key consumes the caller's reference and removing one
/// hands it back, while an overwrite keeps the entry's original key,
/// so the caller's stays the caller's. In an arena table the reset
/// log owes a heap key's release, so the giveback goes through
/// `drop_ref` rather than a bare `ll_release`, which would let the
/// reset drive a string somebody still holds to death — and a child
/// the copy counted as escaping has to lose that hold-count the same
/// way.
mod who_owns_a_key_reference {
    use super::*;

    /// S2.2's ownership rule, both arms measured. Storing a new string
    /// key consumes the caller's reference; the overwrite arm keeps the
    /// entry's original key, so the caller's reference stays the
    /// caller's; removing hands the stored key's reference back. Two
    /// distinct entities with equal bytes, because one entity can
    /// measure only one arm: the stored key catches the remove leak, the
    /// overwriting key catches the stranded retain.
    #[test]
    fn a_stored_key_is_consumed_and_a_dropped_key_comes_back() {
        let _g = crate::memory::block_pool::test_guard();
        let a = mk(b"key");
        let b = mk(b"key");
        assert_ne!(a, b, "two distinct entities, or neither arm is measured");
        let e = arr();
        let a0 = unsafe { (*a).rc.refcount };
        let b0 = unsafe { (*b).rc.refcount };

        unsafe {
            crate::refcount::ll_retain(a as *mut RcHeader);
            let (added, old) =
                crate::array::testing::insert(e, Key::Str(a), Value::int(1)).unwrap();
            assert!(added, "the first insert stores a new key");
            assert!(old.is_none());

            crate::refcount::ll_retain(b as *mut RcHeader);
            let (added, old) =
                crate::array::testing::insert(e, Key::Str(b), Value::int(2)).unwrap();
            assert!(!added, "equal bytes overwrite rather than add");
            assert_eq!(old.unwrap().as_int(), 1);
            // `added == false`: the caller's key was not stored, so the
            // retain above is still the caller's to give back.
            assert!(!ll_release(b as *mut RcHeader));

            let (v, key) = crate::array::testing::remove(e, Key::Str(b)).unwrap();
            assert_eq!(v.as_int(), 2);
            assert_eq!(key, a, "the entry kept its original key entity");
            assert!(!ll_release(key as *mut RcHeader), "the table's reference");

            assert_eq!((*a).rc.refcount, a0, "the stored key's references balance");
            assert_eq!(
                (*b).rc.refcount,
                b0,
                "the overwriting key's references balance"
            );

            assert!(ll_release(e as *mut RcHeader));
            crate::object::ll_entity_die(e as *mut RcHeader);
            assert!(ll_release(a as *mut RcHeader));
            crate::object::ll_entity_die(a as *mut RcHeader);
            assert!(ll_release(b as *mut RcHeader));
            crate::object::ll_entity_die(b as *mut RcHeader);
        }
    }

    /// The ownership rule's cross-category half: in an arena table a
    /// heap key's one release is owed by the reset log — the barrier
    /// records it at publication — so the caller gives the returned key
    /// up through `drop_ref`, which leaves log-owned references alone.
    /// A bare `ll_release` there is the double free `Table::remove`'s
    /// contract names: the reset's own release then drives the string to
    /// death while the test still holds it. Seen failing exactly that
    /// way.
    #[test]
    fn an_arena_tables_key_release_is_owed_by_the_reset_log() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let key = mk(b"heap key in an arena table");
        let e = unsafe { ll_array_new(MemoryCategory::RequestArena) };

        unsafe {
            crate::refcount::ll_retain(key as *mut RcHeader);
            let published = crate::memory::barrier::store_category_barrier(
                arena_ptr,
                MemoryCategory::RequestArena,
                key as *mut RcHeader,
            );
            assert_eq!(
                published, key as *mut RcHeader,
                "a heap entity entering an arena slot is logged, not copied"
            );
            let (added, old) =
                crate::array::testing::insert(e, Key::Str(key), Value::int(1)).unwrap();
            assert!(added);
            assert!(old.is_none());

            let (v, k) = crate::array::testing::remove(e, Key::Str(key)).unwrap();
            assert_eq!(v.as_int(), 1);
            assert_eq!(k, key);
            // The table's reference is the log's to release at reset;
            // `drop_ref` knows that where a bare release would not.
            crate::memory::barrier::drop_ref(MemoryCategory::RequestArena, k as *mut RcHeader);
            assert_eq!(
                (*key).rc.refcount,
                2,
                "the log still holds its one reference"
            );
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});

        unsafe {
            assert_eq!(
                (*key).rc.refcount,
                1,
                "the reset's one release balanced the barrier's one record"
            );
            assert!(ll_release(key as *mut RcHeader));
            crate::object::ll_entity_die(key as *mut RcHeader);
        }
    }

    /// A refusal mid-copy gives a published child back through the
    /// barrier, and the difference from a bare release is an escape
    /// hold-count: the copy's barrier counted the non-COW arena child as
    /// escaping into a heap destination, so the giveback must
    /// `escape_lose` it — a bare `ll_release` no-ops on an arena entity
    /// and leaves the count stuck, and the reset then treats a child
    /// nobody holds as an escapee. Seen failing on the escapee flag.
    #[test]
    fn a_refused_heap_copy_gives_an_escaped_child_back_through_the_barrier() {
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = crate::memory::arena::Arena::new();
        let arena_ptr: *mut crate::memory::arena::Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        // Warm a heap entity block, so the forced refusal below lands on
        // the copy's table storage and not on the copy's own slot.
        let warm = arr();

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let d = unsafe {
            crate::string::ll_string_new_dynamic(context_ptr, MemoryCategory::RequestArena, b"p", 0)
        };

        assert!(!d.is_null());
        unsafe {
            crate::refcount::ll_retain(d as *mut RcHeader);
            crate::array::testing::insert(
                src,
                Key::Int(0),
                Value::entity(crate::value::Tag::String, d as *mut RcHeader),
            );
            crate::refcount::ll_retain(src as *mut RcHeader);
        }

        FORCE_OOM.store(true, Ordering::Relaxed);
        let copy = unsafe {
            separate(
                src,
                MemoryCategory::GcHeap,
                arena_ptr,
                CopyReason::Duplication,
            )
        };

        FORCE_OOM.store(false, Ordering::Relaxed);
        assert!(
            copy.is_null(),
            "the copy was meant to be refused and was not"
        );

        unsafe {
            assert_eq!(
                crate::refcount::header_flags(d as *const RcHeader) & crate::refcount::IS_ESCAPEE,
                0,
                "the refused copy left the child counted as an escapee"
            );
            crate::refcount::ll_release(src as *mut RcHeader);
            assert!(ll_release(warm as *mut RcHeader));
            crate::object::ll_entity_die(warm as *mut RcHeader);
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }
}
