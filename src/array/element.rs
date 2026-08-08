//! The generic element layer over the table (`PLAN.md`, stage S2).
//!
//! The five element operations are here — `get`, `set`, `append`,
//! `unset` and `make_ref`. Every write goes through one composition,
//! `write_through`, and every operation starts by settling what the key
//! *is*, which is why the key constructor lives here too. Canonicalisation
//! sits above `Table` rather than inside it because `Map` is the table's
//! second customer and keys a map exactly: a table that canonicalised
//! would be unusable there.

use crate::array::entity::{LLArray, give_value_back};
use crate::array::table::{Key, Table};
use crate::memory::context::LLContext;
use crate::refcount::{MemoryCategory, RcHeader};
use crate::string::LLString;
use crate::value::{Tag, Value};

/// The separation composition every write here goes through: separate
/// the holder's array if it is shared, run `write` on the array the
/// write must reach, publish the result, and settle the three
/// references.
///
/// `slot` names the array (`Tag::Array`); `owner_cat` is the holder's
/// category, a compiler parameter as at every store-side barrier. On a
/// shared array the copy is filled privately and published only then, in
/// this order: `store_box` writes the copy into `slot`, `ll_release`
/// spends the copy's creation reference, and `drop_ref` takes this
/// holder off the displaced original. The last two are in that order and
/// not in `string::separate`'s, because `drop_ref` runs `__destruct`
/// bodies and one of them can displace the copy from this very slot; the
/// creation reference is therefore spent while no user code can run
/// (`dev/DECISIONS.md`, 2026-08-08). Held in one function, the "copy
/// left at two separates forever" trap is unreachable from any call
/// site — which is why [`set`], [`append`] and [`unset`] differ only in
/// what they pass as `write`.
///
/// **`false` reports a refusal with the arrays unchanged**: the slot
/// still names the original at its old count, every table holds its old
/// entries, and every reference the caller brought is still the
/// caller's. A copy refused part-way dies whole
/// ([`destroy_private_copy`]).
///
/// **The displaced original ends at one holder — up to the reset log.**
/// A heap array displaced from an *arena* holder's slot is owed its
/// release by the log, not by this `drop_ref`, so its count stays high
/// until the reset and a later write through another holder separates
/// once more: conservative — a count above the holders can only copy
/// more, never corrupt — and inherent to log ownership rather than a
/// defect here.
///
/// # Safety
/// `ctx` per `ll_arena_alloc`; `slot` a live slot of a live holder of
/// category `owner_cat`, holding a live array. `write` gets an array no
/// other holder can see once a copy was made, and reports whether it
/// wrote.
unsafe fn write_through(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    slot: *mut Value,
    write: impl FnOnce(*mut LLArray, *mut crate::memory::arena::Arena) -> bool,
) -> bool {
    // The tag, not `is_refcounted`: a ReferenceBox passes the flag test,
    // and `ll_cow_separate` has no arm for that kind — in release it
    // hands the box back, and the table write below would then lay an
    // `LLArray` over an `LLReference`'s `Value`.
    debug_assert!(
        matches!(unsafe { (*slot).tag() }, Tag::Array),
        "the slot names an array"
    );
    let current = unsafe { (*slot).entity_ptr() } as *mut LLArray;
    let arena = crate::memory::context::resolve_arena(ctx);
    let separated =
        unsafe { crate::object::ll_cow_separate(ctx, owner_cat, current as *mut RcHeader) }
            as *mut LLArray;
    if separated.is_null() {
        return false;
    }
    if separated == current {
        return write(current, arena);
    }
    // The copy is private until published, so a refusal from here on
    // destroys it whole and leaves the slot and the original untouched.
    // `store_box` is not one of the operations' named refusals: it
    // refuses only when it must copy an arena COW entity out for a
    // longer-lived holder, and `separation_category` puts the copy in the
    // arena exactly when `owner_cat` is the arena. Tested rather than
    // assumed, because the day that mapping changes, an ignored `false`
    // leaves the slot naming the original while the copy holds every
    // child.
    if !write(separated, arena)
        || !unsafe {
            crate::memory::barrier::store_box(
                arena,
                owner_cat,
                slot,
                Value::entity(Tag::Array, separated as *mut RcHeader),
            )
        }
    {
        unsafe { destroy_private_copy(separated) };
        return false;
    }
    unsafe {
        // The creation reference goes first, while no user code can run:
        // the slot's reference keeps the copy at one, so the release
        // below cannot be a death. `drop_ref` of the displaced original
        // runs `__destruct`s, and one of them may displace the copy from
        // the slot — with the creation reference already spent, that
        // nested displacement is the copy's ordinary death rather than a
        // count stranded at two (Critic, S2.5 round 1).
        let died = crate::refcount::ll_release(separated as *mut RcHeader);
        debug_assert!(!died, "the slot's reference must outlive the creation one");
        crate::memory::barrier::drop_ref(owner_cat, current as *mut RcHeader);
    }
    true
}

/// The element store: `$a[k] = v` through the holder's slot.
///
/// **Three refusals report `false`**, each an allocation no reserve
/// funds: the separation's copy, the publication of an arena COW value
/// or key into a longer-lived array (`escape_copy`, inside
/// `store_category_barrier`), and the table's growth. All three leave
/// every array unchanged, per [`write_through`]. One state does move on
/// a refused insert: a long chain draws the salt before growth is asked
/// to allocate and rung state is one-way, so the flood ladder may have
/// advanced.
///
/// **No caller reference is consumed.** The operation takes references
/// of its own for what the array keeps: the value's entity is retained
/// and published through `store_category_barrier` (which may hand back
/// a copy — an arena COW value crossing into a longer-lived array), and
/// a string key follows S2.2's rule — consumed by a new entry, given
/// back through `drop_ref` when the overwrite arm kept the entry's
/// original key. The displaced element goes back the same way.
///
/// `key` is already canonical (`canonical_key`); a debug build asserts
/// that rather than re-canonicalising on every store.
///
/// # Safety
/// Per [`write_through`], plus: `value`'s entity, if any, live and
/// **holding a reference independent of `slot`** — a subscript RHS in
/// PHP is a temporary with a reference of its own, so `$a[0] = $a`
/// reaches here at count 2 and separates. Handed the slot's own array at
/// count 1, this would build `$a[0] === $a`, a cycle PHP's value
/// semantics cannot produce.
pub unsafe fn set(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    slot: *mut Value,
    key: Key,
    value: Value,
) -> bool {
    #[cfg(debug_assertions)]
    if let Key::Str(s) = key {
        debug_assert!(
            matches!(unsafe { canonical_key(s) }, Key::Str(_)),
            "the layer canonicalises before it stores"
        );
    }
    // A reference-tagged value would mean two different things for one
    // call: the element's own box on a fresh key, and a box written into
    // the existing box on a live one. `$a[k] = &$v` is the rebinding
    // operation, which is not this one and does not exist yet.
    debug_assert!(
        value.tag() != Tag::Reference,
        "the caller dereferences the right-hand side"
    );
    unsafe {
        write_through(ctx, owner_cat, slot, |a, arena| {
            store_into(a, arena, key, value)
        })
    }
}

/// The append: `$a[] = v` under the table's own cursor, which is the
/// highest integer key ever inserted plus one (`Table::append_key`).
///
/// **The cursor is read before the separation, and that is not a
/// shortcut**: a copy adopts the source's cursor
/// (`Table::adopt_append_state`), so both arrays answer with the same
/// key, and reading first means an exhausted cursor refuses without
/// paying for a copy first.
///
/// `false` for [`set`]'s three refusals, and for a fourth that is not an
/// allocation: `i64::MAX` has been a key, so no successor exists and the
/// append refuses rather than wrapping onto a live entry (`PLAN.md`
/// S2.4). Every array is unchanged either way.
///
/// # Safety
/// Per [`set`], with `value` in place of the key-and-value pair.
pub unsafe fn append(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    slot: *mut Value,
    value: Value,
) -> bool {
    debug_assert!(
        matches!(unsafe { (*slot).tag() }, Tag::Array),
        "the slot names an array"
    );
    let current = unsafe { (*slot).entity_ptr() } as *mut LLArray;
    let Some(key) = (unsafe { (*current).table.append_key() }) else {
        return false;
    };
    unsafe {
        write_through(ctx, owner_cat, slot, |a, arena| {
            store_into(a, arena, Key::Int(key), value)
        })
    }
}

/// `unset($a[k])`: drop the element and give both of the table's
/// references back — the value's by the barrier, the key's by S2.2's
/// rule.
///
/// **An absent key is not an error**, and neither is it a reason to skip
/// the separation: the write barrier fires on the operation rather than
/// on the outcome, so `unset($a['nope'])` through a shared holder still
/// separates. The alternative — look the key up first — would read a
/// table this holder does not exclusively own, and would make the
/// holder's sharing state depend on the argument.
///
/// `false` reports the separation's refusal, the only one here: removal
/// allocates nothing.
///
/// # Safety
/// Per [`write_through`].
pub unsafe fn unset(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    slot: *mut Value,
    key: Key,
) -> bool {
    unsafe {
        write_through(ctx, owner_cat, slot, |a, _arena| {
            remove_from(a, key);
            true
        })
    }
}

/// `&$a[k]`: the element's `ReferenceBox`, boxing the element if it is
/// not one already, with the holder's array separated first.
///
/// **The separation comes before the boxing, and that order is the whole
/// step.** Taking a reference is a write — it turns the element into a
/// reference state every later store goes through — so it goes through
/// [`write_through`] like the other three. Boxing a shared table instead
/// would hand `$b`'s reference a box `$a` also names, and `$r = 2` would
/// then be visible through `$a['x']`. The order settles the element that
/// is **not** a box yet; an element already in a reference state comes
/// out of the separation shared, provided a second name still holds the
/// box — the separation itself unwraps a box nobody else holds
/// (`array::entity::element_for_copy`), which is PHP's own rule.
///
/// **An absent key is created as null and referenced**, which is PHP's
/// rule for `$r = &$a['nope']` and the reason this cannot simply forward
/// `Table::make_ref`'s null: that null means "absent", and the caller
/// has no way to tell it from the refusal below.
///
/// Null reports a refusal with every array unchanged: the separation's
/// copy, the box, the publication of an arena COW element into the heap
/// box (`escape_copy`, which copies the element rather than sharing it),
/// or the vivified element's own insert. One state does move behind a
/// refusal on an integer key, as it does for [`set`]: the vivified insert
/// advances the append cursor, and that is one-way.
///
/// **A fresh box comes back at one**, held by the element; an element
/// already in a reference state hands back the box it holds, at whatever
/// count its holders have given it. Either way a caller keeping the box
/// retains for its own holder.
///
/// **The box is a GC-heap entity even for an arena array**
/// (`dev/DECISIONS.md`, 2026-08-08), so boxing an element of an arena
/// array pays twice at the boundary: an arena COW element is copied to
/// the heap and an arena non-COW one counts an escape, and the entry
/// holding the heap box logs a release against the reset. Both are
/// `Table::make_ref`'s, and both are what buys an exact holder count on
/// the box.
///
/// # Safety
/// Per [`write_through`]; `key` canonical, as for [`set`].
pub unsafe fn make_ref(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    slot: *mut Value,
    key: Key,
) -> *mut crate::reference::LLReference {
    #[cfg(debug_assertions)]
    if let Key::Str(s) = key {
        debug_assert!(
            matches!(unsafe { canonical_key(s) }, Key::Str(_)),
            "the layer canonicalises before it references"
        );
    }
    let mut boxed = std::ptr::null_mut();
    let separated = unsafe {
        write_through(ctx, owner_cat, slot, |a, arena| {
            let vivified = !(*a).table.contains(key);
            if vivified && !store_into(a, arena, key, Value::null()) {
                return false;
            }
            boxed = (*a).table.make_ref(arena, a as *const RcHeader, key);
            if boxed.is_null() {
                // The vivified element goes back out. Without this a
                // refusal would leave the key present on an array this
                // call promised not to touch — and only on the
                // exclusively-owned path, where there is no private copy
                // to throw away, so the two paths would disagree about
                // what `false` means.
                if vivified {
                    remove_from(a, key);
                }
                return false;
            }
            true
        })
    };
    if separated {
        boxed
    } else {
        std::ptr::null_mut()
    }
}

/// `$a[k]` by value: the element, with a `ReferenceBox` element read
/// **through** the box, or `None` when the key is absent.
///
/// **The read never separates**, so it takes no category and no context:
/// both holders of a shared array still name it afterwards. That is
/// `values.md`'s by-value read of an element in a reference state — the
/// box is the element's identity, and a reader of `$a['x']` wants what
/// the box currently holds.
///
/// **The returned `Value` carries no reference.** A caller keeping it
/// retains for itself, as with `Table::get`, and must do so before any
/// write to this array: a later `unset` or overwrite gives the table's
/// reference back and can be the entity's last.
///
/// # Safety
/// `slot` a live slot holding a live array.
pub unsafe fn get(slot: *const Value, key: Key) -> Option<Value> {
    debug_assert!(
        matches!(unsafe { (*slot).tag() }, Tag::Array),
        "the slot names an array"
    );
    let a = unsafe { (*slot).entity_ptr() } as *const LLArray;
    let element = unsafe { (*a).table.get(key) }?;
    if element.tag() == Tag::Reference {
        let boxed = element.entity_ptr() as *const crate::reference::LLReference;
        return Some(unsafe { (*boxed).value });
    }
    Some(element)
}

/// Tear a refused, never-published copy down — children given back,
/// storage returned, the heap slot freed. `ll_entity_die` runs
/// **unconditionally** after the count is dropped, because the release
/// verdict cannot be trusted here: on an arena entity `ll_release`
/// reports no death, and a refusal branch that waited for `true` left
/// every reference the replay published — an arena COW child's count,
/// a heap child's log record's +1 — held by a corpse until the reset
/// (Critic, S2.5 round 1).
///
/// # Safety
/// `copy` is a live array at count 1 that no slot has ever named.
unsafe fn destroy_private_copy(copy: *mut LLArray) {
    unsafe {
        crate::refcount::ll_release(copy as *mut RcHeader);
        crate::object::ll_entity_die(copy as *mut RcHeader);
    }
}

/// The table half of [`set`]: publish the value and the key for `a`,
/// insert, and settle S2.2's ownership on every outcome. False on
/// refusal with everything given back and `a` unchanged — the same
/// publication idiom as `fill_from`, which is the worked example.
///
/// **An element already in a reference state is written through its box
/// instead** ([`store_through_box`]), which is what makes `$r = &$a['x']`
/// followed by `$a['x'] = 2` readable through `$r`. Because a copy shares
/// the box rather than copying it, a write through one holder of a
/// once-shared array is visible to the other — PHP's rule, and the reason
/// its manual warns about copying an array with a reference in it.
///
/// The lookup that decides this is a second chain walk on every store,
/// on top of `insert`'s own. `Table::get` hands out a copy of the Box
/// rather than a borrow, because an entry keeps its chain link in the
/// element's reserved bytes, so the walk cannot be shared with `insert`
/// without changing what `insert` returns. Unmeasured, and the array
/// performance stage in `PLAN.md` owns the number.
///
/// # Safety
/// `a` a live, exclusively owned array; `arena` the live mounted arena.
unsafe fn store_into(
    a: *mut LLArray,
    arena: *mut crate::memory::arena::Arena,
    key: Key,
    value: Value,
) -> bool {
    if let Some(element) = unsafe { (*a).table.get(key) } {
        if element.tag() == Tag::Reference {
            let boxed = element.entity_ptr() as *mut crate::reference::LLReference;
            return unsafe { store_through_box(arena, boxed, value) };
        }
    }
    let owner = a as *const RcHeader;
    let category = Table::category_of(owner);
    let mut v = value;
    if v.is_refcounted() {
        let child = v.entity_ptr();
        unsafe { crate::refcount::ll_retain(child) };
        let stored =
            unsafe { crate::memory::barrier::store_category_barrier(arena, category, child) };
        if stored.is_null() {
            unsafe { crate::refcount::ll_release(child) };
            return false;
        }
        if stored != child {
            // The barrier copied it: the copy at +1 is the array's, and
            // the retain above goes back.
            unsafe { crate::refcount::ll_release(child) };
            v = Value::entity(v.tag(), stored);
        }
    }
    let published_key = if let Key::Str(k) = key {
        let child = k as *mut RcHeader;
        unsafe { crate::refcount::ll_retain(child) };
        let stored =
            unsafe { crate::memory::barrier::store_category_barrier(arena, category, child) };
        if stored.is_null() {
            unsafe { crate::refcount::ll_release(child) };
            unsafe { give_value_back(category, &v) };
            return false;
        }
        if stored != child {
            unsafe { crate::refcount::ll_release(child) };
        }
        Key::Str(stored as *mut LLString)
    } else {
        key
    };
    match unsafe { (*a).table.insert(owner, published_key, v) } {
        None => {
            unsafe { give_value_back(category, &v) };
            if let Key::Str(k) = published_key {
                unsafe { crate::memory::barrier::drop_ref(category, k as *mut RcHeader) };
            }
            false
        }
        Some((added, displaced)) => {
            if let Some(old) = displaced {
                unsafe { give_value_back(category, &old) };
            }
            if !added {
                // S2.2: the overwrite arm kept the entry's original key,
                // so the reference published above goes back.
                if let Key::Str(k) = published_key {
                    unsafe { crate::memory::barrier::drop_ref(category, k as *mut RcHeader) };
                }
            }
            true
        }
    }
}

/// Write into an element that is in a reference state: into the box's
/// own slot, through `barrier::ref_store`.
///
/// **`ref_store` rather than a plain store**, and the box being reached
/// through an entry changes nothing about that: `6afd220` moved the
/// reference sever onto the barrier because the collector's relaxed
/// loads race a plain write into a published slot, and this box *is*
/// published — the entry names it. `Table::make_ref`'s own
/// `(*boxed).value = current` is legal only because that box is not
/// published when it runs, which makes it the pattern an implementer
/// would copy and the wrong one.
///
/// The barrier publishes before it releases, so a refusal leaves the box
/// holding exactly what it held, displaced value included, and reports
/// `false` rather than dropping a reference it did not replace.
///
/// # Safety
/// `boxed` a live reference box; `arena` the live mounted arena.
unsafe fn store_through_box(
    arena: *mut crate::memory::arena::Arena,
    boxed: *mut crate::reference::LLReference,
    value: Value,
) -> bool {
    let slot = unsafe { &raw mut (*boxed).value };
    let held = unsafe { *slot };
    let old = if held.is_refcounted() {
        held.entity_ptr()
    } else {
        std::ptr::null_mut()
    };
    unsafe { crate::memory::barrier::ref_store(arena, boxed as *mut RcHeader, slot, old, value) }
}

/// The table half of [`unset`]: remove the entry and give the table's
/// two references back. Silent on an absent key, which PHP's `unset` is
/// too.
///
/// `drop_ref` rather than a bare release for both, by S2.2's rule: the
/// arena reset log or an escape hold-count may own either reference, and
/// it absorbs the integer key's null itself.
///
/// # Safety
/// `a` a live, exclusively owned array.
unsafe fn remove_from(a: *mut LLArray, key: Key) {
    let owner = a as *const RcHeader;
    let category = Table::category_of(owner);
    if let Some((old, removed_key)) = unsafe { (*a).table.remove(key) } {
        unsafe { give_value_back(category, &old) };
        unsafe { crate::memory::barrier::drop_ref(category, removed_key as *mut RcHeader) };
    }
}

/// The key a PHP subscript denotes: an integer for the canonical
/// decimal spelling of an `i64`, the string itself for everything else.
///
/// PHP's rule, and each clause is a pinned test: the spelling is an
/// optional `-` followed by digits, with no leading zero (`"0"` is the
/// one zero), no `"-0"`, no sign `+`, no spaces, no fraction — `$a["1"]`
/// and `$a[1]` are one key while `$a["011"]`, `$a[" 1"]` and
/// `$a["1.0"]` are string keys. A spelling past the `i64` range stays a
/// string key too.
///
/// # Safety
/// `s` is a live string entity. The returned `Key::Str` borrows no
/// reference: key ownership starts where the key is stored
/// (`Table::insert`'s contract), not here.
pub unsafe fn canonical_key(s: *mut LLString) -> Key {
    match canonical_int(unsafe { LLString::bytes(s) }) {
        Some(n) => Key::Int(n),
        None => Key::Str(s),
    }
}

/// The integer whose canonical decimal spelling `bytes` is, or `None`.
fn canonical_int(bytes: &[u8]) -> Option<i64> {
    let (digits, negative) = match bytes {
        [b'-', rest @ ..] => (rest, true),
        _ => (bytes, false),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // No leading zero — "0" is the one zero — and no "-0".
    if digits[0] == b'0' && (negative || digits.len() > 1) {
        return None;
    }
    // `str::parse` refuses overflow, where a hand-rolled accumulator
    // wraps through it: `i64::MAX + 1` must stay a string key. The
    // bytes are ASCII by the digit test above, so the UTF-8 view cannot
    // fail.
    std::str::from_utf8(bytes).ok()?.parse::<i64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::entity::ll_array_new;
    use crate::array::table::Key;
    use crate::class::ClassBuilder;
    use crate::memory::arena::Arena;
    use crate::memory::block_pool::FORCE_OOM;
    use crate::memory::stdapi::ll_free;
    use crate::object::{Object, ll_object_die, new_constructed};
    use crate::refcount::{MemoryCategory, ll_release};
    use crate::string::{LLString, ll_string_new};
    use crate::value::Value;
    use std::sync::atomic::Ordering;

    fn mk(bytes: &[u8]) -> *mut LLString {
        let s = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) };
        assert!(!s.is_null());
        s
    }

    /// A table's first storage: 8 index slots and 8 entries.
    const FIRST_STORAGE_BYTES: usize = 288;

    /// What the first growth asks for: 16 index slots and 16 entries.
    const DOUBLED_STORAGE_BYTES: usize = 576;

    /// Eat every buffer-arena source that could serve `size` — warm
    /// block tails and recycled holes left by earlier tests on this
    /// thread — so the next such allocation must draw a pool block,
    /// which the already-raised `FORCE_OOM` refuses. Without this a
    /// forced refusal is a coin flip on test order: the source array's
    /// own storage warms a 64 KiB block and the copy's fits its tail.
    /// The fillers go back through the caller, after the assertion.
    unsafe fn exhaust_buffer_sources(size: usize) -> Vec<(*mut u8, usize)> {
        let mut fillers = Vec::new();
        loop {
            let (p, granted) = crate::memory::buffer_arena::buffer_alloc_longlived_payload(size);
            if p.is_null() {
                break;
            }
            fillers.push((p, granted));
        }
        fillers
    }

    fn free_fillers(fillers: Vec<(*mut u8, usize)>) {
        for (p, granted) in fillers {
            unsafe { crate::memory::buffer_arena::buffer_free_longlived_payload(p, granted) };
        }
    }

    /// Eat every free entity slot that could serve an inline string of
    /// `len` bytes, so the next such allocation must draw a pool block,
    /// which the already-raised `FORCE_OOM` refuses. The buffer-arena
    /// helper above cannot stand in for this: an entity comes from the
    /// object heap, which that one never touches.
    unsafe fn exhaust_string_entities(len: usize) -> Vec<*mut LLString> {
        let bytes = vec![b'x'; len];
        let mut fillers = Vec::new();
        loop {
            let s = unsafe { ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, &bytes) };
            if s.is_null() {
                break;
            }
            fillers.push(s);
        }
        fillers
    }

    fn free_string_fillers(fillers: Vec<*mut LLString>) {
        for s in fillers {
            free(s);
        }
    }

    /// A heap holder object with two array-slot props, both naming
    /// `src`, which therefore reads as shared: the two-`$var` setup of
    /// every criterion below, built through the real barrier.
    unsafe fn two_holders(
        ctx: *mut crate::memory::context::LLContext,
        arena: *mut Arena,
        src: *mut LLArray,
    ) -> (*mut crate::object::Object, *mut Value, *mut Value) {
        let class = ClassBuilder::new("ElementHolder")
            .prop("a", true)
            .prop("b", true)
            .build();
        let h = unsafe { new_constructed(ctx, class, MemoryCategory::GcHeap) };
        let slot_a = unsafe { Object::prop_at(h, 16) };
        let slot_b = unsafe { Object::prop_at(h, 32) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena,
                h as *mut RcHeader,
                slot_a,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            assert!(crate::memory::barrier::ref_store(
                arena,
                h as *mut RcHeader,
                slot_b,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            // The creation reference goes: the two slots are the holders.
            ll_release(src as *mut RcHeader);
        }
        (h, slot_a, slot_b)
    }

    fn free(s: *mut LLString) {
        unsafe {
            (*s).rc.refcount = 0;
            ll_free(s as *mut u8);
        }
    }

    /// The three canonical spellings of the done criterion, each finding
    /// what the integer key stored — one table, one lookup per pair.
    #[test]
    fn a_canonical_numeric_string_finds_what_the_integer_key_stored() {
        let _g = crate::memory::block_pool::test_guard();
        let a = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };
        let owner = a as *const crate::refcount::RcHeader;
        for (i, k) in [1i64, -1, i64::MAX, i64::MIN].into_iter().enumerate() {
            unsafe {
                (*a).table.insert(owner, Key::Int(k), Value::int(i as i64));
            }
        }
        for (i, spelling) in [
            &b"1"[..],
            b"-1",
            b"9223372036854775807",
            b"-9223372036854775808",
        ]
        .into_iter()
        .enumerate()
        {
            let s = mk(spelling);
            let key = unsafe { canonical_key(s) };
            assert!(
                matches!(key, Key::Int(_)),
                "{:?} must canonicalise",
                std::str::from_utf8(spelling).unwrap()
            );
            unsafe {
                assert_eq!(
                    (*a).table.get(key).unwrap().as_int(),
                    i as i64,
                    "{:?} missed the integer key's entry",
                    std::str::from_utf8(spelling).unwrap()
                );
            }
            free(s);
        }
        unsafe {
            (*a).table.dispose(owner);
            (*a).rc.refcount = 0;
            ll_free(a as *mut u8);
        }
    }

    /// The store's whole composition, measured from both holders: a
    /// store through one leaves the other's entries alone, the displaced
    /// original ends at one holder so the next store to it writes in
    /// place, the copy is held once, and the array takes a value
    /// reference of its own without consuming the caller's.
    #[test]
    fn a_store_through_one_holder_leaves_the_other_holders_entries_alone() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Int(0), Value::int(10));
        }
        let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };
        let val = mk(b"forty-one");

        assert!(unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_a,
                Key::Int(1),
                Value::entity(Tag::String, val as *mut RcHeader),
            )
        });

        unsafe {
            let copy = (*slot_a).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert_eq!(
                (*copy).table.get(Key::Int(1)).unwrap().entity_ptr(),
                val as *mut RcHeader
            );
            assert_eq!(
                (*copy).table.get(Key::Int(0)).unwrap().as_int(),
                10,
                "the copy replayed the source"
            );
            assert!(
                (*src).table.get(Key::Int(1)).is_none(),
                "the other holder's entries changed"
            );
            assert_eq!(
                (*src).rc.refcount,
                1,
                "the displaced original keeps exactly its other holder"
            );
            assert_eq!((*copy).rc.refcount, 1, "the copy is held once, by the slot");
            assert_eq!(
                (*val).rc.refcount,
                2,
                "the array takes its own reference and leaves the caller's"
            );

            // The second store goes through the other holder, whose array
            // is now at count one: in place, no second separation.
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_b,
                Key::Int(1),
                Value::int(7),
            ));
            assert_eq!(
                (*slot_b).entity_ptr() as *mut LLArray,
                src,
                "a store to the displaced original separated again"
            );
            assert_eq!((*src).table.get(Key::Int(1)).unwrap().as_int(), 7);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!(
                (*val).rc.refcount,
                1,
                "the dying array did not give the value back"
            );
            assert!(ll_release(val as *mut RcHeader));
            crate::object::ll_entity_die(val as *mut RcHeader);
        }
    }

    /// The separation's refusal: `false`, and nothing observable moved —
    /// the slot, the original's count, its entries and the caller's value
    /// reference all read as before the call.
    #[test]
    fn a_refused_separation_reports_and_changes_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Int(0), Value::int(10));
        }
        let (h, slot_a, _slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };
        let val = mk(b"unstored");

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_buffer_sources(FIRST_STORAGE_BYTES) };
        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_a,
                Key::Int(1),
                Value::entity(Tag::String, val as *mut RcHeader),
            )
        };
        FORCE_OOM.store(false, Ordering::Relaxed);
        free_fillers(fillers);
        assert!(!stored, "the copy's storage was meant to be refused");

        unsafe {
            assert_eq!(
                (*slot_a).entity_ptr() as *mut LLArray,
                src,
                "a refused store moved the slot"
            );
            assert_eq!((*src).rc.refcount, 2, "a refused store moved a count");
            assert_eq!((*src).table.len(), 1);
            assert!((*src).table.get(Key::Int(1)).is_none());
            assert_eq!(
                (*val).rc.refcount,
                1,
                "a refused store kept the caller's value reference"
            );
            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert!(ll_release(val as *mut RcHeader));
            crate::object::ll_entity_die(val as *mut RcHeader);
        }
    }

    /// The table's own refusal, on an exclusively owned array: growth
    /// cannot allocate, the store reports, and every entry reads as
    /// before.
    #[test]
    fn a_refused_growth_reports_with_the_table_unchanged() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("OneHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            // Fill to capacity, so the next insert must grow.
            for i in 0..8i64 {
                (*src)
                    .table
                    .insert(src as *const RcHeader, Key::Int(i), Value::int(i));
            }
        }

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_buffer_sources(DOUBLED_STORAGE_BYTES) };
        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(100),
                Value::int(1),
            )
        };
        FORCE_OOM.store(false, Ordering::Relaxed);
        free_fillers(fillers);
        assert!(!stored, "growth was meant to be refused");

        unsafe {
            assert_eq!((*src).table.len(), 8, "a refused growth moved an entry");
            assert!((*src).table.get(Key::Int(100)).is_none());
            for i in 0..8i64 {
                assert_eq!((*src).table.get(Key::Int(i)).unwrap().as_int(), i);
            }
            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }

    /// The table's refusal inside a private copy: the copy dies whole,
    /// and the slot, the original and the caller's value all read as
    /// before — the second refusal of the criterion, one array further
    /// in than the separation's.
    #[test]
    fn a_growth_refusal_inside_the_copy_destroys_the_copy_alone() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        // Empty and shared: the separation's replay allocates nothing,
        // so the forced refusal lands on the copy's own first storage.
        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let (h, slot_a, _slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };
        let val = mk(b"unstored");

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_buffer_sources(FIRST_STORAGE_BYTES) };
        // The separation must not be the refusal, or this measures
        // `a_refused_separation_reports_and_changes_nothing` a second
        // time — its assertions are these. The copy's entity comes from
        // the object heap, which the exhaustion above does not reach, so
        // prove a slot is there and hand it straight back.
        unsafe {
            let probe = ll_array_new(MemoryCategory::GcHeap);
            assert!(!probe.is_null(), "the copy's entity slot was refused");
            assert!(ll_release(probe as *mut RcHeader));
            crate::object::ll_entity_die(probe as *mut RcHeader);
        }
        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_a,
                Key::Int(0),
                Value::entity(Tag::String, val as *mut RcHeader),
            )
        };
        FORCE_OOM.store(false, Ordering::Relaxed);
        free_fillers(fillers);
        assert!(!stored, "the copy's storage was meant to be refused");

        unsafe {
            assert_eq!((*slot_a).entity_ptr() as *mut LLArray, src);
            assert_eq!((*src).rc.refcount, 2);
            assert!((*src).table.is_empty());
            assert_eq!(
                (*val).rc.refcount,
                1,
                "the giveback did not balance the copy's publication"
            );
            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert!(ll_release(val as *mut RcHeader));
            crate::object::ll_entity_die(val as *mut RcHeader);
        }
    }

    /// The refusal teardown cannot wait for `ll_release`'s verdict: on
    /// an arena copy the release reports no death, and a verdict-gated
    /// branch leaves every reference the replay published sitting on a
    /// corpse until the reset — a shared COW child then reads an extra
    /// holder and separates on every write for the rest of the request.
    /// Seen failing exactly there with the gated teardown.
    #[test]
    fn a_destroyed_arena_copy_gives_its_children_back() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let child = unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"cow") };
        unsafe {
            crate::refcount::ll_retain(child as *mut RcHeader);
            (*src).table.insert(
                src as *const RcHeader,
                Key::Int(0),
                Value::entity(Tag::String, child as *mut RcHeader),
            );
            crate::refcount::ll_retain(src as *mut RcHeader);
        }
        let before = unsafe { (*child).rc.refcount };

        let copy = unsafe {
            crate::array::entity::separate(
                src,
                MemoryCategory::RequestArena,
                arena_ptr,
                crate::array::entity::CopyReason::Duplication,
            )
        };
        assert!(!copy.is_null());
        unsafe {
            assert_eq!(
                (*child).rc.refcount,
                before + 1,
                "the replay was meant to take a reference of its own"
            );
            destroy_private_copy(copy);
            assert_eq!(
                (*child).rc.refcount,
                before,
                "the corpse kept the replay's reference"
            );
        }
        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// The store's S2.2 half through `set` itself: a fresh string key is
    /// consumed, an equal-bytes overwrite hands the operation's own
    /// reference back, and a refused growth hands the published key
    /// back. Each arm seen failing under a targeted revert of its
    /// giveback.
    #[test]
    fn a_string_key_through_the_store_obeys_the_ownership_rule() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("KeyHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
        }

        let k1 = mk(b"key");
        let k2 = mk(b"key");
        assert_ne!(k1, k2, "two distinct entities, or the arms collapse");
        let k1_start = unsafe { (*k1).rc.refcount };
        let k2_start = unsafe { (*k2).rc.refcount };

        unsafe {
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Str(k1),
                Value::int(1),
            ));
            assert_eq!(
                (*k1).rc.refcount,
                k1_start + 1,
                "a stored new key is consumed into the table's reference"
            );

            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Str(k2),
                Value::int(2),
            ));
            assert_eq!(
                (*k2).rc.refcount,
                k2_start,
                "the overwrite arm kept its published reference"
            );
            assert_eq!((*src).table.get(Key::Str(k1)).unwrap().as_int(), 2);

            // Fill to capacity, so the next new key must grow — and the
            // growth is refused, so the published key must come back.
            for i in 0..7i64 {
                (*src)
                    .table
                    .insert(src as *const RcHeader, Key::Int(i), Value::int(i));
            }
            let k3 = mk(b"other");
            let k3_start = (*k3).rc.refcount;
            FORCE_OOM.store(true, Ordering::Relaxed);
            let fillers = exhaust_buffer_sources(DOUBLED_STORAGE_BYTES);
            let stored = set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Str(k3),
                Value::int(9),
            );
            FORCE_OOM.store(false, Ordering::Relaxed);
            free_fillers(fillers);
            assert!(!stored, "growth was meant to be refused");
            assert_eq!(
                (*k3).rc.refcount,
                k3_start,
                "the refused insert kept the published key"
            );

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!(
                (*k1).rc.refcount,
                k1_start,
                "the dying array did not give its key back"
            );
            for s in [k1, k2, k3] {
                assert!(ll_release(s as *mut RcHeader));
                crate::object::ll_entity_die(s as *mut RcHeader);
            }
        }
    }

    /// The in-place arm, which the shared-array tests above never take:
    /// an exclusively owned array takes a value reference of its own and
    /// gives the displaced element back. One refcounted element
    /// overwrites another, so both halves are measured on one entity
    /// each.
    #[test]
    fn a_store_in_place_gives_the_displaced_element_back() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("InPlaceHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
        }

        let first = mk(b"first");
        let second = mk(b"second");
        let first_start = unsafe { (*first).rc.refcount };
        let second_start = unsafe { (*second).rc.refcount };

        unsafe {
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(0),
                Value::entity(Tag::String, first as *mut RcHeader),
            ));
            assert_eq!(
                (*slot).entity_ptr() as *mut LLArray,
                src,
                "an exclusively owned array separated"
            );
            assert_eq!(
                (*first).rc.refcount,
                first_start + 1,
                "the array takes its own reference and leaves the caller's"
            );

            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(0),
                Value::entity(Tag::String, second as *mut RcHeader),
            ));
            assert_eq!(
                (*first).rc.refcount,
                first_start,
                "the displaced element kept the array's reference"
            );
            assert_eq!((*second).rc.refcount, second_start + 1);
            assert_eq!(
                (*src).table.get(Key::Int(0)).unwrap().entity_ptr(),
                second as *mut RcHeader
            );

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!((*second).rc.refcount, second_start);
            for s in [first, second] {
                assert!(ll_release(s as *mut RcHeader));
                crate::object::ll_entity_die(s as *mut RcHeader);
            }
        }
    }

    /// The third refusal, which is neither the separation's nor the
    /// table's: publishing an arena COW value into a longer-lived array
    /// copies it out through `escape_copy`, and that copy is an
    /// allocation no reserve funds. The array is exclusively owned, so
    /// no separation runs, and its storage already exists, so no growth
    /// runs.
    #[test]
    fn a_refused_escape_copy_of_the_value_reports_and_changes_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("EscapeHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Int(0), Value::int(0));
        }

        let value = unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"arena") };
        let before = unsafe { (*value).rc.refcount };

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_string_entities(b"arena".len()) };
        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(1),
                Value::entity(Tag::String, value as *mut RcHeader),
            )
        };
        FORCE_OOM.store(false, Ordering::Relaxed);
        free_string_fillers(fillers);
        assert!(!stored, "the value's escape copy was meant to be refused");

        unsafe {
            assert_eq!((*slot).entity_ptr() as *mut LLArray, src);
            assert_eq!((*src).table.len(), 1);
            assert!((*src).table.get(Key::Int(1)).is_none());
            assert_eq!(
                (*value).rc.refcount,
                before,
                "a refused store kept the caller's value reference"
            );
            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// The arena half of the operation, which every test above leaves
    /// out: `separation_category` keeps an arena holder's copy in the
    /// arena, so the store neither counts an escape nor logs a release,
    /// and the reset reclaims both arrays.
    #[test]
    fn a_store_through_an_arena_holder_separates_into_the_arena() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        unsafe {
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Int(0), Value::int(10));
        }
        let class = ClassBuilder::new("ArenaHolder")
            .prop("a", true)
            .prop("b", true)
            .build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::RequestArena) };
        let slot_a = unsafe { Object::prop_at(h, 16) };
        let slot_b = unsafe { Object::prop_at(h, 32) };
        unsafe {
            for s in [slot_a, slot_b] {
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    h as *mut RcHeader,
                    s,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Array, src as *mut RcHeader),
                ));
            }
            ll_release(src as *mut RcHeader);
        }

        assert!(unsafe {
            set(
                context_ptr,
                MemoryCategory::RequestArena,
                slot_a,
                Key::Int(1),
                Value::int(7),
            )
        });

        unsafe {
            let copy = (*slot_a).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert_eq!(
                crate::object::header_category(copy as *const RcHeader),
                MemoryCategory::RequestArena,
                "an arena holder's copy left the arena"
            );
            assert_eq!((*copy).table.get(Key::Int(0)).unwrap().as_int(), 10);
            assert_eq!((*copy).table.get(Key::Int(1)).unwrap().as_int(), 7);
            assert!(
                (*src).table.get(Key::Int(1)).is_none(),
                "the other holder's entries changed"
            );
            assert_eq!(
                (*src).rc.refcount,
                1,
                "the displaced original keeps exactly its other holder"
            );
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// The append's three clauses: it writes under the cursor's key, a
    /// shared array separates so the other holder's length stays put,
    /// and an exhausted cursor refuses instead of wrapping onto a live
    /// entry.
    #[test]
    fn an_append_through_one_holder_leaves_the_other_holders_length_alone() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            for i in 0..2i64 {
                (*src)
                    .table
                    .insert(src as *const RcHeader, Key::Int(i), Value::int(10 + i));
            }
        }
        let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

        assert!(unsafe { append(context_ptr, MemoryCategory::GcHeap, slot_a, Value::int(99)) });

        unsafe {
            let copy = (*slot_a).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert_eq!(
                (*copy).table.get(Key::Int(2)).unwrap().as_int(),
                99,
                "the append took the cursor's key"
            );
            assert_eq!((*copy).table.len(), 3);
            assert_eq!(
                (*src).table.len(),
                2,
                "the other holder's length followed the append"
            );
            assert!((*src).table.get(Key::Int(2)).is_none());

            // The original is exclusively `slot_b`'s now, so the highest
            // integer key goes straight in: the cursor has no successor
            // and the next append must refuse.
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Int(i64::MAX), Value::int(1));
            assert!(
                !append(context_ptr, MemoryCategory::GcHeap, slot_b, Value::int(0)),
                "an exhausted cursor appended anyway"
            );
            assert_eq!((*src).table.len(), 3, "a refused append wrote an entry");
            assert_eq!(
                (*slot_b).entity_ptr() as *mut LLArray,
                src,
                "a refused append separated"
            );

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }

    /// `unset` through one holder of a shared array: the copy loses the
    /// entry, the other holder keeps it, and both of the table's
    /// references come back — the key's by S2.2's rule, the value's by
    /// the barrier. The separation replays the entry and the removal
    /// gives it back, so the measurement is a net zero on each entity.
    #[test]
    fn an_unset_gives_the_key_back_and_leaves_the_other_holder_standing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let key = mk(b"gone");
        let value = mk(b"payload");
        unsafe {
            crate::refcount::ll_retain(key as *mut RcHeader);
            crate::refcount::ll_retain(value as *mut RcHeader);
            (*src).table.insert(
                src as *const RcHeader,
                Key::Str(key),
                Value::entity(Tag::String, value as *mut RcHeader),
            );
        }
        let key_shared = unsafe { (*key).rc.refcount };
        let value_shared = unsafe { (*value).rc.refcount };
        let (h, slot_a, _slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

        assert!(unsafe { unset(context_ptr, MemoryCategory::GcHeap, slot_a, Key::Str(key)) });

        unsafe {
            let copy = (*slot_a).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert!(
                (*copy).table.get(Key::Str(key)).is_none(),
                "the copy kept the unset entry"
            );
            assert!(
                (*src).table.get(Key::Str(key)).is_some(),
                "the other holder lost its entry"
            );
            assert_eq!(
                (*key).rc.refcount,
                key_shared,
                "the removed key did not come back"
            );
            assert_eq!(
                (*value).rc.refcount,
                value_shared,
                "the removed element did not come back"
            );

            // An absent key is not an error, and it still separates:
            // `slot_a`'s array is exclusively its own by now, so the
            // observable part is only the report.
            assert!(unset(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_a,
                Key::Int(7)
            ));

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!(
                (*key).rc.refcount,
                key_shared - 1,
                "the dying array kept it"
            );
            assert_eq!((*value).rc.refcount, value_shared - 1);
            for s in [key, value] {
                assert!(ll_release(s as *mut RcHeader));
                crate::object::ll_entity_die(s as *mut RcHeader);
            }
        }
    }

    /// The by-value read of an element in a reference state yields what
    /// the box holds rather than the box, and reading separates nothing:
    /// both holders still name the one array afterwards.
    #[test]
    fn a_read_goes_through_a_reference_box_and_separates_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        unsafe {
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Int(0), Value::int(5));
            let boxed = (*src)
                .table
                .make_ref(arena_ptr, src as *const RcHeader, Key::Int(0));
            assert!(!boxed.is_null(), "the element was meant to be boxed");
            assert_eq!(
                (*src).table.get(Key::Int(0)).unwrap().tag(),
                Tag::Reference,
                "the entry does not hold a box, so the read proves nothing"
            );
        }
        let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

        unsafe {
            let read = get(slot_a, Key::Int(0)).expect("the key is there");
            assert_eq!(read.tag(), Tag::Int, "the read handed the box back");
            assert_eq!(read.as_int(), 5);
            assert!(get(slot_a, Key::Int(1)).is_none(), "an absent key answered");
            assert_eq!(
                (*slot_a).entity_ptr() as *mut LLArray,
                src,
                "the read separated"
            );
            assert_eq!((*slot_b).entity_ptr() as *mut LLArray, src);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }

    /// A store into an element in a reference state goes **through** the
    /// box: the entry still names the box afterwards, the box holds the
    /// new value, and the value it displaced came back.
    #[test]
    fn a_store_into_a_boxed_element_goes_through_the_box() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("BoxHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        let first = mk(b"first");
        let boxed = unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            crate::refcount::ll_retain(first as *mut RcHeader);
            (*src).table.insert(
                src as *const RcHeader,
                Key::Int(0),
                Value::entity(Tag::String, first as *mut RcHeader),
            );
            let boxed = (*src)
                .table
                .make_ref(arena_ptr, src as *const RcHeader, Key::Int(0));
            assert!(!boxed.is_null(), "the element was meant to be boxed");
            boxed
        };
        let first_held = unsafe { (*first).rc.refcount };

        let second = mk(b"second");
        let second_start = unsafe { (*second).rc.refcount };
        assert!(unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(0),
                Value::entity(Tag::String, second as *mut RcHeader),
            )
        });

        unsafe {
            assert_eq!(
                (*src).table.get(Key::Int(0)).unwrap().entity_ptr(),
                boxed as *mut RcHeader,
                "the store replaced the box instead of writing through it"
            );
            assert_eq!((*boxed).value.entity_ptr(), second as *mut RcHeader);
            assert_eq!(
                get(slot, Key::Int(0)).unwrap().entity_ptr(),
                second as *mut RcHeader
            );
            assert_eq!(
                (*first).rc.refcount,
                first_held - 1,
                "the value the box displaced did not come back"
            );
            assert_eq!(
                (*second).rc.refcount,
                second_start + 1,
                "the box took no reference of its own"
            );

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!((*second).rc.refcount, second_start);
            for s in [first, second] {
                assert!(ll_release(s as *mut RcHeader));
                crate::object::ll_entity_die(s as *mut RcHeader);
            }
        }
    }

    /// The barrier publishes before it releases, so a store through the
    /// box that the barrier refuses leaves the box holding exactly what
    /// it held — the displaced value keeps its reference rather than
    /// being dropped for a store that never happened.
    #[test]
    fn a_refused_store_through_the_box_keeps_the_displaced_value() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("RefusedBoxHolder")
            .prop("a", true)
            .build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        let held = mk(b"held");
        let boxed = unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            crate::refcount::ll_retain(held as *mut RcHeader);
            (*src).table.insert(
                src as *const RcHeader,
                Key::Int(0),
                Value::entity(Tag::String, held as *mut RcHeader),
            );
            let boxed = (*src)
                .table
                .make_ref(arena_ptr, src as *const RcHeader, Key::Int(0));
            assert!(!boxed.is_null());
            boxed
        };
        let held_start = unsafe { (*held).rc.refcount };

        // An arena COW value crossing into the heap box is copied out,
        // and that copy is the allocation the refusal lands on.
        let crossing =
            unsafe { ll_string_new(context_ptr, MemoryCategory::RequestArena, b"crossing") };
        let crossing_start = unsafe { (*crossing).rc.refcount };

        FORCE_OOM.store(true, Ordering::Relaxed);
        let fillers = unsafe { exhaust_string_entities(b"crossing".len()) };
        let stored = unsafe {
            set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot,
                Key::Int(0),
                Value::entity(Tag::String, crossing as *mut RcHeader),
            )
        };
        FORCE_OOM.store(false, Ordering::Relaxed);
        free_string_fillers(fillers);
        assert!(!stored, "the crossing value's copy was meant to be refused");

        unsafe {
            assert_eq!(
                (*boxed).value.entity_ptr(),
                held as *mut RcHeader,
                "a refused store moved the box"
            );
            assert_eq!(
                (*held).rc.refcount,
                held_start,
                "the displaced value was released for a store that never happened"
            );
            assert_eq!((*crossing).rc.refcount, crossing_start);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert!(ll_release(held as *mut RcHeader));
            crate::object::ll_entity_die(held as *mut RcHeader);
        }
        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// A box is identity, so the deep copy out of the arena **shares**
    /// it rather than boxing a second one, and the escape hold-count is
    /// what keeps the arena box alive for the longer-lived copy.
    #[test]
    fn a_copy_over_an_arena_source_shares_the_box() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let src = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let boxed = unsafe {
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Int(0), Value::int(7));
            let boxed = (*src)
                .table
                .make_ref(arena_ptr, src as *const RcHeader, Key::Int(0));
            assert!(!boxed.is_null());
            boxed
        };
        assert_eq!(
            unsafe { crate::object::header_category(boxed as *const RcHeader) },
            MemoryCategory::GcHeap,
            "an arena array's box is still a heap entity (S3.1)"
        );
        assert_eq!(
            unsafe { (*boxed).rc.flags } & crate::refcount::IS_ESCAPEE,
            0,
            "a heap box is never an escapee"
        );
        assert_eq!(
            unsafe { (*boxed).rc.refcount },
            1,
            "the source's entry is the box's one holder"
        );

        let copy = unsafe {
            crate::object::escape_copy(arena_ptr, MemoryCategory::GcHeap, src as *mut RcHeader)
        } as *mut LLArray;
        assert!(!copy.is_null());

        unsafe {
            assert_eq!(
                (*copy).table.get(Key::Int(0)).unwrap().entity_ptr(),
                boxed as *mut RcHeader,
                "the copy boxed a second reference instead of sharing this one"
            );
            // A heap box is counted like any other heap entity, which is
            // the whole reason the box lives there: that count is what
            // S3.2 reads to decide between sharing and unwrapping. It
            // stood at one before this copy, and the copy shared anyway,
            // because an escape copy is a store crossing a lifetime
            // boundary rather than a duplication and collapses nothing
            // (`entity::CopyReason`).
            assert_eq!(
                (*boxed).rc.refcount,
                2,
                "the copy took no hold of its own on the shared box"
            );

            assert!(ll_release(copy as *mut RcHeader));
            crate::object::ll_entity_die(copy as *mut RcHeader);
            assert_eq!(
                (*boxed).rc.refcount,
                1,
                "the dying copy kept its hold on the box"
            );

            // The source's own reference is the reset's to give back, and
            // the record is what makes that happen. Draining it here is
            // the reset's release, done by hand so the box's death is
            // visible to this test.
            let mut logged = Vec::new();
            arena.drain_release_log(|e| logged.push(e));
            assert!(
                logged.contains(&(boxed as *mut RcHeader)),
                "the arena entry holding a heap box logged no release"
            );
            for e in logged {
                if ll_release(e) {
                    crate::object::ll_entity_die(e);
                }
            }
        }
        crate::memory::context::set_current_context(std::ptr::null_mut());
        arena.reset(|_| {});
    }

    /// `$a` with one integer element, `&$a[0]` taken or not and the
    /// binding kept or dropped, then `$b = $a; $b[0] = 3;` — and what the
    /// two names read afterwards. The whole of S3's criterion runs
    /// through this, in both memory categories.
    ///
    /// The holder is one object with two properties, so both arrays are
    /// named by real slots and every write goes through the layer rather
    /// than through the table.
    unsafe fn reference_then_copy(
        ctx: *mut crate::memory::context::LLContext,
        arena: *mut Arena,
        category: MemoryCategory,
        take_reference: bool,
        keep_binding: bool,
    ) -> (i64, i64) {
        let class = ClassBuilder::new("RefCopyHolder")
            .prop("a", true)
            .prop("b", true)
            .build();
        let holder = unsafe { new_constructed(ctx, class, category) };
        let slot_a = unsafe { Object::prop_at(holder, 16) };
        let slot_b = unsafe { Object::prop_at(holder, 32) };
        let a = unsafe { ll_array_new(category) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena,
                holder as *mut RcHeader,
                slot_a,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, a as *mut RcHeader),
            ));
            ll_release(a as *mut RcHeader);
            assert!(set(ctx, category, slot_a, Key::Int(0), Value::int(1)));

            if take_reference {
                let r = make_ref(ctx, category, slot_a, Key::Int(0));
                assert!(!r.is_null(), "the reference was refused");
                // The `$r` binding, taken and — unless it is kept — given
                // straight back, which is `unset($r)` and leaves the
                // element a reference with one holder (measured on php
                // 8.3.6: `unset` does not collapse the element).
                //
                // `GcHeap` is the binding's category whatever the array's
                // is, because `$r` is a frame slot rather than a container
                // in the arena: its reference is counted and given back
                // inside the request. Through an arena owner the release
                // would belong to the reset log, and `unset($r)` would not
                // take effect until the request ended.
                crate::refcount::ll_retain(r as *mut RcHeader);
                if !keep_binding {
                    crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, r as *mut RcHeader);
                }
            }

            // `$b = $a`, then `$b[0] = 3`.
            assert!(crate::memory::barrier::ref_store(
                arena,
                holder as *mut RcHeader,
                slot_b,
                std::ptr::null_mut(),
                *slot_a,
            ));
            assert!(set(ctx, category, slot_b, Key::Int(0), Value::int(3)));

            let read_a = get(slot_a, Key::Int(0)).expect("the key is there").as_int();
            let read_b = get(slot_b, Key::Int(0)).expect("the key is there").as_int();
            if take_reference && keep_binding {
                let boxed = match get_element(slot_a) {
                    Some(v) => v.entity_ptr(),
                    None => std::ptr::null_mut(),
                };
                if !boxed.is_null() {
                    crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, boxed);
                }
            }
            // The holder goes, and both arrays with it. Left standing,
            // their storage keeps buffer-arena chunks that the arena's
            // own tests then find in a shape they did not put it in.
            if category == MemoryCategory::GcHeap {
                assert!(ll_release(holder as *mut RcHeader));
                ll_object_die(holder);
            }
            (read_a, read_b)
        }
    }

    /// The element as the entry holds it, box included — what [`get`]
    /// deliberately looks through.
    unsafe fn get_element(slot: *const Value) -> Option<Value> {
        let a = unsafe { (*slot).entity_ptr() } as *const LLArray;
        unsafe { (*a).table.get(Key::Int(0)) }
    }

    /// S3.2's criterion, four cases against php 8.3.6, in both memory
    /// categories. The copy unwraps a box nobody else names and shares
    /// one a live `&` still binds — which is where PHP collapses a
    /// reference, and the only place it does.
    #[test]
    fn a_copy_unwraps_a_box_with_a_single_holder() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        for category in [MemoryCategory::GcHeap, MemoryCategory::RequestArena] {
            assert_eq!(
                unsafe { reference_then_copy(context_ptr, arena_ptr, category, false, false) },
                (1, 3),
                "no reference: {category:?}"
            );
            assert_eq!(
                unsafe { reference_then_copy(context_ptr, arena_ptr, category, true, false) },
                (1, 3),
                "a dead reference must not alias the copy: {category:?}"
            );
            assert_eq!(
                unsafe { reference_then_copy(context_ptr, arena_ptr, category, true, true) },
                (3, 3),
                "a live reference must alias the copy: {category:?}"
            );
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        unsafe { crate::promote::arena_reset_full(arena_ptr) };
    }

    /// The sequence that separates an exact holder count from an upper
    /// bound, pinned in both categories because the two answers differ
    /// and the difference is decided rather than accidental
    /// (`dev/DECISIONS.md`, 2026-08-08, the arena's upper bound).
    ///
    /// `$a=[1]; $r=&$a[0]; $b=$a; $b[0]=3; unset($b); unset($r);
    /// $c=$a; $c[0]=9;` then `($a[0], $c[0])`. php 8.3.6 answers
    /// `(3, 9)`: by the third copy the box has one holder and is
    /// collapsed. The heap agrees. The arena answers `(9, 9)`, because
    /// `unset($b)` gives nothing back there — an arena container is not
    /// counted, so it dies at the reset and its hold on the box stands
    /// until then. The copy therefore errs toward sharing, which is the
    /// safe direction: a count above the holders can only share, never
    /// unwrap a box a live name still reaches.
    #[test]
    fn the_arena_reads_a_box_count_as_an_upper_bound() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        for (category, expected) in [
            (MemoryCategory::GcHeap, (3, 9)),
            (MemoryCategory::RequestArena, (9, 9)),
        ] {
            let class = ClassBuilder::new("UpperBoundHolder")
                .prop("a", true)
                .prop("b", true)
                .prop("c", true)
                .build();
            let holder = unsafe { new_constructed(context_ptr, class, category) };
            let slot_a = unsafe { Object::prop_at(holder, 16) };
            let slot_b = unsafe { Object::prop_at(holder, 32) };
            let slot_c = unsafe { Object::prop_at(holder, 48) };
            let a = unsafe { ll_array_new(category) };
            let answer = unsafe {
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    holder as *mut RcHeader,
                    slot_a,
                    std::ptr::null_mut(),
                    Value::entity(Tag::Array, a as *mut RcHeader),
                ));
                ll_release(a as *mut RcHeader);
                assert!(set(
                    context_ptr,
                    category,
                    slot_a,
                    Key::Int(0),
                    Value::int(1)
                ));

                // `$r = &$a[0]`, then a copy taken while it is alive.
                let r = make_ref(context_ptr, category, slot_a, Key::Int(0));
                assert!(!r.is_null());
                crate::refcount::ll_retain(r as *mut RcHeader);
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    holder as *mut RcHeader,
                    slot_b,
                    std::ptr::null_mut(),
                    *slot_a,
                ));
                assert!(set(
                    context_ptr,
                    category,
                    slot_b,
                    Key::Int(0),
                    Value::int(3)
                ));

                // `unset($b)` through the holder's own category, which is
                // the step the arena defers, and `unset($r)` through the
                // frame's.
                let held_b = (*slot_b).entity_ptr();
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    holder as *mut RcHeader,
                    slot_b,
                    held_b,
                    Value::null(),
                ));
                crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, r as *mut RcHeader);

                // `$c = $a; $c[0] = 9;`
                assert!(crate::memory::barrier::ref_store(
                    arena_ptr,
                    holder as *mut RcHeader,
                    slot_c,
                    std::ptr::null_mut(),
                    *slot_a,
                ));
                assert!(set(
                    context_ptr,
                    category,
                    slot_c,
                    Key::Int(0),
                    Value::int(9)
                ));

                let read_a = get(slot_a, Key::Int(0)).expect("the key is there").as_int();
                let read_c = get(slot_c, Key::Int(0)).expect("the key is there").as_int();
                if category == MemoryCategory::GcHeap {
                    assert!(ll_release(holder as *mut RcHeader));
                    ll_object_die(holder);
                }
                (read_a, read_c)
            };
            assert_eq!(answer, expected, "{category:?}");
        }

        crate::memory::context::set_current_context(std::ptr::null_mut());
        unsafe { crate::promote::arena_reset_full(arena_ptr) };
    }

    /// The other crossing the heap box forces: the element enters a
    /// longer-lived holder, so an arena element becomes an escapee and
    /// outlives the request that made it. Without the gain the reset
    /// frees the object while the box still names it, which the
    /// destructor count sees as a death one reset too early.
    #[test]
    fn boxing_an_arena_element_counts_its_escape() {
        let _g = crate::memory::block_pool::test_guard();
        use std::sync::atomic::AtomicUsize;
        static DESTRUCTS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn counting(_o: *mut Object) {
            DESTRUCTS.fetch_add(1, Ordering::Relaxed);
        }
        DESTRUCTS.store(0, Ordering::Relaxed);
        let cls = ClassBuilder::new("Boxed")
            .destructor(counting as *const ())
            .build();

        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);

        let a = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let mut named = Value::entity(Tag::Array, a as *mut RcHeader);
        let slot: *mut Value = &raw mut named;
        let (keeper, keeper_slot, boxed) = unsafe {
            let target = new_constructed(context_ptr, cls, MemoryCategory::RequestArena);
            assert!(set(
                context_ptr,
                MemoryCategory::RequestArena,
                slot,
                Key::Int(0),
                Value::entity(Tag::Object, target as *mut RcHeader),
            ));
            let boxed = make_ref(context_ptr, MemoryCategory::RequestArena, slot, Key::Int(0));
            assert!(!boxed.is_null());

            // A heap holder for the box, so the box is what outlives the
            // request and the object's survival is the box's doing.
            let holder_cls = ClassBuilder::new("BoxKeeper").prop("r", true).build();
            let keeper = new_constructed(context_ptr, holder_cls, MemoryCategory::GcHeap);
            let keeper_slot = Object::prop_at(keeper, 16);
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                keeper as *mut RcHeader,
                keeper_slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Reference, boxed as *mut RcHeader),
            ));
            (keeper, keeper_slot, boxed)
        };

        named = Value::null();
        let _ = named;
        unsafe { crate::promote::arena_reset_full(arena_ptr) };
        assert_eq!(
            DESTRUCTS.load(Ordering::Relaxed),
            0,
            "the reset freed an arena object a heap box still named"
        );

        unsafe {
            let survivor = (*boxed).value;
            assert_eq!(survivor.tag(), Tag::Object, "the box lost its element");
            assert_eq!(
                crate::object::header_category(survivor.entity_ptr()),
                MemoryCategory::GcHeap,
                "the survivor was not promoted out of the arena"
            );
            crate::memory::barrier::write_value_slot(keeper_slot, Value::null());
            crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, boxed as *mut RcHeader);
            assert_eq!(
                DESTRUCTS.load(Ordering::Relaxed),
                1,
                "the object outlived the last holder of its box"
            );
            assert!(ll_release(keeper as *mut RcHeader));
            ll_object_die(keeper);
        }
        crate::memory::context::set_current_context(std::ptr::null_mut());
    }

    /// S3.1's third criterion: a whole request that takes a reference
    /// ends with the box freed and no arena block retained. The box is a
    /// heap entity inside an arena array, so the only thing that can free
    /// it is the release the entry logged — the mechanism the ruling
    /// leans on, exercised through `arena_reset_full` rather than by
    /// draining the log by hand.
    #[test]
    fn a_request_that_takes_a_reference_ends_holding_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;
        crate::memory::context::set_current_context(context_ptr);
        let retained_before = crate::memory::retained::snapshot().len();

        let cls = ClassBuilder::new("Plain").build();
        let a = unsafe { ll_array_new(MemoryCategory::RequestArena) };
        let mut holder = Value::entity(Tag::Array, a as *mut RcHeader);
        let slot: *mut Value = &raw mut holder;
        let boxed = unsafe {
            let target = new_constructed(context_ptr, cls, MemoryCategory::RequestArena);
            assert!(set(
                context_ptr,
                MemoryCategory::RequestArena,
                slot,
                Key::Int(0),
                Value::entity(Tag::Object, target as *mut RcHeader),
            ));
            make_ref(context_ptr, MemoryCategory::RequestArena, slot, Key::Int(0))
        };
        assert!(!boxed.is_null());
        assert_eq!(
            unsafe { crate::object::header_category(boxed as *const RcHeader) },
            MemoryCategory::GcHeap
        );

        // The request ends: no live stack, so the local names go first.
        holder = Value::null();
        let _ = holder;
        unsafe { crate::promote::arena_reset_full(arena_ptr) };

        let mut alive = Vec::new();
        unsafe { crate::memory::heap::for_each_entity_slot(|e| alive.push(e as usize)) };
        assert!(
            !alive.contains(&(boxed as usize)),
            "the reference box outlived the request that made it"
        );
        assert_eq!(
            crate::memory::retained::snapshot().len(),
            retained_before,
            "the request retained a block on the way out"
        );
        crate::memory::context::set_current_context(std::ptr::null_mut());
    }

    /// S2.8's criterion in full: `$a=['x'=>1]; $b=$a; $r=&$b['x']; $r=2`
    /// leaves `$a['x']` at 1 and `$b['x']` at 2. The shared table is
    /// separated before the box is written, so `$a` never names the box
    /// and the reference is not refused.
    #[test]
    fn taking_a_reference_separates_the_shared_table_first() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let x = mk(b"x");
        unsafe {
            crate::refcount::ll_retain(x as *mut RcHeader);
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Str(x), Value::int(1));
        }
        let x_shared = unsafe { (*x).rc.refcount };
        // `slot_a` is `$a`, `slot_b` is `$b`.
        let (h, slot_a, slot_b) = unsafe { two_holders(context_ptr, arena_ptr, src) };

        let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot_b, Key::Str(x)) };
        assert!(!r.is_null(), "the reference was refused");

        unsafe {
            assert_ne!(
                (*slot_b).entity_ptr() as *mut LLArray,
                src,
                "the shared table was boxed without separating"
            );
            assert_eq!(
                (*slot_a).entity_ptr() as *mut LLArray,
                src,
                "the other holder followed the separation"
            );
            assert!(
                (*src).table.get(Key::Str(x)).unwrap().tag() != Tag::Reference,
                "the original's element was boxed too"
            );

            // `$r = 2` through the public door: `$b['x']` is in a
            // reference state, so the store finds the box and writes
            // into it.
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_b,
                Key::Str(x),
                Value::int(2)
            ));
            assert_eq!((*r).value.as_int(), 2, "the store missed the box");
            assert_eq!(
                get(slot_a, Key::Str(x)).unwrap().as_int(),
                1,
                "the write through the reference reached the other holder"
            );
            assert_eq!(get(slot_b, Key::Str(x)).unwrap().as_int(), 2);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            assert_eq!((*x).rc.refcount, x_shared - 1);
            assert!(ll_release(x as *mut RcHeader));
            crate::object::ll_entity_die(x as *mut RcHeader);
        }
    }

    /// A copy shares the box, so a write through one holder of a
    /// once-shared array reaches the other: `$a=['x'=>1]; $r=&$a['x'];
    /// $b=$a; $b['x']=3;` leaves both at 3, which is PHP's rule and the
    /// reason its manual warns about copying an array holding a
    /// reference.
    #[test]
    fn a_write_through_a_shared_box_reaches_both_holders() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("SharedBoxHolder")
            .prop("a", true)
            .prop("b", true)
            .build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot_a = unsafe { Object::prop_at(h, 16) };
        let slot_b = unsafe { Object::prop_at(h, 32) };
        unsafe {
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Int(0), Value::int(1));
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot_a,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
        }

        // `$r = &$a['x']` while `$a` is the only holder: nothing to
        // separate, so the box goes into the array both will share.
        let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot_a, Key::Int(0)) };
        assert!(!r.is_null());
        // `$r` is a name, and a name is a holder. The layer hands the box
        // back at the element's count and leaves the caller's reference to
        // the caller; without it the box has one holder and the copy below
        // would unwrap it rather than share it (S3.2).
        unsafe { crate::refcount::ll_retain(r as *mut RcHeader) };
        assert_eq!(
            unsafe { (*slot_a).entity_ptr() } as *mut LLArray,
            src,
            "an exclusively owned array separated"
        );

        unsafe {
            // `$b = $a`, then `$b['x'] = 3`.
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot_b,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            assert!(set(
                context_ptr,
                MemoryCategory::GcHeap,
                slot_b,
                Key::Int(0),
                Value::int(3)
            ));

            let copy = (*slot_b).entity_ptr() as *mut LLArray;
            assert_ne!(copy, src, "the shared table separated");
            assert_eq!(
                (*copy).table.get(Key::Int(0)).unwrap().entity_ptr(),
                r as *mut RcHeader,
                "the copy boxed a second reference instead of sharing this one"
            );
            assert_eq!((*r).value.as_int(), 3);
            assert_eq!(
                get(slot_a, Key::Int(0)).unwrap().as_int(),
                3,
                "the shared box did not carry the write to the other holder"
            );
            assert_eq!(get(slot_b, Key::Int(0)).unwrap().as_int(), 3);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
            // `$r` goes out of scope last, and it is the box's final
            // holder once both arrays are gone.
            assert!(ll_release(r as *mut RcHeader));
            crate::object::ll_entity_die(r as *mut RcHeader);
        }
    }

    /// A refused box leaves the array exactly as it was, the key the
    /// reference would have created included. The exclusively-owned path
    /// has no private copy to throw away, so that rollback is explicit
    /// and this is what holds it.
    #[test]
    fn a_refused_box_takes_the_vivified_element_back_out() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("RefusedRefHolder")
            .prop("a", true)
            .build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
            // One entry buys the storage, so the vivified insert below
            // needs no growth and the refusal lands on the box alone.
            (*src)
                .table
                .insert(src as *const RcHeader, Key::Int(0), Value::int(1));
        }

        FORCE_OOM.store(true, Ordering::Relaxed);
        // A reference box is 24 bytes, the size class an empty inline
        // string takes.
        let fillers = unsafe { exhaust_string_entities(0) };
        let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot, Key::Int(9)) };
        FORCE_OOM.store(false, Ordering::Relaxed);
        free_string_fillers(fillers);
        assert!(r.is_null(), "the box was meant to be refused");

        unsafe {
            assert!(
                !(*src).table.contains(Key::Int(9)),
                "the refusal left the vivified element behind"
            );
            assert_eq!((*src).table.len(), 1);
            assert_eq!(
                (*slot).entity_ptr() as *mut LLArray,
                src,
                "a refused reference separated"
            );
            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }

    /// `$r = &$a['nope']` creates the element as null and references it,
    /// which is PHP's rule and the reason the layer cannot forward
    /// `Table::make_ref`'s null: that one means "absent".
    #[test]
    fn a_reference_to_an_absent_key_creates_it_as_null() {
        let _g = crate::memory::block_pool::test_guard();
        let mut arena = Arena::new();
        let arena_ptr: *mut Arena = &mut arena;
        let mut ctx = crate::memory::context::LLContext { arena: arena_ptr };
        let context_ptr: *mut crate::memory::context::LLContext = &mut ctx;

        let src = unsafe { ll_array_new(MemoryCategory::GcHeap) };
        let class = ClassBuilder::new("VivifyHolder").prop("a", true).build();
        let h = unsafe { new_constructed(context_ptr, class, MemoryCategory::GcHeap) };
        let slot = unsafe { Object::prop_at(h, 16) };
        unsafe {
            assert!(crate::memory::barrier::ref_store(
                arena_ptr,
                h as *mut RcHeader,
                slot,
                std::ptr::null_mut(),
                Value::entity(Tag::Array, src as *mut RcHeader),
            ));
            ll_release(src as *mut RcHeader);
        }

        let r = unsafe { make_ref(context_ptr, MemoryCategory::GcHeap, slot, Key::Int(5)) };

        unsafe {
            assert!(!r.is_null(), "an absent key was reported as a refusal");
            assert_eq!(
                (*r).value.tag(),
                Tag::Null,
                "the vivified element is not null"
            );
            assert_eq!(
                (*src).table.len(),
                1,
                "the vivified element is not in the table"
            );
            assert_eq!(
                (*src).table.get(Key::Int(5)).unwrap().entity_ptr(),
                r as *mut RcHeader
            );
            assert_eq!(get(slot, Key::Int(5)).unwrap().tag(), Tag::Null);

            assert!(ll_release(h as *mut RcHeader));
            ll_object_die(h);
        }
    }

    /// The five non-canonical spellings of the done criterion stay
    /// string keys, plus the two cheap boundaries beside them.
    #[test]
    fn a_non_canonical_spelling_stays_a_string_key() {
        let _g = crate::memory::block_pool::test_guard();
        for spelling in [&b"011"[..], b"1.0", b" 1", b"-0", b"9223372036854775808"] {
            let s = mk(spelling);
            let key = unsafe { canonical_key(s) };
            assert!(
                matches!(key, Key::Str(p) if p == s),
                "{:?} must stay a string key",
                std::str::from_utf8(spelling).unwrap()
            );
            free(s);
        }
        for spelling in [&b""[..], b"+1", b"-"] {
            let s = mk(spelling);
            assert!(
                matches!(unsafe { canonical_key(s) }, Key::Str(_)),
                "{:?} must stay a string key",
                std::str::from_utf8(spelling).unwrap()
            );
            free(s);
        }
    }
}
