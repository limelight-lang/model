//! Where the head sits, expressed as the one thing that depends on it:
//! a collector thread reads the head's words while the owning thread is
//! mid-write in the representation beside them.
//!
//! **`cargo test` cannot validate this.** Every word the walker reads is
//! atomic and the mutator's own writes go elsewhere, so a run reports
//! nothing whichever placement is in force; what the placement decides is
//! whether the mutator's `&mut` asserts uniqueness over the bytes the
//! walker is reading, and Miri is the only instrument that sees such a
//! claim (`dev/WORKFLOW.md`, and `dev/POSTMORTEM.md` 2026-08-10, where an
//! atomic field inside a borrowed struct was the same defect).

use super::*;

/// A raw pointer handed to the collector's thread. The array outlives
/// the walk: the join below is what makes that true, rather than a
/// lifetime.
struct Handed(*const StorageHead);
unsafe impl Send for Handed {}

/// The walker takes the head's address and reads through it while the
/// owner inserts, which is the arrangement the whole bracket exists
/// for. Nothing here asserts a moment: what a walker sees is any state
/// the insert sequence passed through, and the coherence of it — a
/// count no larger than the entries written, and the tag the array was
/// stamped with — is all a reading can be validated by.
#[test]
fn a_walker_reads_the_head_while_the_mutator_writes_the_table() {
    const INSERTS: i64 = 32;
    /// More readings than there are inserts, so the walker is still
    /// reading after the last one rather than racing to finish first.
    /// A counted loop rather than a flag the mutator lowers: a walker
    /// that is asked to stop can be scheduled for the first time
    /// after the request and read nothing at all, which is a green run
    /// over an untouched head.
    const READINGS: usize = 128;
    let _g = crate::memory::block_pool::test_guard();
    let a = hash_arr();
    let handed = Handed(unsafe { storage_head(a) });
    let walker = std::thread::spawn(move || {
        let handed = handed;
        let mut accepted = 0usize;
        let mut highest = 0usize;
        for _ in 0..READINGS {
            if let Some(view) = unsafe { StorageHead::coherent(handed.0) } {
                assert_eq!(view.tag, StorageTag::Hash);
                assert!(
                    view.used <= INSERTS as usize,
                    "a reading counted more entries than were ever inserted"
                );
                accepted += 1;
                highest = highest.max(view.used);
            }

            // The yield is what lets Miri's scheduler put the mutator
            // between two readings; without it a spin loop can hold
            // the interpreter for the whole insert sequence.
            std::thread::yield_now();
        }

        (accepted, highest)
    });
    for i in 0..INSERTS {
        unsafe { crate::array::testing::insert(a, Key::Int(i), Value::int(i)) };
        std::thread::yield_now();
    }

    let (accepted, highest) = walker.join().unwrap();
    assert!(accepted > 0, "the walker accepted no reading at all");
    assert!(highest <= INSERTS as usize);
    unsafe {
        crate::array::entity::dispose_storage(a, category_of(a));
        crate::refcount::set_header_refcount(a as *mut crate::refcount::RcHeader, 0);
        crate::memory::stdapi::ll_free(a as *mut u8);
    }
}
