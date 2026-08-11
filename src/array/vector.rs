//! Storage strategy 2 of `rfc/model/arrays.md`: the mixed vector.
//!
//! Dense integer keys `0..len`, one 16-byte [`Value`] per element, and no
//! key stored anywhere — the key **is** the position. Zend's packed array
//! is the same trade: an array that never left `0..n-1` pays neither for
//! an index nor for a key beside each element, which is half the bytes
//! strategy 3 spends and an iteration that reads no index at all.
//!
//! What this cannot hold is what ends it: a string key, or a hole in the
//! integer key space. Either one migrates the array to the ordered hash
//! (`rfc/model/arrays-hashtable.md`), and that migration is the reason
//! the version counter and the strategy tag sit above the representation
//! rather than inside it — see [`crate::array::head`].
//!
//! **No collisions, no holes, no flood.** A position cannot collide with
//! another position, a dense range has no holes by definition, and a hash
//! nobody computes cannot be attacked. So none of the ordered hash's
//! defences appear here, and the six fields that serve them are not
//! carried along either.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::array::head::{CoherentView, StorageHead, StorageTag};
use crate::refcount::{MemoryCategory, RcHeader};
use crate::value::Value;

/// Elements the first allocation holds. Same as the ordered hash's first
/// entry capacity, so an array that migrates early does not change the
/// number of growths it took to get there.
const FIRST_CAP: usize = 8;

/// The most elements a vector addresses. Past it the array is over four
/// billion elements and the migration to the hash — whose own bound is
/// `MAX_ENTRIES` — has long since been forced by something else; the
/// bound exists so the byte arithmetic below cannot wrap.
const MAX_ELEMENTS: usize = u32::MAX as usize;

/// The mixed vector — the private tail of one, the words a walker reads
/// living in the entity's [`StorageHead`] and reaching every method here
/// as a parameter ([`crate::array::head`], and `Table`'s own doc for the
/// rule both representations obey).
#[repr(C)]
pub struct Vector {
    /// Elements the chunk holds. The head's `used` is how many are
    /// written.
    cap: usize,
    /// Bytes really granted, which is not always what was asked for: a
    /// reused buffer-arena chunk may be larger, and the free that returns
    /// it carries the size. Freeing with the requested size would lose
    /// the difference from the block's free list.
    storage_capacity: usize,
}

/// Bytes for `cap` elements, or `None` when that many cannot be
/// addressed. Checked rather than assumed: `cap` grows by doubling from a
/// length the program chose.
fn storage_bytes(cap: usize) -> Option<usize> {
    if cap > MAX_ELEMENTS {
        return None;
    }

    cap.checked_mul(size_of::<Value>())
}

impl Vector {
    /// A vector over nothing. The first push allocates. The tag saying
    /// which representation this is belongs to the head, stamped by
    /// `array::entity::new_with_storage`.
    pub const fn empty() -> Self {
        Vector {
            cap: 0,
            storage_capacity: 0,
        }
    }

    /// The key the next append takes, which for a dense range is the
    /// length — and the length is the head's `used`, every element of a
    /// vector being live. Reading it moves nothing.
    #[inline]
    pub(crate) fn append_key(head: &StorageHead) -> Option<i64> {
        // A vector that reached `i64::MAX` elements cannot exist: the
        // element bound is four billion, and the allocation is 16 bytes
        // apiece. The `Option` matches the hash's `append_key`, whose
        // refusal is real, so the element layer above asks both the same
        // question.
        Some(head.used() as i64)
    }

    /// The element at `i`, or `None` past the end.
    ///
    /// A negative or oversize key is not this type's to answer: the
    /// element layer decides what a key outside `0..len` means, because
    /// the answer is a migration rather than an absence.
    #[inline]
    pub(crate) fn get(&self, head: &StorageHead, i: usize) -> Option<Value> {
        if i >= head.used() {
            return None;
        }

        Some(unsafe { load_element(self.element_ptr(head, i)) })
    }

    /// Overwrite a published element and hand the displaced value back,
    /// or `None` when `i` is past the end.
    ///
    /// **The displaced value's reference is the caller's**, exactly as
    /// the ordered hash's insert hands one back: this layer allocates no
    /// entity and calls no barrier, so it cannot release one either.
    #[inline]
    pub(crate) fn set(&mut self, head: &StorageHead, i: usize, v: Value) -> Option<Value> {
        if i >= head.used() {
            return None;
        }

        let at = self.element_ptr(head, i);
        let old = unsafe { load_element(at) };
        unsafe { store_element(at, v) };
        Some(old)
    }

    /// Append `v` at the next position. **False** when the storage could
    /// not grow, with the vector unchanged — the same refusal shape every
    /// allocating call in this crate has.
    pub(crate) fn push(&mut self, head: &StorageHead, category: MemoryCategory, v: Value) -> bool {
        let n = head.used();
        if n == self.cap && !self.grow(head, category) {
            return false;
        }

        // The element before the count that publishes it: a walker that
        // saw the count first would stride over sixteen bytes nobody had
        // written. `set_used` is the release store that orders the pair.
        unsafe { store_element(self.element_ptr(head, n), v) };
        head.set_used(n + 1);
        true
    }

    /// Double the storage, or take the first one. False on refusal, and
    /// the vector is left serving.
    ///
    /// Growth **moves elements**, so it runs inside the head's window: a
    /// walker mid-stride over the old chunk would otherwise read a fresh
    /// count against it.
    fn grow(&mut self, head: &StorageHead, category: MemoryCategory) -> bool {
        let cap = if self.cap == 0 {
            FIRST_CAP
        } else {
            self.cap * 2
        };
        let Some(bytes) = storage_bytes(cap) else {
            return false;
        };

        let (mem, granted) =
            unsafe { crate::memory::routing::body_alloc(std::ptr::null_mut(), category, bytes) };
        if mem.is_null() {
            return false;
        }

        head.begin_move();
        let old_storage = head.storage();
        let old_capacity = self.storage_capacity;
        let n = head.used();
        if !old_storage.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(old_storage, mem, n * size_of::<Value>());
            }
        }

        head.set_storage(mem);
        self.storage_capacity = granted;
        self.cap = cap;
        head.end_move();

        if !old_storage.is_null() {
            unsafe { crate::memory::routing::body_free(category, old_storage, old_capacity) };
        }

        true
    }

    /// The bytes the storage was granted, for the carry out of a dying
    /// arena (`entity::carry_storage_out_of`).
    #[inline]
    pub(crate) fn granted_capacity_mut(&mut self) -> &mut usize {
        &mut self.storage_capacity
    }

    /// Where element `i` sits. Callers have already bounded `i`.
    #[inline]
    fn element_ptr(&self, head: &StorageHead, i: usize) -> *mut u8 {
        debug_assert!(i < self.cap);
        unsafe { head.storage().add(i * size_of::<Value>()) }
    }

    /// The elements and how many there are, taken from a reading
    /// [`StorageHead::coherent`] has already validated.
    ///
    /// The vector's counterpart to `Table::entries_of`: where the head
    /// answers what every representation has in common, this turns the
    /// answer into the vector's own stride, which is the elements
    /// themselves at offset zero — a vector has no index ahead of them.
    ///
    /// # Safety
    /// `view` was read from a vector's head.
    pub(crate) unsafe fn elements_of(view: &CoherentView) -> (*mut u8, usize) {
        debug_assert_eq!(view.tag, StorageTag::Vector);
        debug_assert_eq!(
            view.nslots, 0,
            "a vector has no index ahead of its elements"
        );
        if view.storage.is_null() {
            return (std::ptr::null_mut(), 0);
        }

        (view.storage, view.used)
    }

    /// Every element in order, for a caller that owns the vector.
    pub(crate) fn for_each_value(&self, head: &StorageHead, mut f: impl FnMut(Value)) {
        for i in 0..head.used() {
            f(unsafe { load_element(self.element_ptr(head, i)) });
        }
    }

    /// Replace every element with what `f` answers, in order.
    pub(crate) fn for_each_value_mut(
        &mut self,
        head: &StorageHead,
        mut f: impl FnMut(Value) -> Value,
    ) {
        for i in 0..head.used() {
            let at = self.element_ptr(head, i);
            let next = f(unsafe { load_element(at) });
            unsafe { store_element(at, next) };
        }
    }

    /// Hand every counted child out and leave the vector empty, without
    /// releasing anything: the caller owns the references it collects.
    ///
    /// The counterpart of the hash's `sever_entries`, and shorter for the
    /// reason the whole type is shorter — there are no string keys to
    /// hand out beside the values, and no holes to skip.
    pub(crate) fn sever_entries(&mut self, head: &StorageHead, displaced: &mut Vec<*mut RcHeader>) {
        for i in 0..head.used() {
            let at = self.element_ptr(head, i);
            let value = unsafe { load_element(at) };
            unsafe { store_element(at, Value::null()) };
            if value.is_refcounted() {
                displaced.push(value.entity_ptr());
            }
        }

        head.set_used(0);
    }

    /// Give the storage back. The elements are the caller's to release
    /// first: this frees bytes and knows nothing about what was in them.
    pub fn dispose(&mut self, head: &StorageHead, category: MemoryCategory) {
        let p = head.storage();
        let capacity = self.storage_capacity;
        head.set_storage(std::ptr::null_mut());
        head.set_used(0);
        self.cap = 0;
        self.storage_capacity = 0;
        if !p.is_null() {
            unsafe { crate::memory::routing::body_free(category, p, capacity) };
        }
    }
}

/// The element at `at`.
///
/// A plain read, where the write below is two atomic stores: the walker
/// only ever reads these bytes, so the mutator races nothing by loading
/// them the cheap way. This is `Entry::value`'s asymmetry, for the same
/// reason.
///
/// # Safety
/// `at` addresses a written element of a live vector.
#[inline]
unsafe fn load_element(at: *mut u8) -> Value {
    unsafe { (at as *const Value).read() }
}

/// Publish `v` at `at` as the two words the collector loads.
///
/// **Both words go out whole**, unlike the ordered hash's entry, which
/// masks its second word because it keeps a chain link in the Box's
/// reserved bytes. A vector has no links to keep, so there is nothing to
/// preserve and nothing that could travel in a copy.
///
/// # Safety
/// `at` addresses an element slot of a live vector.
#[inline]
unsafe fn store_element(at: *mut u8, v: Value) {
    let words = v.into_words();
    unsafe {
        (*(at as *const AtomicU64)).store(words[0], Ordering::Relaxed);
        (*(at.add(8) as *const AtomicU64)).store(words[1], Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
