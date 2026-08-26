//! Interpolated string template: the runtime half of `rfc/model/strings.md`,
//! "Rule 3: the shape of the template object, and how it flattens".
//!
//! A template preserves the seam between what the programmer wrote and
//! what was substituted, so a consumer that cares — a query builder, a
//! logger — can read the two apart instead of receiving one flat string
//! it has to parse back.
//!
//! **The parts are the site's, the values are the evaluation's**:
//! [`TemplateShape`] carries the literals, the instance carries only the
//! values.
//!
//! **The instance is an ordinary entity**: `RcHeader | class | shape |
//! Value[n]`, an object whose body is a shape pointer and the values. The
//! count is read from the shape in exactly **one** place:
//! [`crate::object::for_each_counted_cell`], which both cell readers and
//! the sever go through. A layout known in one place cannot be learned by
//! three walkers and missed by a fourth.
//!
//! **Flattening is the rare path** (rule 2: an object exists only where
//! the destination declared the interface), so it is one shared routine
//! over the shape rather than a generated method per site.
//!
//! Not here, and deliberately: the C ABI a foreign consumer would read
//! the structure through. There is no such consumer until the compiler
//! exists, and a signature invented now would be a guess.

use crate::class::{CLASS_TEMPLATE, Class};
use crate::memory::context::{LLContext, resolve_arena};
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
/// plain load (`crate::cells::CellReader`).
///
/// The **shape word** is the instance's and goes through the reader; the
/// count inside the shape does not, the shape being static data no
/// mutator writes. Chasing that word is safe for the concurrent reader
/// for the same reason the class word is: the entity is mature, so the
/// store that published it was ordered long before the read.
///
/// # Safety
/// `base` is a live template instance, and under a relaxed reader its
/// cells may be concurrently written.
#[inline]
pub(crate) unsafe fn value_count_at<R: crate::cells::CellReader>(base: *const u8) -> usize {
    let shape = unsafe { R::ptr(base.add(16)) } as *const TemplateShape;
    unsafe { (*shape).value_count as usize }
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
        unsafe { Class::flags_of(class) } & CLASS_TEMPLATE != 0,
        "a template instance needs the template class"
    );

    let size = VALUES_OFFSET + n * size_of::<Value>();
    let mem = unsafe { crate::memory::routing::entity_alloc_in(ctx, category, size) };
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
            // And the memory itself, which no walker has seen: the header
            // is unpublished, so the slot reads dead and the free is the
            // ordinary one. Abandoning it was survivable while a refused
            // entity was at most a size class; a template of five
            // thousand values takes a block-aligned run of its own, and
            // that one would keep a registry entry every collection
            // walks for the life of the process.
            unsafe { crate::memory::stdapi::ll_free(mem) };
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

    let reserved = unsafe { crate::string::new_uninit(ctx, category, total) };
    if reserved.is_null() {
        return std::ptr::null_mut();
    }

    // Pass 2: write. One destination, filled once, in order.
    let start = reserved.bytes();
    let mut dst = start;
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
        dst as usize - start as usize,
        total,
        "the two passes disagreed about the length"
    );
    unsafe { crate::string::publish_uninit(reserved, category) }
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
mod tests;
