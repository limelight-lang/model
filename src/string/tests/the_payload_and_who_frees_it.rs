//! A heap payload is a buffer-arena chunk that teardown gives back,
//! while an arena string leaves both halves to the reset. A survivor
//! takes its payload with it by the two routes the design fixes: an
//! in-block payload is copied, its block going back to the pool, and
//! an OS-direct run transfers, which is why nothing can refuse it.
//! An append loop moves its payload once, measured at one against
//! nine for the same 256 appends.

use super::*;

/// An arena dynamic string takes its payload from the arena, so the
/// reset reclaims both halves and teardown must not hand the payload
/// to the long-lived free routine — a block of the wrong kind.
#[test]
fn an_arena_dynamic_string_leaves_both_halves_to_the_reset() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::RequestArena, b"scoped", 0) };
    assert_eq!(
        unsafe { (*s).rc.memory_category() },
        MemoryCategory::RequestArena
    );
    assert!(unsafe { ll_string_append(&mut ctx, s, b" and grown") });
    assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"scoped and grown");

    let payload = unsafe { (*s).data };
    unsafe { string_die(s as *mut LLString) };
    // The payload is still the arena's, and still intact. Handing it
    // to the long-lived free routine instead would have written a
    // free-list link — `{ next, size }`, 16 bytes — over the front of
    // it, so reading the content back is what catches that.
    assert_eq!(
        unsafe { std::slice::from_raw_parts(payload, 16) },
        b"scoped and grown",
        "an arena payload belongs to the reset, and teardown left it alone"
    );
    arena.reset(|_| {});
}

/// An accumulator built in the arena and stored into a heap holder:
/// the entity survives the reset and its payload comes with it. An
/// in-block payload is copied, because the block it sits in goes back
/// to the pool.
#[test]
fn an_escaped_arena_string_carries_its_payload_through_the_reset() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let s =
        unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::RequestArena, b"accumulated", 0) };

    let arena_payload = unsafe { (*s).data };

    let mut heap_slot: *mut RcHeader = std::ptr::null_mut();
    unsafe {
        assert!(crate::memory::barrier::store_ptr(
            &raw mut arena,
            MemoryCategory::GcHeap,
            &raw mut heap_slot,
            s as *mut RcHeader,
        ));
        crate::promote::arena_reset_full(&raw mut arena);
    }

    let s = heap_slot as *mut LLStringDynamic;
    assert_eq!(
        unsafe { (*s).rc.memory_category() },
        MemoryCategory::GcHeap,
        "promoted"
    );
    assert_ne!(
        unsafe { (*s).data },
        arena_payload,
        "an in-block payload is copied: its block went back to the pool"
    );
    assert_eq!(unsafe { LLStringDynamic::bytes(s) }, b"accumulated");

    unsafe {
        crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, s as *mut RcHeader);
    }
}

/// The same, with a payload too large for a block. There the arena
/// only owns an OS-direct run, so ownership transfers instead of
/// being copied — the pointer does not move, nothing is allocated,
/// and the reset therefore has no way to refuse.
#[test]
fn an_os_direct_payload_transfers_instead_of_being_copied() {
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let big = vec![b'z'; crate::memory::block_pool::BLOCK_PAYLOAD + 64];
    let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::RequestArena, &big, 0) };
    let os_direct = unsafe { (*s).data };
    assert!(unsafe { (*s).capacity } as usize > crate::memory::block_pool::BLOCK_PAYLOAD);

    let mut heap_slot: *mut RcHeader = std::ptr::null_mut();
    unsafe {
        assert!(crate::memory::barrier::store_ptr(
            &raw mut arena,
            MemoryCategory::GcHeap,
            &raw mut heap_slot,
            s as *mut RcHeader,
        ));
        crate::promote::arena_reset_full(&raw mut arena);
    }

    let s = heap_slot as *mut LLStringDynamic;
    assert_eq!(
        unsafe { (*s).data },
        os_direct,
        "the run is handed over, not copied"
    );
    assert_eq!(unsafe { LLStringDynamic::bytes(s) }, &big[..]);

    unsafe {
        crate::memory::barrier::drop_ref(MemoryCategory::GcHeap, s as *mut RcHeader);
    }
}

/// Teardown returns a heap dynamic string's payload to the arena it
/// came from, and this is the assertion that says so — deleting the
/// payload half of `string_die` leaves every other test `string` has
/// green. The proof is the buffer arena's own: in critical mode a
/// freed chunk goes on the block's free list and a fitting allocation
/// finds it, so the same address coming back means the chunk was
/// really returned.
#[test]
fn teardown_returns_a_heap_payload_to_the_buffer_arena() {
    use crate::memory::buffer::{PressureMode, set_pressure_mode};
    let _g = crate::memory::block_pool::test_guard();
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };

    let content = vec![b'p'; 64];
    let s = unsafe { ll_string_new_dynamic(&mut ctx, MemoryCategory::GcHeap, &content, 0) };
    let payload = unsafe { (*s).data };
    let capacity = unsafe { (*s).capacity } as usize;
    assert!(!payload.is_null());

    unsafe {
        assert!(ll_release(s as *mut RcHeader));
        crate::object::ll_entity_die(s as *mut RcHeader);
    }

    set_pressure_mode(PressureMode::Critical);
    let (reused, _) = crate::memory::buffer_arena::with_buffer_arena(|a| a.alloc(capacity));
    set_pressure_mode(PressureMode::Plenty);
    assert_eq!(
        reused, payload,
        "the payload was not returned: the free list has no chunk of that size"
    );

    crate::memory::buffer_arena::with_buffer_arena(|a| unsafe { a.free(reused, capacity) });
    arena.reset(|_| {});
}

/// An append loop on a GC-heap string allocates its payload once and
/// extends it in place from then on, for as long as the block it sits
/// in has room.
///
/// The buffer arena extends a payload that is still the last chunk it
/// bumped, and this is what says that the string path reaches that,
/// rather than only the arena's own unit test. Measured both ways on
/// 2026-08-05: one move with the in-place path, nine without it, for
/// the same 256 appends of 16 bytes. Nine moves are nine copies of
/// everything written so far, which is the cost this exists to keep
/// at one. The benchmark could not resolve the difference
/// (`dev/BENCHMARKS.md`), so the count is the evidence, not the clock.
///
/// **The room is asked for rather than assumed, and that is the whole
/// difference between this and the version that failed one run in
/// thirteen at sixteen threads.** A rotation adopts an abandoned
/// block before it takes a fresh one — a block with no owner has
/// nobody to collect the frees posted into it (`dev/DECISIONS.md`,
/// "a buffer block carries its own cursor, so an adopted block is
/// reused and not just held") — so this thread's arena can begin in
/// a block with a few kilobytes of tail left, and a payload doubling
/// past that tail is copied once however well the in-place path
/// works. Measured on 2026-08-11, from the run that failed: the
/// payload was allocated in a block with 3280 bytes free and copied
/// when it grew to 4096, into a block with 61184. The number of
/// appends cannot fix this and neither can `test_guard()`, which
/// serialises the block pool rather than the tails other threads
/// abandon. So a move is counted against the path only when the
/// block could have held the growth, which is
/// [`try_grow_in_place`]'s own second condition.
///
/// [`try_grow_in_place`]: crate::memory::buffer_arena::BufferArena::try_grow_in_place
#[test]
fn an_append_loop_moves_its_payload_once() {
    let _g = crate::memory::block_pool::test_guard();
    let s = unsafe { ll_string_new_dynamic(std::ptr::null_mut(), MemoryCategory::GcHeap, b"", 0) };
    assert!(!s.is_null());

    let chunk = [b'x'; 16];
    let mut moves = 0;
    let mut out_of_room = 0;
    let mut extensions = 0;
    let mut last = unsafe { (*s).data };
    for _ in 0..256 {
        let capacity = unsafe { (*s).capacity };
        let room =
            crate::memory::buffer_arena::with_buffer_arena(|a| a.room_in_the_current_block());

        assert!(unsafe { ll_string_append(std::ptr::null_mut(), s, &chunk) });
        let grown = unsafe { (*s).capacity } - capacity;
        let now = unsafe { (*s).data };
        if now == last {
            if grown > 0 {
                extensions += 1;
            }

            continue;
        }

        moves += 1;
        last = now;
        // The first allocation is not a move: there was no chunk to
        // extend. After it, the block's tail is what decides.
        if capacity > 0 && room < grown as usize {
            out_of_room += 1;
        }
    }

    assert!(
        extensions > 0,
        "no growth was served in place, so the loop measured nothing"
    );
    assert_eq!(
        moves - out_of_room,
        1,
        "the payload was reallocated with room to extend it"
    );
    assert_eq!(unsafe { (*s).len }, 256 * 16);
    assert!(
        unsafe { LLStringDynamic::bytes(s) }
            .iter()
            .all(|&b| b == b'x'),
        "extending in place must not disturb what was written"
    );

    unsafe {
        if ll_release(s as *mut RcHeader) {
            crate::object::ll_entity_die(s as *mut RcHeader);
        }
    }
}
