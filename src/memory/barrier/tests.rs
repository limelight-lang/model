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
        if self.slot.is_refcounted() {
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

mod the_ordinary_store;
mod what_a_counted_pair_costs_when_headers_miss;
mod what_a_prefetch_recovers_from_a_cold_pair;
mod what_a_store_costs_by_working_set;
mod what_crossing_a_category_boundary_costs;
