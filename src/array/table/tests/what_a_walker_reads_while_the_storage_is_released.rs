//! What a trace is handed while the owner releases the storage.
//!
//! An array dies while a collection still names its slot, so the head is
//! read while `dispose` drives the three words to their empty values.
//! Every reading the head hands out has to be a state the array was
//! actually in, which for these two words means: a null chunk carries no
//! counts, and a chunk carries the slots that offset past the index
//! region.
//!
//! The chunk is not strided here. That question — what a trace reads
//! while the mutator rearranges the very chunk it is striding — is
//! S38.0's, and only Miri can answer it (`PLAN.md`). The bracket this
//! test drives belongs to the array rather than to a collector, which is
//! why it survives having none.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Fill and release, over and over, against a thread doing nothing
/// but accepting readings.
///
/// The count of rounds is what the test costs and what it buys: a
/// mixed reading is on offer only while the two stores between the
/// chunk and the last count are in flight, so it is rare and its rate
/// is not steady. Against the unbracketed `dispose` this replaced,
/// seven runs of 4096 rounds reported 13, 26, 30, 60, 84, 204 and 275
/// mixed readings — an order of magnitude apart and never zero. A
/// reading is not a round, the reader taking several inside one
/// window, so those counts bound the rounds that caught it from
/// above rather than below, and no single round should be asked to
/// flip that coin. Under Miri the interpreter costs orders of
/// magnitude more per round and explores no hardware interleaving
/// anyway, so the count drops to what still builds the arrangement.
///
/// **Every mixed reading was the harmless half**, and that is the
/// point of asking both clauses: the body published the null chunk
/// first, so what a reader could catch was the null chunk against the counts
/// of the live state, which `entries_of` short-circuits before it
/// strides anything. The half that frees a live entity — a live
/// chunk against the empty counts, strided as `nslots` zero says the
/// entries begin at offset zero, which is the index region — is
/// reachable from the same body written in the tidier order, and the
/// window is what makes that order free to choose.
#[test]
fn disposing_hands_out_no_state_the_array_never_had() {
    const ENTRIES: i64 = 8;
    let rounds = if cfg!(miri) { 4 } else { 4096 };
    let _g = crate::memory::block_pool::test_guard();

    let mut m = t();
    let handed = Handed(m.0);
    let stop = Arc::new(AtomicBool::new(false));
    let mixed = Arc::new(AtomicUsize::new(0));
    let empty = Arc::new(AtomicUsize::new(0));
    let filled = Arc::new(AtomicUsize::new(0));
    let reader = {
        let (stop, mixed, empty, filled) =
            (stop.clone(), mixed.clone(), empty.clone(), filled.clone());
        std::thread::spawn(move || {
            let handed = handed;
            let head = unsafe { crate::array::entity::storage_head(handed.0) };
            while !stop.load(Ordering::Relaxed) {
                let Some(view) = (unsafe { StorageHead::coherent(head) }) else {
                    continue;
                };

                if view.storage.is_null() {
                    empty.fetch_add(1, Ordering::Relaxed);
                    if view.nslots != 0 || view.used != 0 {
                        mixed.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    filled.fetch_add(1, Ordering::Relaxed);
                    if view.nslots == 0 {
                        mixed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
    };

    for _ in 0..rounds {
        for i in 0..ENTRIES {
            assert!(m.insert(Key::Int(i), Value::int(i)).is_some());
        }

        m.dispose();
    }

    stop.store(true, Ordering::Relaxed);
    reader.join().unwrap();
    assert_eq!(
        mixed.load(Ordering::Relaxed),
        0,
        "a reading mixed the chunk of one state with the counts of another"
    );
    assert!(
        empty.load(Ordering::Relaxed) > 0 && filled.load(Ordering::Relaxed) > 0,
        "the reader saw only one side of the release, so it raced nothing"
    );
}

/// The release is one window rather than three separate
/// publications, which is what makes the order of the three stores
/// inside it free to change.
#[test]
fn disposing_the_storage_opens_one_window() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..8i64 {
        assert!(m.insert(Key::Int(i), Value::int(i)).is_some());
    }

    let before = m.version();
    m.dispose();
    assert_eq!(
        m.version(),
        before + 2,
        "one window opened and closed around the three words"
    );
    assert!(m.storage().is_null() && m.nslots() == 0 && m.used() == 0);
}
