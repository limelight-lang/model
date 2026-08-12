//! The table is process-wide, so two threads interning the same name
//! have to end on one entity rather than two.

use super::*;

#[test]
fn interning_is_thread_safe() {
    let _g = crate::memory::block_pool::test_guard();
    let handles: Vec<_> = (0..8)
        .map(|_| std::thread::spawn(|| intern_str("shared-name") as usize))
        .collect();
    let ptrs: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(
        ptrs.windows(2).all(|w| w[0] == w[1]),
        "all threads must agree on one address"
    );
}
