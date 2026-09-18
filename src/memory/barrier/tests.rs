use super::*;

fn entity(cat: MemoryCategory) -> RcHeader {
    RcHeader::new(cat, 0)
}

/// A one-slot container: header + slot, like a minimal object.
struct Holder {
    header: RcHeader,
    slot: Value,
}

impl Holder {
    fn new(cat: MemoryCategory) -> Self {
        Holder {
            header: entity(cat),
            slot: Value::null(),
        }
    }

    fn entity_ptr(&self) -> *mut RcHeader {
        if self.slot.is_pointer() {
            self.slot.entity_ptr()
        } else {
            std::ptr::null_mut()
        }
    }

    unsafe fn store(&mut self, arena: &mut Arena, new: *mut RcHeader) {
        let old = self.entity_ptr();
        let value = if new.is_null() {
            Value::null()
        } else {
            Value::entity(crate::value::Tag::Object, new)
        };

        assert!(unsafe { ref_store(arena, &mut self.header, &mut self.slot, old, value) });
    }
}

/// Take GC-heap strings of `bytes` until the heap refuses one, and answer
/// what was taken so the caller can give it back.
///
/// The two refusal cases need the copy of an escaping value to be refused,
/// and a block budget of zero alone does not do it: the copy asks the pool
/// only where its size class has no room, and whether a thread's own init
/// left room there is the pool's warmth on the day
/// (`dev/POSTMORTEM.md`, "a first-touch draw is the thread's history"). This
/// empties the class first, with the same call the copy will make, so the
/// refusal the case reads is the one it asked for. The budget must already
/// be zero, or this takes the heap to the end of memory.
unsafe fn fill_the_class_until_refused(ctx: *mut LLContext, bytes: &[u8]) -> Vec<*mut RcHeader> {
    const BOUND: usize = 200_000;
    let mut taken = Vec::new();
    loop {
        let s = unsafe { crate::string::ll_string_new(ctx, MemoryCategory::GcHeap, bytes) };
        if s.is_null() {
            return taken;
        }

        taken.push(s as *mut RcHeader);
        assert!(
            taken.len() <= BOUND,
            "the budgeted heap refused nothing in {BOUND} allocations, so this case reads no refusal"
        );
    }
}

mod the_ordinary_store;
mod the_owned_store;
mod what_a_counted_pair_costs_when_headers_miss;
mod what_a_prefetch_recovers_from_a_cold_pair;
mod what_a_store_costs_by_working_set;
mod what_crossing_a_category_boundary_costs;
