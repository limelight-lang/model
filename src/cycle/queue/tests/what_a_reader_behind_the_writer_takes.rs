//! The reader's side of the ring, driven from the registration path: a
//! stand-in for the collector on a second thread takes entries from the
//! front while this thread registers, and every registration comes out once,
//! in order, across the writer's block change.
//!
//! The reader is `crate::ring::Reader` over the record's two words, which is
//! what the collector's batch reads through (`crate::cycle::worker`);
//! nothing here holds the token, since a reader that only reads R needs the
//! token for its trace and not for the ring (`rfc/dev/DECISIONS.md`, "the
//! candidate queue is read behind its writer, and the collector's verdicts
//! come back by a second ring", "Who reads R").

use super::*;

use crate::cycle::testing::Sent;
use crate::ring::Reader;

#[test]
fn a_reader_on_another_thread_takes_every_registration_once_and_in_order() {
    let _g = test_guard();
    reset();
    assert!(
        refill_spares(),
        "two blocks' worth of registrations, from the cells"
    );

    // More entries than a block holds, so the reader crosses the writer's
    // block change; boxed, so that no header moves under a pointer.
    let count = BLOCK_ENTRIES + 50;
    let mut headers: Box<[RcHeader]> = (0..count).map(|_| candidate(2)).collect();
    let expected: Vec<usize> = headers
        .iter_mut()
        .map(|header| (&raw mut *header).addr())
        .collect();

    // The first block and the start of the second are written before the
    // reader starts, so that it crosses the block change while the writer
    // is in the second block; a reader that kept up would see the writer
    // wrap inside one block and no change at all.
    let ahead = BLOCK_ENTRIES + 1;
    for header in headers[..ahead].iter_mut() {
        assert!(unsafe { !release(&raw mut *header) });
    }
    assert_eq!(segment_count(), 2);

    let record = Sent(owner_record::this_thread_record());
    assert!(!record.0.is_null(), "this thread has registered before");
    let reader = std::thread::spawn(move || {
        let record: &'static OwnerRecord = unsafe { &*record.into_inner() };
        let reader = unsafe { Reader::new(record.candidate_ring()) };
        let mut out = [0; 61];
        let mut taken_in_order = Vec::with_capacity(count);
        let mut idle = 0;
        while taken_in_order.len() < count {
            let taken = reader.take(&mut out);
            taken_in_order.extend_from_slice(&out[..taken]);
            if taken == 0 {
                idle += 1;
                std::thread::yield_now();
            }
        }
        assert_eq!(
            reader.take(&mut out),
            0,
            "nothing past the last registration"
        );
        (taken_in_order, idle)
    });

    for (index, header) in headers[ahead..].iter_mut().enumerate() {
        assert!(unsafe { !release(&raw mut *header) });
        if index % 7 == 0 {
            std::thread::yield_now();
        }
    }

    let (taken, _idle) = reader.join().expect("the reader finished");
    assert_eq!(
        taken, expected,
        "every registration once, in the order it was made"
    );
    assert_eq!(candidate_count(), 0, "the reader consumed the ring");
    assert_eq!(segment_count(), 2, "and both blocks stay in the circle");

    reset();
}
