//! Extending the last chunk bumped costs no copy; anything else moves
//! the payload and must carry the content with it. How much slack a
//! growth takes is the pressure mode's to say.

use super::*;

#[test]
fn top_of_bump_extends_in_place_without_copy() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = arena();
    let mut b = Buffer::new();

    buffer_append(&mut a, &mut b, b"hello");
    let before = b.data;
    // Nobody allocated after us: growth must not move the payload.
    buffer_ensure(&mut a, &mut b, 4096, 0);
    assert_eq!(b.data, before, "top-of-bump growth must extend in place");
    assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 5) }, b"hello");
}

#[test]
fn displaced_payload_moves_and_preserves_content() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = arena();
    let mut b = Buffer::new();

    buffer_append(&mut a, &mut b, b"hello world");
    let _intruder = a.alloc(8); // now we are not the bump top
    let before = b.data;
    let over = b.capacity + 1;
    buffer_ensure(&mut a, &mut b, over, 0);
    assert_ne!(b.data, before, "must have moved");
    assert_eq!(
        unsafe { std::slice::from_raw_parts(b.data, b.len) },
        b"hello world"
    );
}

#[test]
fn plenty_gives_slack_tight_and_critical_do_not() {
    let _g = crate::memory::block_pool::test_guard();
    let mut a = arena();

    set_pressure_mode(PressureMode::Plenty);
    let mut b = Buffer::new();
    buffer_ensure(&mut a, &mut b, 100, 0);
    let over = b.capacity + 1;
    buffer_ensure(&mut a, &mut b, over, 0);
    assert!(b.capacity >= 200, "plenty must double, got {}", b.capacity);

    set_pressure_mode(PressureMode::Critical);
    let mut c = Buffer::new();
    buffer_ensure(&mut a, &mut c, 100, 4096);
    assert_eq!(c.capacity, 104, "critical ignores hint, exact-size only");

    set_pressure_mode(PressureMode::Tight);
    let mut d = Buffer::new();
    buffer_ensure(&mut a, &mut d, 100, 4096);
    assert_eq!(d.capacity, 4096, "tight honors the hint, no doubling");

    set_pressure_mode(PressureMode::Plenty);
}
