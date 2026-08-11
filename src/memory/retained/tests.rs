use super::*;

/// A block address and occupants a walk may dereference, which is
/// what the module doc requires of anything registered here.
///
/// The registry is process-global, so an index left in it is read by
/// every later walk in the process. The tests below hold the block
/// pool's test guard, which is what serializes them against the walks
/// that take it; the cells are **leaked** on top of that, because a
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

/// A bump-filled former-arena block has no stride to divide by, so
/// this inventory is the only way its occupants can be enumerated. It
/// is sorted whatever order it arrives in, and safe to read while the
/// enumerator holds it.
mod the_index_a_walker_reads {
    use super::*;

    /// Registration sorts, because the census binary-searches the index
    /// and the reset discovers survivors in trace order.
    #[test]
    fn an_index_is_stored_sorted_whatever_order_it_arrives_in() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, _live) = walkable_index(3);
        let _empty = unsafe { register(block, vec![cells[2], cells[0], cells[1]]) };
        let found = snapshot()
            .into_iter()
            .find(|&(b, _)| b == block)
            .expect("registered block is in the snapshot");
        let mut ascending = cells.clone();
        ascending.sort_unstable();
        assert_eq!(&*found.1, &ascending[..]);
        drop_index(block);
    }

    /// The synchronous enumerator walks a registered index without
    /// checking that the block exists, so a registered address is
    /// dereferenced by whichever thread walks next. A zeroed cell reads
    /// refcount 0 and is skipped, which is the contract; a fabricated
    /// address is a wild read, which is what this pins against.
    #[test]
    fn a_registered_index_is_safe_for_the_enumerator_to_read() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, _live) = walkable_index(4);
        let _empty = unsafe { register(block, cells.clone()) };
        let mut seen = 0usize;
        unsafe {
            crate::memory::heap::for_each_entity_slot(|slot| {
                if cells.contains(&(slot as usize)) {
                    seen += 1;
                }
            })
        };

        drop_index(block);
        assert_eq!(seen, 0, "zeroed cells read refcount 0 and are skipped");
    }
}

/// The live count is what returns the block: the last occupant's
/// death drops the index and hands the block to the pool, and an
/// occupant already dead at registration holds nothing.
mod when_a_retained_block_goes_home {
    use super::*;

    /// The last live occupant's death empties the block, and the index
    /// is gone before the caller is told to hand the block over — the
    /// order the enumerators' readable-address contract requires.
    #[test]
    fn the_last_live_occupant_empties_the_block() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, live) = walkable_index(2);
        unsafe {
            live[0].write(1);
            live[1].write(1);
        }

        let _empty = unsafe { register(block, cells.clone()) };
        assert!(snapshot().iter().any(|&(b, _)| b == block));
        assert!(!occupant_freed(block), "one of two occupants emptied it");
        assert!(snapshot().iter().any(|&(b, _)| b == block));
        assert!(occupant_freed(block), "the second death left it occupied");
        assert!(!snapshot().iter().any(|&(b, _)| b == block));
        unsafe {
            live[0].write(0);
            live[1].write(0);
        }
    }

    /// An occupant already dead when the index is built is not counted,
    /// or the block would wait forever for a death that has happened.
    #[test]
    fn an_occupant_dead_at_registration_holds_nothing() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, live) = walkable_index(2);
        let _empty = unsafe {
            live[0].write(1);
            register(block, cells.clone())
        };

        assert!(occupant_freed(block), "the dead occupant was counted live");
        assert!(!snapshot().iter().any(|&(b, _)| b == block));
        unsafe { live[0].write(0) };
    }
}

/// A block retained for a payload the reset could not carry out waits
/// for that payload's own free the way it waits for an occupant's
/// death — and the pin is a count, one block being able to hold
/// several survivors' payloads.
mod a_block_pinned_for_a_payload {
    use super::*;

    /// A block retained for a payload it could not carry out outlives
    /// its occupants: their deaths say nothing about bytes they do not
    /// own.
    #[test]
    fn a_pinned_block_outlives_its_occupants() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, live) = walkable_index(1);
        pin(block);
        let _empty = unsafe {
            live[0].write(1);
            register(block, cells.clone())
        };

        assert!(!occupant_freed(block), "a pinned block was handed back");
        assert!(
            snapshot().iter().any(|&(b, _)| b == block),
            "registration cleared the pin set before it"
        );
        unsafe { live[0].write(0) };
        drop_index(block);
    }

    /// The payload's own free is the event the block was waiting for, so
    /// a block held for bytes alone goes home when they are freed. Before
    /// this the pin was permanent and the block was out of circulation
    /// for the life of the process; the test was seen failing on
    /// `payload_freed` answering false.
    #[test]
    fn a_freed_payload_empties_the_block_it_pinned() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, live) = walkable_index(1);
        pin(block);
        let _empty = unsafe {
            live[0].write(1);
            register(block, cells.clone())
        };

        assert!(!occupant_freed(block), "the payload still holds it");
        assert!(payload_freed(block), "the last holder of the block died");
        assert!(
            !snapshot().iter().any(|&(b, _)| b == block),
            "the index outlived the block it describes"
        );
        unsafe { live[0].write(0) };
    }

    /// One block can hold the payloads of several survivors, so the pin
    /// is a count and every payload has to report. Seen failing with the
    /// count as a flag: the first free released a block the second
    /// payload was still living in.
    #[test]
    fn a_block_pinned_for_two_payloads_waits_for_both() {
        let _g = crate::memory::block_pool::test_guard();
        let (block, cells, _live) = walkable_index(1);
        pin(block);
        pin(block);
        let _empty = unsafe { register(block, Vec::new()) };

        assert!(!payload_freed(block), "one payload still lives there");
        assert!(payload_freed(block), "both are gone now");
        assert!(!payload_freed(block), "an unpinned block reports nothing");
        // The registry is process-global and a leaked cell's block
        // address can come up again in another test, so nothing is left
        // behind even on the paths where the assertions above hold.
        drop_index(block);
        let _ = cells;
    }
}
