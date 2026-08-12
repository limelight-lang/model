//! The generic element layer over the table.
//!
//! The five element operations are here — `get`, `set`, `append`,
//! `unset` and `make_ref`. Every write goes through one composition,
//! `write_through`, and every operation starts by settling what the key
//! *is*, which is why the key constructor lives here too. Canonicalisation
//! sits above `Table` rather than inside it because `Map` is the table's
//! second customer and keys a map exactly: a table that canonicalised
//! would be unusable there.
//!
//! **The barrier and the entity factories are this layer's, and the
//! table below only stores what it is handed**. `store_into`
//! publishes a value and a key through `barrier::publish_child` and
//! hands `Table::insert` values it may keep; `box_element` allocates the
//! `ReferenceBox`, crosses the category boundary twice for it, and gives
//! the displaced element back. `Table` allocates no entity and calls no
//! barrier, which is what lets `Map` reuse it under a different set of
//! those rules.

use crate::array::entity::{
    LLArray, as_table, as_table_mut, as_vector, as_vector_mut, category_of, give_value_back,
    migrate_to_hash, publish_key, storage_head,
};
use crate::array::head::StorageTag;
use crate::array::table::Key;
use crate::array::vector::Vector;
use crate::memory::context::LLContext;
use crate::refcount::{MemoryCategory, RcHeader};
use crate::string::{LLString, string_bytes};
use crate::value::{Tag, Value};

/// Which representation `a` holds, read once per operation.
///
/// Every function in this layer that reaches the storage asks this first:
/// the two representations answer the same questions with different
/// strides, and reading the tag is the only way to know which. The
/// mutator may read it plainly — a walker takes it from a validated
/// reading instead (`crate::array::head`).
///
/// # Safety
/// `a` addresses a live array.
#[inline]
unsafe fn tag_of(a: *mut LLArray) -> StorageTag {
    unsafe { (*storage_head(a)).tag() }
}

/// The element under `key` as the storage holds it, or `None` for an
/// absent key. A reference-state element comes back as the box's
/// `Value`, which is what every caller here wants to see.
///
/// A vector answers `None` for every key outside `0..used`, including a
/// string key: what such a key *means* is a migration, and that decision
/// belongs to the writes rather than to a read
/// (`crate::array::vector::Vector::get`).
///
/// # Safety
/// `a` addresses a live array; `key` is canonical.
#[inline]
unsafe fn element_at(a: *mut LLArray, key: Key) -> Option<Value> {
    match unsafe { tag_of(a) } {
        StorageTag::Hash => {
            let (table, head) = unsafe { as_table(a) };
            table.get(head, key)
        }
        StorageTag::Vector => {
            let position = dense_position(key)?;
            let (vector, head) = unsafe { as_vector(a) };
            vector.get(head, position)
        }
        StorageTag::Typed => unreachable!("no producer stamps the typed vector"),
    }
}

/// The key the next append takes, or `None` when no successor is left —
/// [`append`] asks before it separates, so this reads and moves nothing.
///
/// # Safety
/// `a` addresses a live array.
#[inline]
unsafe fn append_cursor(a: *mut LLArray) -> Option<i64> {
    match unsafe { tag_of(a) } {
        StorageTag::Hash => unsafe { as_table(a) }.0.append_key(),
        StorageTag::Vector => Vector::append_key(unsafe { as_vector(a) }.1),
        StorageTag::Typed => unreachable!("no producer stamps the typed vector"),
    }
}

/// `key` as a vector position, or `None` when it is not one: a string
/// key, or a negative integer.
///
/// A position past the end is still a position here — the caller decides
/// whether that is an absence, an append or a migration.
#[inline]
fn dense_position(key: Key) -> Option<usize> {
    match key {
        Key::Int(k) if k >= 0 => Some(k as usize),
        _ => None,
    }
}

/// Bring `a` to a representation that can hold `key`, which for a vector
/// meeting a key outside `0..=used` is the 2 → 3 migration
/// (`rfc/model/arrays.md`, "Transition Rules": a string key or a sparse
/// index ends the dense list).
///
/// **`0..=used` rather than `0..used`**, because the position one past
/// the end is the append and a vector holds it by growing. Everything
/// else a write could name — a string key, a negative key, a gap — is a
/// state the representation has no bytes for.
///
/// False is the migration's own refusal, with the array still a vector
/// holding everything it held.
///
/// # Safety
/// `a` a live, exclusively owned array; `key` canonical.
unsafe fn representation_for(a: *mut LLArray, key: Key) -> bool {
    if unsafe { tag_of(a) } != StorageTag::Vector {
        return true;
    }

    let used = unsafe { as_vector(a) }.1.used();
    match dense_position(key) {
        Some(position) if position <= used => true,
        _ => unsafe { migrate_to_hash(a, category_of(a)) },
    }
}

/// The separation composition every write here goes through: separate
/// the holder's array if it is shared, run `write` on the array the
/// write must reach, publish the result, and settle the three references
/// in the order `dev/DECISIONS.md` fixes under "the creation reference is
/// spent before the displaced original is dropped".
///
/// `slot` names the array (`Tag::Array`); `owner_cat` is the holder's
/// category, a compiler parameter as at every store-side barrier.
/// [`set`], [`append`] and [`unset`] differ only in what they pass as
/// `write`.
///
/// **`false` reports a refusal with the arrays unchanged**: the slot
/// still names the original at its old count, every table holds its old
/// entries, and every reference the caller brought is still the
/// caller's. A copy refused part-way dies whole
/// ([`destroy_unpublished`]).
///
/// **The displaced original ends at one holder, up to the reset log.** A
/// heap array displaced from an *arena* holder's slot is owed its release
/// by the log rather than by this `drop_ref`, so its count stays high
/// until the reset and a later write through another holder separates
/// once more. A count above the holders can only copy more, never
/// corrupt.
///
/// `write` is handed an array no other holder can see once a copy was
/// made, and reports whether it wrote.
///
/// # Safety
/// `ctx` per `ll_arena_alloc`; `slot` a live slot of a live holder of
/// category `owner_cat`, holding a live array.
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
        unsafe { crate::object::destroy_unpublished(separated as *mut RcHeader) };
        return false;
    }

    unsafe {
        // The creation reference goes first, while no user code can run:
        // the slot's reference keeps the copy at one, so the release
        // below cannot be a death. `drop_ref` of the displaced original
        // runs `__destruct`s, and one of them may displace the copy from
        // the slot — with the creation reference already spent, that
        // nested displacement is the copy's ordinary death rather than a
        // count stranded at two.
        let died = crate::refcount::ll_release(separated as *mut RcHeader);
        debug_assert!(!died, "the slot's reference must outlive the creation one");
        crate::memory::barrier::drop_ref(owner_cat, current as *mut RcHeader);
    }

    true
}

/// The element store: `$a[k] = v` through the holder's slot.
///
/// **The order every write here guarantees**, which the three below rest
/// on: a shared array is separated first, a write being a write whatever
/// it turns out to change; the copy is written into `slot` before this
/// holder is taken off the displaced original; and the original's release
/// comes last, so a `__destruct` body it runs finds the slot already
/// naming the copy.
///
/// **Three refusals report `false`**, each an allocation no reserve
/// funds: the separation's copy, the publication of an arena COW value
/// or key into a longer-lived array (`escape_copy`, inside
/// `store_category_barrier`), and the table's growth. All three leave
/// every array unchanged — the slot names the original at its old count,
/// every table holds the entries it held, and every reference the caller
/// brought is still the caller's. One state does move on a refused
/// insert: a long chain draws the salt before growth is asked to
/// allocate and rung state is one-way, so the flood ladder may have
/// advanced.
///
/// **No caller reference is consumed.** The operation takes references
/// of its own for what the array keeps: the value's entity is retained
/// and published through `barrier::publish_child` (which may hand back
/// a copy — an arena COW value crossing into a longer-lived array), and
/// a string key follows the table's ownership rule — consumed by a new
/// entry, given
/// back through `drop_ref` when the overwrite arm kept the entry's
/// original key. The displaced element goes back the same way.
///
/// `key` is already canonical (`canonical_key`); a debug build asserts
/// that rather than re-canonicalising on every store.
///
/// # Safety
/// `ctx` per `ll_arena_alloc`; `slot` a live slot of a live holder of
/// category `owner_cat`, naming a live array; `key` canonical. And
/// `value`'s entity, if any, live and **holding a reference independent
/// of `slot`** — a subscript RHS in
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
/// append refuses rather than wrapping onto a live entry. Every array is
/// unchanged either way.
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
    let Some(key) = (unsafe { append_cursor(current) }) else {
        return false;
    };

    unsafe {
        write_through(ctx, owner_cat, slot, |a, arena| {
            store_into(a, arena, Key::Int(key), value)
        })
    }
}

/// `unset($a[k])`: drop the element and give both of the table's
/// references back — the value's by the barrier, the key's by the table's
/// rule.
///
/// **An absent key is not an error**, and neither is it a reason to skip
/// the separation: the write barrier fires on the operation rather than
/// on the outcome, so `unset($a['nope'])` through a shared holder still
/// separates. The alternative — look the key up first — would read a
/// table this holder does not exclusively own, and would make the
/// holder's sharing state depend on the argument.
///
/// `false` reports two refusals, each leaving every array unchanged as
/// they do for [`set`]: the separation's copy, and — on a strategy-2
/// array holding the key — the migration a removal needs before it can
/// express what it leaves behind ([`remove_from`]). Removal itself
/// allocates nothing.
///
/// # Safety
/// Per [`set`], less the value.
pub unsafe fn unset(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    slot: *mut Value,
    key: Key,
) -> bool {
    unsafe { write_through(ctx, owner_cat, slot, |a, _arena| remove_from(a, key)) }
}

/// `&$a[k]`: the element's `ReferenceBox`, boxing the element if it is
/// not one already, with the holder's array separated first.
///
/// **The separation comes before the boxing.** Taking a reference is a
/// write, turning the element into a reference state every later store
/// goes through, so it separates first, in [`set`]'s order and for
/// [`set`]'s reason. Boxing a shared table instead would hand `$b`'s
/// reference a box `$a` also names, and `$r = 2` would then be visible
/// through `$a['x']`. That order settles the element that is **not** a box
/// yet; one already in a reference state comes out of the separation
/// shared, provided a second name still holds the box, the separation
/// itself unwrapping a box nobody else holds
/// (`array::entity::element_for_copy`).
///
/// **An absent key is created as null and referenced**, which is PHP's
/// rule for `$r = &$a['nope']` and the reason this cannot simply forward
/// the null of the boxing step below it: that null means "absent", and
/// the caller has no way to tell it from the refusal below.
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
/// # Safety
/// Per [`set`], less the value.
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
            let vivified = element_at(a, key).is_none();
            if vivified {
                // The undo below may not be able to refuse, and on a
                // vector a removal is a migration, which can. So the
                // migration happens here, before anything is vivified,
                // where its refusal is this call's ordinary `false` with
                // nothing to take back.
                if tag_of(a) == StorageTag::Vector && !migrate_to_hash(a, category_of(a)) {
                    return false;
                }

                if !store_into(a, arena, key, Value::null()) {
                    return false;
                }
            }

            boxed = box_element(a, arena, key);
            if boxed.is_null() {
                // The vivified element goes back out. Without this a
                // refusal would leave the key present on an array this
                // call promised not to touch — and only on the
                // exclusively-owned path, where there is no private copy
                // to throw away, so the two paths would disagree about
                // what `false` means.
                if vivified {
                    let removed = remove_from(a, key);
                    debug_assert!(
                        removed,
                        "the vivification left a hash, whose removal is free"
                    );
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
/// `key` is canonical (`canonical_key`), as at every other operation
/// here: a numeric string that skipped it misses the integer key its
/// spelling denotes.
///
/// # Safety
/// `slot` a live slot holding a live array.
pub unsafe fn get(slot: *const Value, key: Key) -> Option<Value> {
    debug_assert!(
        matches!(unsafe { (*slot).tag() }, Tag::Array),
        "the slot names an array"
    );
    let a = unsafe { (*slot).entity_ptr() } as *mut LLArray;
    let element = unsafe { element_at(a, key) }?;
    if element.tag() == Tag::Reference {
        let boxed = element.entity_ptr() as *const crate::reference::LLReference;
        return Some(unsafe { (*boxed).value });
    }

    Some(element)
}

/// The table half of [`set`]: publish the value and the key for `a`,
/// insert, and settle the key's ownership on every outcome. False on
/// refusal with everything given back and `a` unchanged. Both
/// publications are `barrier::publish_child`, which `fill_from` uses for
/// the same pair.
///
/// **An element already in a reference state is written through its box
/// instead** ([`store_through_box`]), which is what makes `$r = &$a['x']`
/// followed by `$a['x'] = 2` readable through `$r`. Because a copy shares
/// the box rather than copying it, a write through one holder of a
/// once-shared array is visible to the other — PHP's rule, and the reason
/// its manual warns about copying an array with a reference in it.
///
/// The lookup that decides this is a second chain walk on every store,
/// on top of `insert`'s own. It cannot be shared with `insert` without
/// changing what `insert` returns, `Table::get` handing out a copy of the
/// Box rather than a borrow (`array/table.rs`). Unmeasured.
///
/// # Safety
/// `a` a live, exclusively owned array; `arena` the live mounted arena.
unsafe fn store_into(
    a: *mut LLArray,
    arena: *mut crate::memory::arena::Arena,
    key: Key,
    value: Value,
) -> bool {
    // Before the element is read, because a migration moves every element
    // and the reading would be of the representation that is going away.
    if !unsafe { representation_for(a, key) } {
        return false;
    }

    if let Some(element) = unsafe { element_at(a, key) } {
        if element.tag() == Tag::Reference {
            let boxed = element.entity_ptr() as *mut crate::reference::LLReference;
            return unsafe { store_through_box(arena, boxed, value) };
        }
    }

    if unsafe { tag_of(a) } == StorageTag::Vector {
        return unsafe { store_into_vector(a, arena, key, value) };
    }

    let category = unsafe { category_of(a) };
    let v = match unsafe { crate::memory::barrier::publish_child(arena, category, value) } {
        Some(published) => published,
        None => return false,
    };

    let published_key = match unsafe { publish_key(arena, category, key) } {
        Some(published) => published,
        None => {
            unsafe { give_value_back(category, &v) };
            return false;
        }
    };

    let (table, head) = unsafe { as_table_mut(a) };
    match table.insert(head, category, published_key, v) {
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
                // Key ownership: the overwrite arm kept the entry's original key,
                // so the reference published above goes back.
                if let Key::Str(k) = published_key {
                    unsafe { crate::memory::barrier::drop_ref(category, k as *mut RcHeader) };
                }
            }

            true
        }
    }
}

/// [`store_into`]'s vector half: overwrite a position or append past the
/// last one, the two writes a dense range can take.
///
/// The key is an integer inside `0..=used` — [`representation_for`] has
/// migrated everything else away — so the only refusals are the value's
/// publication and the vector's growth, and both leave the array as it
/// was. The reference-state element is the caller's case, settled before
/// this is reached.
///
/// # Safety
/// `a` a live, exclusively owned array whose storage is the mixed vector;
/// `arena` the live mounted arena.
unsafe fn store_into_vector(
    a: *mut LLArray,
    arena: *mut crate::memory::arena::Arena,
    key: Key,
    value: Value,
) -> bool {
    let position = dense_position(key).expect("a key the vector cannot hold has migrated");
    let category = unsafe { category_of(a) };
    let v = match unsafe { crate::memory::barrier::publish_child(arena, category, value) } {
        Some(published) => published,
        None => return false,
    };

    let (vector, head) = unsafe { as_vector_mut(a) };
    if let Some(displaced) = vector.set(head, position, v) {
        unsafe { give_value_back(category, &displaced) };
        return true;
    }

    if vector.push(head, category, v) {
        return true;
    }

    unsafe { give_value_back(category, &v) };
    false
}

/// Write into an element that is in a reference state: into the box's
/// own slot, through `barrier::ref_store`.
///
/// **`ref_store` rather than a plain store**, and the box being reached
/// through an entry changes nothing about that: `6afd220` moved the
/// reference sever onto the barrier because the collector's relaxed
/// loads race a plain write into a published slot, and this box *is*
/// published — the entry names it. [`box_element`]'s own
/// `write_value_slot` into a fresh box is legal only because that box is
/// not published when it runs, which makes it the pattern an implementer
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

/// Turn the element under `key` into a reference and hand the box back,
/// creating the box if the element is not one already. Null when the key
/// is absent, the box could not be allocated, or the element could not be
/// published into it.
///
/// **A reference into an element is a `ReferenceBox`, never a pointer to
/// the slot**, and the box is a heap entity even for an arena array, so
/// boxing crosses the category boundary twice
/// (`rfc/model/arrays-hashtable.md`, "Element states";
/// `dev/DECISIONS.md`, "a reference box is allocated in the GC heap,
/// always"). The entry takes the box at its factory count.
///
/// **The chain is walked twice**, once to read the element and once by
/// the insert that replaces it, where the table's own version walked it
/// once. What the second walk buys is the boundary: the table is handed
/// a finished element and decides nothing about it. `&$a[k]` is not a
/// hot path and the overwrite arm returns before any allocation, so the
/// cost is unmeasured and was not weighed.
///
/// # Safety
/// `a` a live, exclusively owned array; `arena` the live mounted arena.
unsafe fn box_element(
    a: *mut LLArray,
    arena: *mut crate::memory::arena::Arena,
    key: Key,
) -> *mut crate::reference::LLReference {
    let current = match unsafe { element_at(a, key) } {
        Some(element) => element,
        None => return std::ptr::null_mut(),
    };

    if current.tag() == Tag::Reference {
        return current.entity_ptr() as *mut crate::reference::LLReference;
    }

    let category = unsafe { category_of(a) };
    let boxed = crate::reference::ll_reference_new();
    if boxed.is_null() {
        return std::ptr::null_mut();
    }

    // Into `GcHeap` rather than into `category`: the box is the holder
    // here, and a box is a heap entity whatever the array is.
    let held = match unsafe {
        crate::memory::barrier::publish_child(arena, MemoryCategory::GcHeap, current)
    } {
        Some(element) => element,
        None => {
            unsafe { crate::object::destroy_unpublished(boxed as *mut RcHeader) };
            return std::ptr::null_mut();
        }
    };

    // Through `write_value_slot`, not a plain assignment: the factory
    // publishes the header before it returns, so the box is a counted
    // entity in the census from that instant and the collector's relaxed
    // reader can be striding it. A plain 16-byte assignment orders the
    // payload and the meta word not at all, and a reader that sees the
    // meta half first takes a refcounted tag with a null payload. No
    // holder but the entry below names the box yet, which is why the
    // store need not be `ref_store`'s composition.
    unsafe { crate::memory::barrier::write_value_slot(&raw mut (*boxed).value, held) };
    let published = unsafe {
        crate::memory::barrier::store_category_barrier(arena, category, boxed as *mut RcHeader)
    };

    debug_assert_eq!(
        published, boxed as *mut RcHeader,
        "a heap non-COW entity is never copied by the barrier"
    );
    let element = Value::entity(Tag::Reference, boxed as *mut RcHeader);
    // The key is present, so the element is overwritten rather than added:
    // no growth, nothing to refuse, and the key this call passes is never
    // the one an entry keeps. A vector holds a boxed element at a position
    // like any other Value, so this is the one write of the pair that
    // needs no migration.
    let displaced = if unsafe { tag_of(a) } == StorageTag::Vector {
        let position = dense_position(key).expect("a present vector element sits at a position");
        let (vector, head) = unsafe { as_vector_mut(a) };
        vector.set(head, position, element)
    } else {
        let (table, head) = unsafe { as_table_mut(a) };
        match table.insert(head, category, key, element) {
            Some((_, displaced)) => displaced,
            None => {
                debug_assert!(false, "an overwrite of a present key cannot be refused");
                // Not `destroy_unpublished`: the barrier above published
                // this box, and for an arena array that publication is a
                // release-at-reset record naming it. It goes back the way
                // any published entity does, so the record still has an
                // entity to release when the reset reaches it.
                unsafe { crate::memory::barrier::drop_ref(category, boxed as *mut RcHeader) };
                return std::ptr::null_mut();
            }
        }
    };

    // The entry's own reference on the element, given back only now: the
    // box already holds one of its own, and `drop_ref` runs `__destruct`
    // bodies.
    if let Some(old) = displaced {
        unsafe { give_value_back(category, &old) };
    }

    boxed
}

/// The storage half of [`unset`]: remove the entry and give the table's
/// two references back. Silent on an absent key, which PHP's `unset` is
/// too.
///
/// `drop_ref` rather than a bare release for both, by the table's rule: the
/// arena reset log or an escape hold-count may own either reference, and
/// it absorbs the integer key's null itself.
///
/// **A vector migrates before it removes anything**, and false is that
/// migration's refusal — the one thing removal can refuse. A dense range
/// has no bytes for the state a removal leaves: a position taken out of
/// the middle is a hole, and the last one taken out would rewind the
/// append cursor, which is the vector's length and may not fall
/// (`Table::adopt_append_state`, on the cursor a removal leaves standing).
/// An absent key needs no representation of its own, so it is answered
/// before the migration and costs nothing.
///
/// # Safety
/// `a` a live, exclusively owned array.
unsafe fn remove_from(a: *mut LLArray, key: Key) -> bool {
    if unsafe { tag_of(a) } == StorageTag::Vector {
        if unsafe { element_at(a, key) }.is_none() {
            return true;
        }

        if !unsafe { migrate_to_hash(a, category_of(a)) } {
            return false;
        }
    }

    let category = unsafe { category_of(a) };
    let (table, head) = unsafe { as_table_mut(a) };
    if let Some((old, removed_key)) = table.remove(head, key) {
        unsafe { give_value_back(category, &old) };
        unsafe { crate::memory::barrier::drop_ref(category, removed_key as *mut RcHeader) };
    }

    true
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
    // Through the layout-agnostic accessor: a key is whatever the string
    // factory produced, and past what its category packs in one slot that
    // is the out-of-line layout, where the inline accessor would read the
    // `data` pointer and the entity's neighbours as content.
    match canonical_int(unsafe { string_bytes(s) }) {
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
mod tests;
