use super::*;
use crate::memory::context::LLContext;

fn ctx() -> *mut LLContext {
    std::ptr::null_mut()
}

/// A table whose storage is released when the binding is dropped, so
/// a test cannot leak blocks into the pool's free-list order and
/// disturb an unrelated test (which is exactly what happened once).
/// A table **inside its array**, which is the only place a table
/// lives: the memory its storage comes from is the owning entity's
/// header to say (`dev/DECISIONS.md`, "the `RcHeader` is the only
/// authority on which memory an entity lives in"), so a test needs the
/// entity to have a category to pass. Derefs to the table, so a test
/// reads as if it held one.
struct Owned(*mut crate::array::entity::LLArray);

/// The operations that need the category or the head, wrapped so that a
/// test writes them as if the table answered for itself. It cannot,
/// and for two reasons that both come from outside the table: the
/// category is read from the array through
/// `array::entity::category_of`, and a reference to the body carries
/// provenance over the body alone, so the entity pointer has to
/// arrive from outside; and the words a walker reads live in the
/// entity's head, which every operation over them takes as a
/// parameter (`array::head`). Supplying both here is what keeps a
/// test about the ordered hash from being a test about how to reach
/// one.
impl Owned {
    fn category(&self) -> MemoryCategory {
        unsafe { crate::array::entity::category_of(self.0) }
    }

    fn head(&self) -> &StorageHead {
        unsafe { &*crate::array::entity::storage_head(self.0) }
    }

    fn insert(&mut self, key: Key, value: Value) -> Option<(bool, Option<Value>)> {
        let category = self.category();
        let (table, head) = unsafe { crate::array::entity::as_table_mut(self.0) };
        table.insert(head, category, key, value)
    }

    fn get(&self, key: Key) -> Option<Value> {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.get(head, key)
    }

    fn contains(&self, key: Key) -> bool {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.contains(head, key)
    }

    #[must_use = "the pair carries the table's key reference; dropping it leaks the key"]
    fn remove(&mut self, key: Key) -> Option<(Value, *mut LLString)> {
        let (table, head) = unsafe { crate::array::entity::as_table_mut(self.0) };
        table.remove(head, key)
    }

    fn compact(&mut self) -> Option<usize> {
        let category = self.category();
        let (table, head) = unsafe { crate::array::entity::as_table_mut(self.0) };
        table.compact(head, category)
    }

    fn entry(&self, i: usize) -> &Entry {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.entry(head, i)
    }

    fn iter(&self) -> impl Iterator<Item = &Entry> {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.iter(head)
    }

    fn for_each_value(&self, f: impl FnMut(Value)) {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.for_each_value(head, f);
    }

    fn for_each_string_key(&self, f: impl FnMut(*mut LLString)) {
        let (table, head) = unsafe { crate::array::entity::as_table(self.0) };
        table.for_each_string_key(head, f);
    }

    fn used(&self) -> usize {
        self.head().used()
    }

    fn nslots(&self) -> usize {
        self.head().nslots()
    }

    fn storage(&self) -> *mut u8 {
        self.head().storage()
    }

    fn version(&self) -> usize {
        self.head().version()
    }

    fn slots(&self) -> *mut u32 {
        Table::slots(self.head())
    }

    fn entries(&self) -> *mut Entry {
        Table::entries(self.head())
    }

    fn dispose(&mut self) {
        let category = self.category();
        unsafe { crate::array::entity::dispose_storage(self.0, category) };
    }
}

impl std::ops::Deref for Owned {
    type Target = Table;
    fn deref(&self) -> &Table {
        unsafe { crate::array::entity::as_table(self.0).0 }
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            crate::array::entity::dispose_storage(
                self.0,
                crate::array::entity::category_of(self.0),
            );
            // The entity's own slot, by hand rather than through
            // `ll_entity_die`: these tests own the children and give
            // them back themselves, and teardown would release them a
            // second time. The count goes to zero first because that
            // is what a slot reaching the free list must read.
            (*self.0).rc.refcount = 0;
            crate::memory::stdapi::ll_free(self.0 as *mut u8);
        }
    }
}

/// An array address for a second thread, which the compiler will not
/// send on its own. Every test that hands one over joins the thread
/// before the array dies, and that join is what makes the address good
/// for the reading thread's whole life.
struct Handed(*mut crate::array::entity::LLArray);
unsafe impl Send for Handed {}

fn t() -> Owned {
    let a = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };
    assert!(!a.is_null(), "allocation refused in a test");
    Owned(a)
}

// ---- string keys -----------------------------------------------

fn mk(bytes: &[u8]) -> *mut LLString {
    unsafe { crate::string::ll_string_new(std::ptr::null_mut(), MemoryCategory::GcHeap, bytes) }
}

/// The entries on one slot's chain, in the order the lookup walks them.
///
/// A hole reached from a chain fails here rather than being skipped:
/// a removal that left its entry linked is what this exists to catch,
/// and `get` would answer correctly through such a chain anyway.
fn chain(m: &Owned, slot: usize) -> Vec<usize> {
    let mut walked = Vec::new();
    let mut i = unsafe { *m.slots().add(slot) };
    while i != NONE {
        let e = m.entry(i as usize);
        assert!(
            !e.is_hole(),
            "entry {i} is a hole and is still on the chain"
        );
        walked.push(i as usize);
        i = e.link();
    }

    walked
}

/// The same chain read as integer keys, which is what a test that built
/// it from a stride can compare against.
fn chain_keys(m: &Owned, slot: usize) -> Vec<i64> {
    chain(m, slot)
        .into_iter()
        .map(|i| {
            let e = m.entry(i);
            assert!(e.is_int_key(), "entry {i} is not an integer key");
            e.hash_or_key as i64
        })
        .collect()
}

// ---- the flood backstop -----------------------------------------

/// Forge the state the backstop exists for: many entries whose *full*
/// 64-bit hash agrees. Real construction of such a set needs a break
/// of the hash; here the stored hash is written directly, which
/// exercises the same code path the attack would reach.
fn force_equal_hashes(m: &mut Owned, n: usize) {
    for i in 0..n {
        let s = mk(format!("collider-{i}").as_bytes());
        unsafe { (*s).hash = 0x0BAD_C0DE_0BAD_C0DE };
        m.insert(Key::Str(s), Value::int(i as i64));
    }
}

/// Forge the other trigger's state: keys whose full hashes *differ*,
/// so the equal-hash trigger stays quiet, but whose low 16 bits agree,
/// so every one lands in the same index slot at any table size up to
/// 65536 and they form one chain.
fn extend_one_chain(m: &mut Owned, from: usize, to: usize) -> Vec<*mut LLString> {
    (from..to)
        .map(|i| {
            let s = mk(format!("chain-{i}").as_bytes());
            unsafe { (*s).hash = ((i as u64 + 1) << 16) | 0xC0DE };
            m.insert(Key::Str(s), Value::int(i as i64));
            s
        })
        .collect()
}

mod keys_that_are_strings;
mod the_append_cursor;
mod the_flood_ladder;
mod the_ordered_hash_itself;
mod what_a_walker_is_shown;
#[cfg(feature = "rc-walk")]
mod what_a_walker_reads_during_a_move;
mod what_a_walker_reads_while_the_storage_is_released;
mod what_moves_the_entries;
mod where_the_storage_comes_from;
