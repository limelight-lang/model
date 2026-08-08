//! The generic element layer over the table (`PLAN.md`, stage S2).
//!
//! The five element operations — read, store, append, unset, take a
//! reference — are built here with S2.5, over the key constructor,
//! because every one of them starts by settling what the key *is*.
//! Canonicalisation sits in this layer rather than inside
//! `Table` on purpose: `Map` is the table's second customer and keys a
//! map exactly, so a table that canonicalised would be unusable there.

use crate::array::entity::{LLArray, give_value_back};
use crate::array::table::{Key, Table};
use crate::memory::context::LLContext;
use crate::refcount::{MemoryCategory, RcHeader};
use crate::string::LLString;
use crate::value::{Tag, Value};

/// The element store: `$a[k] = v` through the holder's slot, with the
/// whole separation composition inside.
///
/// `slot` names the array (`Tag::Array`); `owner_cat` is the holder's
/// category, a compiler parameter as at every store-side barrier. On a
/// shared array the operation separates first, fills the private copy,
/// and publishes only then, in this order: `store_box` writes the copy
/// into `slot`, `ll_release` spends the copy's creation reference, and
/// `drop_ref` takes this holder off the displaced original. The last
/// two are in that order and not in `string::separate`'s, because
/// `drop_ref` runs `__destruct` bodies and one of them can displace the
/// copy from this very slot; the creation reference is therefore spent
/// while no user code can run. Composed in one place, the "copy left at
/// two separates forever" trap is unreachable from any call site.
///
/// **`false` reports a refusal with the arrays unchanged**: the slot
/// still names the original at its old count, every table holds its old
/// entries, and every reference the caller brought is still the
/// caller's. Three refusals report this way, each an allocation no
/// reserve funds: the separation's copy, the publication of an arena
/// COW value or key into a longer-lived array (`escape_copy`, inside
/// `store_category_barrier`), and the table's growth. A copy refused
/// part-way dies whole ([`destroy_private_copy`]). One state does move
/// on a refused insert: a long chain draws the salt before growth is
/// asked to allocate and rung state is one-way, so the flood ladder may
/// have advanced.
///
/// **The displaced original ends at one holder — up to the reset log.**
/// A heap array displaced from an *arena* holder's slot is owed its
/// release by the log, not by this `drop_ref`, so its count stays high
/// until the reset and a later store through another holder separates
/// once more: conservative — a count above the holders can only copy
/// more, never corrupt — and inherent to log ownership rather than a
/// defect here.
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
/// `ctx` per `ll_arena_alloc`; `slot` a live slot of a live holder of
/// category `owner_cat`, holding a live array; `value`'s entity, if
/// any, live and **holding a reference independent of `slot`** — a
/// subscript RHS in PHP is a temporary with a reference of its own, so
/// `$a[0] = $a` reaches here at count 2 and separates. Handed the
/// slot's own array at count 1, this would build `$a[0] === $a`, a
/// cycle PHP's value semantics cannot produce.
pub unsafe fn set(
    ctx: *mut LLContext,
    owner_cat: MemoryCategory,
    slot: *mut Value,
    key: Key,
    value: Value,
) -> bool {
    // The tag, not `is_refcounted`: a ReferenceBox passes the flag test,
    // and `ll_cow_separate` has no arm for that kind — in release it
    // hands the box back, and the table write below would then lay an
    // `LLArray` over an `LLReference`'s `Value`.
    debug_assert!(
        matches!(unsafe { (*slot).tag() }, Tag::Array),
        "the slot names an array"
    );
    #[cfg(debug_assertions)]
    if let Key::Str(s) = key {
        debug_assert!(
            matches!(unsafe { canonical_key(s) }, Key::Str(_)),
            "the layer canonicalises before it stores"
        );
    }
    let current = unsafe { (*slot).entity_ptr() } as *mut LLArray;
    let arena = crate::memory::context::resolve_arena(ctx);
    let separated =
        unsafe { crate::object::ll_cow_separate(ctx, owner_cat, current as *mut RcHeader) }
            as *mut LLArray;
    if separated.is_null() {
        return false;
    }
    if separated == current {
        return unsafe { store_into(current, arena, key, value) };
    }
    // The copy is private until published, so a refusal from here on
    // destroys it whole and leaves the slot and the original untouched.
    // `store_box` is not one of the three refusals today: it refuses
    // only when it must copy an arena COW entity out for a longer-lived
    // holder, and `separation_category` puts the copy in the arena
    // exactly when `owner_cat` is the arena. Tested rather than assumed,
    // because the day that mapping changes, an ignored `false` leaves
    // the slot naming the original while the copy holds every child.
    if !unsafe { store_into(separated, arena, key, value) }
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
        // count stranded at two (Критик, S2.5 round 1).
        let died = crate::refcount::ll_release(separated as *mut RcHeader);
        debug_assert!(!died, "the slot's reference must outlive the creation one");
        crate::memory::barrier::drop_ref(owner_cat, current as *mut RcHeader);
    }
    true
}

/// Tear a refused, never-published copy down — children given back,
/// storage returned, the heap slot freed. `ll_entity_die` runs
/// **unconditionally** after the count is dropped, because the release
/// verdict cannot be trusted here: on an arena entity `ll_release`
/// reports no death, and a refusal branch that waited for `true` left
/// every reference the replay published — an arena COW child's count,
/// a heap child's log record's +1 — held by a corpse until the reset
/// (Критик, S2.5 round 1).
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
/// # Safety
/// `a` a live, exclusively owned array; `arena` the live mounted arena.
unsafe fn store_into(
    a: *mut LLArray,
    arena: *mut crate::memory::arena::Arena,
    key: Key,
    value: Value,
) -> bool {
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
        // The copy's first storage: 8 index slots and 8 entries, 288 bytes.
        let fillers = unsafe { exhaust_buffer_sources(288) };
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
        // The doubled storage: 16 index slots and 16 entries, 576 bytes.
        let fillers = unsafe { exhaust_buffer_sources(576) };
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
        // The copy's first storage: 8 index slots and 8 entries, 288 bytes.
        let fillers = unsafe { exhaust_buffer_sources(288) };
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

        let copy =
            unsafe { crate::array::entity::separate(src, MemoryCategory::RequestArena, arena_ptr) };
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
            let fillers = exhaust_buffer_sources(576);
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
