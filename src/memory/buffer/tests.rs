use super::*;

fn arena() -> Arena {
    Arena::new()
}

/// A payload is rounded up on the way in, and a refused growth leaves
/// the buffer exactly as it was — capacity included, since a stamped
/// capacity would promise memory the buffer does not own.
mod allocation_and_refusal {
    use super::*;

    #[test]
    fn ensure_allocates_rounded_and_empty_stays_empty() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();
        let mut b = Buffer::new();

        let p = buffer_ensure(&mut a, &mut b, 20, 0);
        assert!(!p.is_null());
        assert_eq!(b.len, 0);
        assert!(b.capacity >= 20);
        assert_eq!(b.capacity % 8, 0);
    }

    /// A refused growth must leave the buffer as it was. Copying into the
    /// null payload was a write through null; stamping the new capacity in
    /// would have left a buffer promising memory it does not own.
    #[test]
    fn refused_growth_leaves_the_buffer_intact() {
        let _g = crate::memory::block_pool::test_guard();
        use crate::memory::block_pool::FORCE_OOM;
        use std::sync::atomic::Ordering;

        let mut a = arena();
        let mut b = Buffer::new();
        assert!(buffer_append(&mut a, &mut b, b"hello"));
        // Not the bump top any more, so growth must allocate; and the rest
        // of the block is too small for the request below, so the
        // allocation has to ask the pool — which is what is refused.
        let _intruder = a.alloc(40_000);
        let (data, capacity) = (b.data, b.capacity);

        FORCE_OOM.store(true, Ordering::Relaxed);
        let p = buffer_ensure(&mut a, &mut b, 40_000, 0);
        let appended = buffer_append(&mut a, &mut b, &[0u8; 40_000]);
        FORCE_OOM.store(false, Ordering::Relaxed);

        assert!(p.is_null(), "exhaustion must report, not abort");
        assert!(!appended, "a refused append reports instead of writing");
        assert_eq!((b.data, b.capacity, b.len), (data, capacity, 5));
        assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 5) }, b"hello");
    }
}

/// Extending the last chunk bumped costs no copy; anything else moves
/// the payload and must carry the content with it. How much slack a
/// growth takes is the pressure mode's to say.
mod how_a_growth_finds_its_room {
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
}

/// A payload over a block payload is an OS-direct run, which is a
/// second allocation shape: it outlives the reset the same way and it
/// grows by copying between runs.
mod past_one_block {
    use super::*;

    #[test]
    fn payload_larger_than_a_block_goes_os_direct_and_survives_until_reset() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();
        let mut b = Buffer::new();

        buffer_append(&mut a, &mut b, b"start");
        let big = BLOCK_PAYLOAD * 3;
        buffer_ensure(&mut a, &mut b, big, 0);
        assert!(b.capacity >= big);

        // The whole payload is writable and the prefix survived the move.
        unsafe {
            std::ptr::write_bytes(b.data.add(b.len), 0xEE, b.capacity - b.len);
        }

        assert_eq!(unsafe { std::slice::from_raw_parts(b.data, 5) }, b"start");

        a.reset(|_| {}); // frees the tracked OS-direct payload; no crash
    }

    #[test]
    fn growth_in_large_payload_copies_between_runs() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();
        let mut b = Buffer::new();

        buffer_ensure(&mut a, &mut b, BLOCK_PAYLOAD * 2, 0);
        unsafe { std::ptr::write_bytes(b.data, 0xAB, BLOCK_PAYLOAD * 2) };
        b.len = BLOCK_PAYLOAD * 2;

        buffer_ensure(&mut a, &mut b, BLOCK_PAYLOAD * 5, 0);
        assert_eq!(unsafe { *b.data }, 0xAB);
        assert_eq!(unsafe { *b.data.add(BLOCK_PAYLOAD * 2 - 1) }, 0xAB);

        a.reset(|_| {});
    }
}

/// The C entry takes the context explicitly, which is the shape
/// generated code has and the one a null context falls back from.
mod the_abi_door {
    use super::*;

    #[test]
    fn abi_entry_works_with_explicit_context() {
        let _g = crate::memory::block_pool::test_guard();
        let mut a = arena();
        let mut ctx = LLContext { arena: &mut a };
        let mut b = Buffer::new();

        let p = unsafe { ll_buffer_ensure(&mut ctx, &mut b, 64, 0) };
        assert!(!p.is_null());
        assert_eq!(p, b.data);
        unsafe { crate::memory::context::ll_arena_reset(&mut ctx) };
    }
}
