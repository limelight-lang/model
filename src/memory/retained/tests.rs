use super::*;

/// A block address and occupants a walk may dereference, which is
/// what the module doc requires of anything registered here.
///
/// The registry is process-global, so an index left in it is read by
/// every later walk in the process. Every test in the groups declared
/// below holds the block pool's test guard, which is what serializes
/// them against the walks that take it; the cells are **leaked** on top
/// of that, because a
/// test that panics before it empties its index leaves that index
/// registered for the rest of the run and no guard covers that.
/// Freeing the cells would make such an entry a use-after-free rather
/// than one that reads refcount 0 and is skipped.
///
/// The block address is derived from the cells so that it names the
/// range they lie in. A constant would be a guess about an address
/// space the process is also carving regions out of.
///
/// The cells come back as raw pointers beside their addresses, and a
/// test that occupies one writes through **the pointer its address
/// was taken from**. Neither half of that is optional: an address
/// that has been through `usize` carries no provenance to write
/// with, and writing through the leaked slice's reference instead
/// pops the exposed raw tags off the borrow stack, so the read the
/// registry itself performs becomes the violation. Miri rejects
/// both mistakes, one per run.
fn walkable_index(n: usize) -> (usize, Vec<usize>, Vec<*mut u64>) {
    let cells: &'static mut [u64] = Box::leak(vec![0u64; n].into_boxed_slice());
    let base = cells.as_mut_ptr();
    let pointers: Vec<*mut u64> = (0..n).map(|i| unsafe { base.add(i) }).collect();
    let addresses: Vec<usize> = pointers.iter().map(|&p| p as usize).collect();
    let block = addresses[0] & !crate::memory::block_pool::BLOCK_MASK;
    (block, addresses, pointers)
}

/// Take an index out of the process-global registry, which a test
/// that registered one owes whether or not it emptied it.
fn drop_index(block: usize) {
    registry()
        .lock()
        .expect("retained index registry poisoned")
        .remove(&block);
}

mod a_block_pinned_for_a_payload;
mod the_index_a_walker_reads;
mod when_a_retained_block_goes_home;
