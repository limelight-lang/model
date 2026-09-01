use super::*;

/// A sever empties the index with the entries: every bucket reads
/// `NONE` and every hole's link is severed too, so no chain reaches a
/// hole. Without that the chains survive the sever, and the next
/// insert walks them — `chain_len` and `equal_hashes` reading the
/// collision defense's thresholds off entries that no longer exist.
#[test]
fn a_sever_unlinks_what_it_holes_and_the_next_insert_starts_clean() {
    let _g = crate::memory::block_pool::test_guard();
    let mut m = t();
    for i in 0..40i64 {
        m.insert(Key::Int(i), Value::int(i));
    }

    // Integer keys over integer values: nothing severed is refcounted,
    // so the displaced list stays empty and the test owes no releases.
    let displaced = m.sever();
    assert!(
        displaced.is_empty(),
        "an int-keyed, int-valued table displaces nothing"
    );
    assert_eq!(m.len(), 0, "a severed table holds no live entries");

    for slot in 0..m.nslots() {
        assert!(
            chain(&m, slot).is_empty(),
            "slot {slot} still chains into the severed entries"
        );
    }

    let (fresh, old) = m
        .insert(Key::Int(1000), Value::int(7))
        .expect("a severed table refuses a legal insert");
    assert!(
        fresh && old.is_none(),
        "the key was admitted as a fresh entry"
    );
    assert_eq!(m.get(Key::Int(1000)).unwrap().as_int(), 7);

    let walked: usize = (0..m.nslots()).map(|s| chain(&m, s).len()).sum();
    assert_eq!(walked, 1, "the fresh entry is the only one on any chain");
}
