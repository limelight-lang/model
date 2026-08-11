use super::*;
use crate::class::ClassBuilder;
use crate::intern::intern_str;
use crate::memory::arena::Arena;
use crate::refcount::{ll_release, ll_retain};
use crate::string::ll_string_new;

fn with_ctx<R>(f: impl FnOnce(*mut LLContext) -> R) -> R {
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let r = f(&mut ctx);
    arena.reset(|_| {});
    r
}

/// One site's static half: `"id = $id, name = $name!"` is three parts
/// around two holes.
fn shape_of(parts: &[&str]) -> Box<TemplateShape> {
    let interned: Vec<*const LLString> = parts.iter().map(|p| intern_str(p)).collect();
    Box::new(TemplateShape {
        value_count: (parts.len() - 1) as u32,
        parts: Box::leak(interned.into_boxed_slice()).as_ptr(),
    })
}

/// Parts and values alternate, part first and part last, which is
/// what lets an empty part be ordinary and needs no offset map. An
/// integer and `true` convert as PHP converts them, and `false` and
/// null render as empty text rather than being refused the way a float
/// and an object are.
mod what_flattening_produces {
    use super::*;

    /// Parts and values alternate, part first and part last, and an
    /// integer and `true` convert as PHP converts them.
    #[test]
    fn flattening_alternates_parts_and_values() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["id = ", ", name = ", ", ok = ", "!"]);

        with_ctx(|ctx| {
            let name =
                unsafe { ll_string_new(ctx, MemoryCategory::RequestArena, "édouard".as_bytes()) };
            let held = [
                Value::int(-42),
                Value::entity(Tag::String, name as *mut RcHeader),
                Value::bool(true),
            ];
            let t =
                unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
            let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
            assert!(!out.is_null());
            assert_eq!(
                unsafe { crate::string::string_bytes(out) },
                "id = -42, name = édouard, ok = 1!".as_bytes()
            );
        });
    }

    /// An empty part is ordinary — `"$a$b"` is three parts, two of them
    /// empty — and it is what makes the alternation need no offset map.
    #[test]
    fn empty_parts_are_ordinary() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["", "", ""]);

        with_ctx(|ctx| {
            let held = [Value::int(7), Value::int(8)];
            let t =
                unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
            let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
            assert_eq!(unsafe { crate::string::string_bytes(out) }, b"78");
        });
    }

    /// `false` is empty text in PHP, and empty is a length the measuring
    /// pass answers rather than a value it declines: a `false` dropped
    /// from that arm falls to the refusal a float and an object take, and
    /// the whole flattening reports null. The parts around the hole are
    /// there to give the emptiness somewhere to show — they do not
    /// separate a hole measured at zero from one skipped in both passes,
    /// which no output can.
    #[test]
    fn false_is_empty_text_between_its_parts() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["<", ">"]);

        with_ctx(|ctx| {
            let held = [Value::bool(false)];
            let t =
                unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
            let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
            assert!(!out.is_null(), "`false` is rendered, not refused");
            assert_eq!(unsafe { crate::string::string_bytes(out) }, b"<>");
        });
    }

    /// Null the same, and separately: the two share one arm today, and a
    /// single test over both would go on passing if one of them left it.
    #[test]
    fn null_is_empty_text_between_its_parts() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["<", ">"]);

        with_ctx(|ctx| {
            let held = [Value::null()];
            let t =
                unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
            let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
            assert!(!out.is_null(), "null is rendered, not refused");
            assert_eq!(unsafe { crate::string::string_bytes(out) }, b"<>");
        });
    }

    /// The one integer whose absolute value does not fit its own type.
    #[test]
    fn the_smallest_integer_writes_all_of_itself() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["", ""]);

        with_ctx(|ctx| {
            let held = [Value::int(i64::MIN)];
            let t =
                unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
            let out = unsafe { flatten(ctx, t, MemoryCategory::RequestArena) };
            assert_eq!(
                unsafe { crate::string::string_bytes(out) },
                b"-9223372036854775808"
            );
        });
    }
}

/// Everything is measured before anything is allocated, so a value
/// whose text the crate cannot yet produce stops the whole
/// flattening rather than leaving a partial result. The factory
/// assembles in place, which makes it a second maker of the layout
/// choice `ll_string_new` makes when it copies, and a result in a
/// category another thread can reach is hashed before publication
/// because two threads would race to fill the lazy field.
mod the_string_the_flattening_allocates {
    use super::*;

    /// A result past what the category packs in one slot takes the
    /// out-of-line layout: this is the assemble-in-place factory's half
    /// of the choice `ll_string_new` makes when it copies, and the two
    /// write to different places, so it needs its own test.
    #[test]
    fn a_flattened_result_past_the_slot_limit_is_out_of_line() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["head:", ":tail"]);

        with_ctx(|ctx| {
            let long = vec![b'v'; crate::memory::heap::MAX_SMALL];
            let value = unsafe { ll_string_new(ctx, MemoryCategory::GcHeap, &long) };
            let held = [Value::entity(Tag::String, value as *mut RcHeader)];
            let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::GcHeap) };
            let out = unsafe { flatten(ctx, t, MemoryCategory::GcHeap) };
            assert!(!out.is_null());
            assert_ne!(
                unsafe { crate::refcount::header_flags(out as *const RcHeader) }
                    & crate::refcount::STRING_OUT_OF_LINE,
                0,
                "the assembled result did not fit one slot"
            );

            let mut want = b"head:".to_vec();
            want.extend_from_slice(&long);
            want.extend_from_slice(b":tail");
            assert_eq!(unsafe { crate::string::string_bytes(out) }, &want[..]);

            unsafe {
                assert!(ll_release(out as *mut RcHeader));
                crate::object::ll_entity_die(out as *mut RcHeader);
                assert!(ll_release(t as *mut RcHeader));
                crate::object::ll_entity_die(t as *mut RcHeader);
                assert!(ll_release(value as *mut RcHeader));
                crate::object::ll_entity_die(value as *mut RcHeader);
            }
        });
    }

    /// A value whose text needs machinery the crate does not have stops
    /// the whole flattening, before anything is allocated — a partial
    /// result would be a wrong string rather than a missing one.
    #[test]
    fn a_value_with_no_text_yet_refuses_the_whole_flattening() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let plain = ClassBuilder::new("Plain").build();
        let shape = shape_of(&["v = ", ""]);

        with_ctx(|ctx| {
            let obj =
                unsafe { crate::object::ll_object_new(ctx, plain, MemoryCategory::RequestArena) };
            let held = [Value::entity(Tag::Object, obj as *mut RcHeader)];
            let t =
                unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
            assert!(
                unsafe { flatten(ctx, t, MemoryCategory::RequestArena) }.is_null(),
                "an object needs __toString, which is user code with no call path yet"
            );

            let float = shape_of(&["", ""]);
            let held = [Value::float(1.5)];
            let t2 =
                unsafe { ll_template_new(ctx, cls, &*float, &held, MemoryCategory::RequestArena) };
            assert!(
                unsafe { flatten(ctx, t2, MemoryCategory::RequestArena) }.is_null(),
                "a float needs the language's precision rules, which are undecided"
            );
        });
    }

    /// A result in a category another thread can reach cannot carry the
    /// lazy hash — two threads would race to fill one field — so it is
    /// hashed before it is published, and by content.
    #[test]
    fn a_shared_result_is_hashed_before_it_is_published() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["a", "b"]);

        with_ctx(|ctx| {
            let held = [Value::int(1)];
            let t =
                unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::RequestArena) };
            let out = unsafe { flatten(ctx, t, MemoryCategory::LongLived) };
            assert_eq!(unsafe { crate::string::string_bytes(out) }, b"a1b");
            assert_eq!(
                unsafe { (*out).hash },
                crate::hash::hash_bytes(b"a1b"),
                "a shared string must arrive already hashed"
            );
        });
    }
}

/// The value count is the instance's rather than the class's, so
/// every walker reads it from the shape: the read-only walk finds
/// the values, the death releases them, and the drain's sever
/// reaches them by lvalue, which the walk cannot do.
mod the_instance_as_an_ordinary_entity {
    use super::*;

    /// The values are the instance's, and the collector reaches them
    /// through the shape — the class has no runs to reach them by, and one
    /// class serves every site, so a class-driven walk would find nothing.
    #[test]
    fn the_walker_sees_a_templates_values() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["id = ", ""]);

        with_ctx(|ctx| {
            let s = unsafe { ll_string_new(ctx, MemoryCategory::GcHeap, b"abc") };
            let held = [Value::entity(Tag::String, s as *mut RcHeader)];
            // GcHeap on both sides, so teardown is what gives the
            // reference back: an arena template's heap child is released
            // by the arena's release-at-reset record instead, and that
            // path says nothing about this walk.
            let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::GcHeap) };
            assert!(!t.is_null());
            assert_eq!(
                unsafe { (*(s as *mut RcHeader)).refcount },
                2,
                "the template took its own reference, the caller kept its own"
            );

            let mut seen = Vec::new();
            unsafe {
                crate::object::for_each_counted_child(t as *mut crate::object::Object, |c| {
                    seen.push(c)
                })
            };

            assert_eq!(seen, vec![s as *mut RcHeader], "the value is the one child");

            // Teardown gives that reference back.
            // Release reports the death; tearing down is the caller's,
            // exactly as the store barrier's `drop` does it.
            assert!(
                unsafe { ll_release(t as *mut RcHeader) },
                "the template dies"
            );
            unsafe { crate::object::ll_entity_die(t as *mut RcHeader) };
            assert_eq!(unsafe { (*(s as *mut RcHeader)).refcount }, 1);
            unsafe { ll_release(s as *mut RcHeader) };
        });
    }

    /// The instance is an ordinary entity, so a reference held in it is
    /// released when it dies — and the retain/release pair below is the
    /// whole ownership contract of the factory.
    #[test]
    fn a_dying_template_releases_what_it_held() {
        let _g = crate::memory::block_pool::test_guard();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["", ""]);

        with_ctx(|ctx| {
            let s = unsafe { ll_string_new(ctx, MemoryCategory::GcHeap, b"held") };
            unsafe { ll_retain(s as *mut RcHeader) };
            let held = [Value::entity(Tag::String, s as *mut RcHeader)];
            let t = unsafe { ll_template_new(ctx, cls, &*shape, &held, MemoryCategory::GcHeap) };
            assert_eq!(unsafe { (*(s as *mut RcHeader)).refcount }, 3);

            assert!(
                unsafe { ll_release(t as *mut RcHeader) },
                "the last reference to the template is its own death"
            );
            unsafe { crate::object::ll_entity_die(t as *mut RcHeader) };
            assert_eq!(
                unsafe { (*(s as *mut RcHeader)).refcount },
                2,
                "the template's own reference went back"
            );
            unsafe {
                ll_release(s as *mut RcHeader);
                ll_release(s as *mut RcHeader);
            }
        });
    }

    /// A ring that runs through a template is garbage like any other, and
    /// the drain's walker is a second place that has to find a template's
    /// values: it severs them by lvalue, which the read-only walk cannot
    /// do.
    #[test]
    fn a_ring_through_a_template_is_collected() {
        let _g = crate::memory::block_pool::test_guard();
        let holder_class = ClassBuilder::new("Holder").prop("t", true).build();
        let cls = ClassBuilder::new("InterpolatedString").template().build();
        let shape = shape_of(&["", ""]);

        let mut arena = Arena::new();
        let mut ctx = LLContext { arena: &mut arena };
        let holder = unsafe {
            crate::object::new_constructed(&mut ctx, holder_class, MemoryCategory::GcHeap)
        };

        let held = [Value::entity(Tag::Object, holder as *mut RcHeader)];
        let t = unsafe { ll_template_new(&mut ctx, cls, &*shape, &held, MemoryCategory::GcHeap) };
        // Close the ring: the holder takes the template, the template
        // already holds the holder.
        unsafe {
            crate::object::Object::prop_at(holder, 16)
                .write(Value::entity(Tag::Object, t as *mut RcHeader));
            crate::refcount::ll_retain(t as *mut RcHeader);
            ll_release(t as *mut RcHeader);
            // Every count in the ring must now be a heap edge and nothing
            // else — a test's local pointer is not a root here, so the
            // creation reference has to go or the ring reads as live.
            ll_release(holder as *mut RcHeader);
        }

        unsafe { crate::walk::collect_cycles() };
        let mut alive = Vec::new();
        unsafe { crate::memory::heap::for_each_entity_slot(|e| alive.push(e as usize)) };
        assert!(
            !alive.contains(&(t as usize)) && !alive.contains(&(holder as usize)),
            "a ring through a template outlived a whole-heap collection"
        );
        arena.reset(|_| {});
    }
}
