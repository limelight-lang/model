//! The region is process-wide, so two threads bumping it may not be
//! handed overlapping memory.

use super::*;

#[test]
fn concurrent_allocation_hands_out_distinct_memory() {
    let _g = crate::memory::block_pool::test_guard();
    use std::thread;

    let handles: Vec<_> = (0..8)
        .map(|t| {
            thread::spawn(move || {
                let mut mine = Vec::new();
                for i in 0..500u64 {
                    let p = immortal_alloc(16) as *mut u64;
                    unsafe { p.write(t as u64 * 1_000_000 + i) };
                    mine.push((p, t as u64 * 1_000_000 + i));
                }

                // Nothing is ever freed, so every write must survive.
                for (p, v) in mine {
                    assert_eq!(unsafe { *p }, v, "immortal memory corrupted");
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
}
