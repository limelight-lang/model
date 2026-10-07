//! A ring held from a local is kept, a child held only by a root whose count
//! saturates the side count is kept, and a chain longer than the mark's stack
//! is kept whole; each is freed by the trace after its holder lets go, which is
//! also what shows the field cleared behind the trace that kept it.

use super::*;

/// Destructor bodies run by this file's objects since a case last cleared it.
static RUNS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_destructor(_object: *mut Object) {
    RUNS.fetch_add(1, Ordering::Relaxed);
}

#[test]
fn a_live_ring_held_from_a_local_is_kept() {
    let _g = test_guard();
    RUNS.store(0, Ordering::Relaxed);
    let mut arena = Arena::new();
    let class = node_class("TbHeldRing", counting_destructor as *const ());
    let members = unsafe { ring(&mut arena, [class; 2]) };
    // The local's counted reference, which no walked entity accounts for.
    unsafe { ll_retain(members[0] as *mut RcHeader) };

    let _ = one_trace();
    assert!(
        members.iter().all(|&member| live(member)),
        "the ring held from the local stands"
    );
    assert_eq!(RUNS.load(Ordering::Relaxed), 0, "no destructor ran");
    assert!(
        members.iter().all(|&member| unsafe {
            crate::refcount::trace_field_load(member as *mut RcHeader)
        } == 0),
        "the trace cleared the field of every survivor"
    );

    assert!(!unsafe { ll_release(members[0] as *mut RcHeader) });
    let _ = one_trace();
    assert!(
        members.iter().all(|&member| !live(member)),
        "let go, the ring is freed"
    );
    assert_eq!(RUNS.load(Ordering::Relaxed), 2);
}

/// References from outside the heap a saturating root carries: past both the
/// side count's top, 0x3FFF, and bit 15, which a mark sharing the count's
/// bits would read as marked.
const HOLDERS: usize = 0x8001;

#[test]
fn a_child_held_only_by_a_root_whose_count_saturates_is_kept() {
    let _g = test_guard();
    RUNS.store(0, Ordering::Relaxed);
    let mut arena = Arena::new();
    let class = node_class("TbSaturatedRoot", counting_destructor as *const ());
    let root = object(&mut arena, class);
    let child = object(&mut arena, class);
    unsafe {
        // Root and child hold each other, and the child has no other holder:
        // its count is the root's edge alone.
        store_prop(&mut arena, root, prop_offset(0), child);
        store_prop(&mut arena, child, prop_offset(0), root);
        assert!(!ll_release(child as *mut RcHeader));
        for _ in 0..HOLDERS {
            ll_retain(root as *mut RcHeader);
        }
        assert!(!ll_release(root as *mut RcHeader));
    }
    assert_eq!(
        unsafe { crate::refcount::header_refcount(root as *mut RcHeader) } as usize,
        HOLDERS + 1
    );

    let _ = one_trace();
    assert!(live(root), "the saturated root stands");
    assert!(live(child), "the child only the root holds stands");
    assert_eq!(RUNS.load(Ordering::Relaxed), 0, "no destructor ran");

    unsafe {
        for _ in 0..HOLDERS {
            assert!(!ll_release(root as *mut RcHeader));
        }
    }
    let _ = one_trace();
    assert!(
        !live(root) && !live(child),
        "let go, the pair is garbage and freed"
    );
    assert_eq!(RUNS.load(Ordering::Relaxed), 2);
}

#[test]
fn a_chain_longer_than_the_marks_stack_is_kept_whole() {
    let _g = test_guard();
    RUNS.store(0, Ordering::Relaxed);
    let _bound = census::bound_the_mark_stack(4);
    let mut arena = Arena::new();
    let class = node_class("TbLongChain", counting_destructor as *const ());
    // A chain of 64 whose last member points back at the first: a ring held
    // from a local at one member, so every other member is reached through
    // the mark alone.
    let chain: Vec<*mut Object> = (0..64).map(|_| object(&mut arena, class)).collect();
    unsafe {
        for (index, &member) in chain.iter().enumerate() {
            store_prop(
                &mut arena,
                member,
                prop_offset(0),
                chain[(index + 1) % chain.len()],
            );
        }

        for &member in &chain[1..] {
            assert!(!ll_release(member as *mut RcHeader));
        }
    }

    let _ = one_trace();
    assert!(
        chain.iter().all(|&member| live(member)),
        "every member stands although the stack held four"
    );
    assert_eq!(RUNS.load(Ordering::Relaxed), 0);

    assert!(!unsafe { ll_release(chain[0] as *mut RcHeader) });
    let _ = one_trace();
    assert!(chain.iter().all(|&member| !live(member)));
    assert_eq!(RUNS.load(Ordering::Relaxed), chain.len());
}
