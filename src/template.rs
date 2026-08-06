//! Interpolated string template: the runtime half of
//! `rfc/model/strings.md`, rule 3.
//!
//! A template preserves the seam between what the programmer wrote and
//! what was substituted, so a consumer that cares — a query builder, a
//! logger — can read the two apart instead of receiving one flat string
//! it has to parse back. Parts and values alternate, part first and part
//! last, so there is always exactly one more part than there are values
//! and no offset map is needed to say which is which.
//!
//! **The parts are the site's, the values are the evaluation's.** The
//! literal fragments are compile-time constants: they live in a
//! [`TemplateShape`] the compiler emits once per interpolation site and
//! never frees, as interned immortal strings that no refcount and no
//! collector has to touch. What differs from pass to pass is the values,
//! and only they live in the instance.
//!
//! **The instance is an ordinary entity**: `RcHeader | class | shape |
//! Value[n]`, an object whose body is a shape pointer and the values.
//! One class serves every site — the site's identity is the shape, not a
//! generated class — which is why the body's length is a property of the
//! instance rather than of the class, and why the count is read from the
//! shape in exactly **one** place: [`crate::object::for_each_counted_cell`],
//! which both cell readers share and which the sever goes through as well
//! as the two tracers. A layout known in one place cannot be learned by
//! three walkers and missed by a fourth, which is what happened here.
//!
//! **Flattening is the rare path** (rule 2: an object exists only where
//! the destination declared the interface), so it is one shared routine
//! over the shape rather than a generated method per site. Two passes,
//! as in Zend's rope: measure everything, allocate once, write each piece
//! into place — no temporary string per value.
//!
//! Not here, and deliberately: the C ABI a foreign consumer would read
//! the structure through. There is no such consumer until the compiler
//! exists, and a signature invented now would be a guess
//! (`model/PLAN.md`, task 9).

use crate::class::{CLASS_TEMPLATE, Class};
use crate::memory::context::{LLContext, resolve_arena};
use crate::memory::immortal::immortal_alloc;
use crate::refcount::{EntityKind, MemoryCategory, RcHeader, publish_header};
use crate::string::LLString;
use crate::value::{Tag, Value};

/// The per-site half of a template: what the programmer typed, known at
/// compile time and shared by every pass through that site.
///
/// Emitted by the compiler as static data and never freed — which is
/// what lets the parts be plain immortal strings with no count kept on
/// them. `parts` has `value_count + 1` entries, and an empty part is
/// ordinary: `"$a$b"` is three parts, the first and last empty.
#[repr(C)]
pub struct TemplateShape {
    pub value_count: u32,
    pub parts: *const *const LLString,
}

impl TemplateShape {
    /// The literal fragments, `value_count + 1` of them.
    ///
    /// # Safety
    /// `parts` points to that many live immortal strings, as the
    /// compiler emitted them.
    pub unsafe fn parts<'a>(&self) -> &'a [*const LLString] {
        unsafe { std::slice::from_raw_parts(self.parts, self.value_count as usize + 1) }
    }
}

/// A template instance: the shape it was built from, and the values that
/// pass substituted. Header and class word sit where every entity's do,
/// so the ordinary machinery reads it as the object it is.
#[repr(C)]
pub struct Template {
    pub rc: RcHeader,
    pub class: *const Class,
    pub shape: *const TemplateShape,
    // Value[shape.value_count] follows at +24.
}

/// Byte offset of the first value, past the header, class word and shape
/// pointer.
pub(crate) const VALUES_OFFSET: usize = size_of::<Template>();

/// The values a template instance holds.
///
/// # Safety
/// `t` is a live template instance.
pub(crate) unsafe fn values<'a>(t: *const Template) -> &'a [Value] {
    let n = unsafe { (*(*t).shape).value_count } as usize;
    unsafe { std::slice::from_raw_parts((t as *const u8).add(VALUES_OFFSET) as *const Value, n) }
}

/// How many values the instance at `base` holds, for a walker that must
/// read the instance's own memory through its reader rather than by a
/// plain load (`crate::walk::CellReader`).
///
/// The **shape word** is the instance's and goes through the reader; the
/// count inside the shape does not, the shape being static data no
/// mutator writes. Chasing that word is safe for the concurrent reader
/// for the same reason the class word is: the entity is mature, so the
/// store that published it was ordered by a handshake epochs ago.
///
/// # Safety
/// `base` is a live template instance, and under a relaxed reader its
/// cells may be concurrently written.
#[inline]
pub(crate) unsafe fn value_count_at<R: crate::walk::CellReader>(base: *const u8) -> usize {
    let shape = unsafe { R::ptr(base.add(16)) } as *const TemplateShape;
    unsafe { (*shape).value_count as usize }
}

/// The values of the instance based at `base`, for a caller that has the
/// address and the class but no typed pointer — the teardown walkers.
///
/// # Safety
/// `base` is a live template instance and `cls` is its class.
pub(crate) unsafe fn values_at<'a>(base: *mut u8, cls: &Class) -> &'a mut [Value] {
    debug_assert!(cls.flags & CLASS_TEMPLATE != 0);
    let n = unsafe { (*(base as *const Template)).shape };
    let n = unsafe { (*n).value_count } as usize;
    unsafe { std::slice::from_raw_parts_mut(base.add(VALUES_OFFSET) as *mut Value, n) }
}

/// Build a template instance in `category` from `shape` and the values
/// this pass substituted.
///
/// The instance comes back at **+1**, owned by the caller, and takes
/// **its own reference** to every value that carries one: the caller
/// keeps what it had. Null when the allocation fails, and
/// null when a value could not be published into an instance of this
/// category — an arena COW value taken by a longer-lived template is
/// copied, and that copy is an allocation that can fail
/// (`rfc/model/values.md`). Nothing is half-built either way: the values
/// already stored are released before the null is returned.
///
/// # Safety
/// `class` is the template class ([`CLASS_TEMPLATE`]), `shape` outlives
/// the instance, `values` holds exactly `shape.value_count` live values,
/// and `ctx` is per [`crate::memory::context::ll_arena_alloc`].
pub unsafe fn ll_template_new(
    ctx: *mut LLContext,
    class: *const Class,
    shape: *const TemplateShape,
    values: &[Value],
    category: MemoryCategory,
) -> *mut Template {
    let n = unsafe { (*shape).value_count } as usize;
    debug_assert_eq!(values.len(), n, "a pass substitutes one value per hole");
    debug_assert!(
        unsafe { (*class).flags } & CLASS_TEMPLATE != 0,
        "a template instance needs the template class"
    );

    let size = VALUES_OFFSET + n * size_of::<Value>();
    let mem = match category {
        MemoryCategory::RequestArena => unsafe { (*resolve_arena(ctx)).alloc(size) },
        MemoryCategory::GcHeap | MemoryCategory::LongLived => unsafe {
            crate::memory::heap::entity_alloc(size)
        },
        MemoryCategory::Immortal => immortal_alloc(size),
    };
    if mem.is_null() {
        return std::ptr::null_mut();
    }

    let t = mem as *mut Template;
    unsafe {
        // An all-zero Value is `null`, which is what the slots must read
        // as if the loop below stops early.
        std::ptr::write_bytes(
            mem.add(size_of::<RcHeader>()),
            0,
            size - size_of::<RcHeader>(),
        );
        (&raw mut (*t).class).write(class);
        (&raw mut (*t).shape).write(shape);
    }

    // The barrier takes the owner's category as a parameter and never
    // reads it from the owner, so the values go in before the header does
    // — and they must, so that a refusal abandons memory no walker has
    // seen. The header is published last, as every other factory here
    // publishes it.
    let arena = if category == MemoryCategory::RequestArena {
        resolve_arena(ctx)
    } else {
        std::ptr::null_mut()
    };
    let slots = unsafe { std::slice::from_raw_parts_mut(mem.add(VALUES_OFFSET) as *mut Value, n) };
    for (i, v) in values.iter().enumerate() {
        let stored =
            unsafe { crate::memory::barrier::store_box(arena, category, &mut slots[i], *v) };
        if !stored {
            unsafe { release_stored(category, slots, i) };
            return std::ptr::null_mut();
        }
    }

    unsafe {
        publish_header(
            t as *mut RcHeader,
            RcHeader::new(category, EntityKind::Object.to_flags()),
        );
    }
    t
}

/// Give back what a refused build had already taken, through the same
/// micro-op that teardown uses — the inverse of the store is `drop_ref`
/// and not a bare release: an arena escapee's hold-count has to come back
/// down, and a value the barrier copied out of the arena has to die when
/// its only reference goes.
///
/// The slots are not cleared: the instance was never published, so
/// nothing will ever read them.
///
/// # Safety
/// `slots[..i]` are the values stored so far by [`ll_template_new`] into
/// an instance of category `owner_cat` that was not published.
unsafe fn release_stored(owner_cat: MemoryCategory, slots: &mut [Value], i: usize) {
    for slot in &mut slots[..i] {
        if slot.is_refcounted() {
            unsafe { crate::memory::barrier::drop_ref(owner_cat, slot.entity_ptr()) };
        }
    }
}

/// Flatten a template into one string in `category`: every part and every
/// value in order. The result comes back at **+1**, owned by the caller,
/// like every other factory here.
///
/// Null when a value's text cannot be produced yet, and null when the
/// allocation fails; the template is unchanged either way, and may be
/// flattened again. **A value is not consumed** — flattening reads.
///
/// What "cannot be produced yet" covers is stated once, in
/// [`text_len`]: a float, and anything whose text is user code.
///
/// # Safety
/// `t` is a live template instance and `ctx` is per
/// [`crate::memory::context::ll_arena_alloc`].
pub unsafe fn flatten(
    ctx: *mut LLContext,
    t: *const Template,
    category: MemoryCategory,
) -> *mut LLString {
    let shape = unsafe { &*(*t).shape };
    let parts = unsafe { shape.parts() };
    let values = unsafe { values(t) };

    // Pass 1: measure. Nothing is allocated until every length is known,
    // which is also where a value that cannot be rendered stops the whole
    // flattening rather than half of it.
    let mut total = 0usize;
    for part in parts {
        total += unsafe { LLString::bytes(*part) }.len();
    }
    for v in values {
        match text_len(v) {
            Some(len) => total += len,
            None => return std::ptr::null_mut(),
        }
    }

    let mem = unsafe { crate::string::new_uninit(ctx, category, total) };
    if mem.is_null() {
        return std::ptr::null_mut();
    }

    // Pass 2: write. One destination, filled once, in order.
    let mut dst = unsafe { mem.add(size_of::<LLString>()) };
    for (i, part) in parts.iter().enumerate() {
        let bytes = unsafe { LLString::bytes(*part) };
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            dst = dst.add(bytes.len());
        }
        if let Some(v) = values.get(i) {
            dst = unsafe { write_text(v, dst) };
        }
    }
    debug_assert_eq!(
        dst as usize - (mem as usize + size_of::<LLString>()),
        total,
        "the two passes disagreed about the length"
    );
    unsafe { crate::string::publish_uninit(mem, category) }
}

/// Bytes `v` will occupy in a flattened result, or `None` when its text
/// cannot be produced.
///
/// Produced here: a string is its own bytes, an integer its decimal
/// digits, and the three PHP constants their conversions — `true` is
/// `"1"`, `false` and `null` are empty.
///
/// Not produced, and each for its own missing piece. A float needs the
/// language's precision rules, which are not decided. An object needs
/// `__toString`, which is user code the crate has no way to call yet —
/// rule 3 requires every such call to complete in this pass, before the
/// result is allocated, so the call site belongs exactly here once that
/// mechanism exists. An array, resource or reference has no string form
/// worth guessing at.
fn text_len(v: &Value) -> Option<usize> {
    match v.tag() {
        Tag::Null | Tag::False => Some(0),
        Tag::True => Some(1),
        Tag::Int => Some(decimal_len(v.as_int())),
        Tag::String => {
            Some(unsafe { crate::string::string_bytes(v.entity_ptr() as *const LLString) }.len())
        }
        _ => None,
    }
}

/// Write `v`'s text at `dst` and return the position after it. Writes
/// exactly what [`text_len`] measured.
///
/// # Safety
/// `dst` has room for [`text_len`] of `v`, which the caller measured, and
/// `v` is one of the tags that measured.
unsafe fn write_text(v: &Value, dst: *mut u8) -> *mut u8 {
    match v.tag() {
        Tag::Null | Tag::False => dst,
        Tag::True => unsafe {
            dst.write(b'1');
            dst.add(1)
        },
        Tag::Int => unsafe { write_decimal(v.as_int(), dst) },
        Tag::String => unsafe {
            let bytes = crate::string::string_bytes(v.entity_ptr() as *const LLString);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            dst.add(bytes.len())
        },
        // Unreachable by contract: `flatten` refuses before it allocates.
        _ => dst,
    }
}

/// Digits in `n`'s decimal form, minus sign included.
fn decimal_len(n: i64) -> usize {
    let mut len = usize::from(n < 0);
    let mut rest = n.unsigned_abs();
    loop {
        len += 1;
        rest /= 10;
        if rest == 0 {
            return len;
        }
    }
}

/// Write `n` in decimal at `dst` and return the position after it.
///
/// Digits come out least-significant first, so they are produced into a
/// stack buffer and copied back — twenty digits is the most an `i64`
/// needs, and the minus sign one more.
///
/// # Safety
/// `dst` has room for [`decimal_len`] of `n`.
unsafe fn write_decimal(n: i64, dst: *mut u8) -> *mut u8 {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    let mut rest = n.unsigned_abs();
    loop {
        i -= 1;
        digits[i] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    unsafe {
        let mut at = dst;
        if n < 0 {
            at.write(b'-');
            at = at.add(1);
        }
        let tail = &digits[i..];
        std::ptr::copy_nonoverlapping(tail.as_ptr(), at, tail.len());
        at.add(tail.len())
    }
}

#[cfg(test)]
mod tests {
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

    /// Parts and values alternate, part first and part last, and the
    /// three PHP constants convert as PHP converts them.
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
