//! The order the held stack expands a batch in (`crate::cycle::mark`, "The
//! held stack"): a registered target is held when first met, the passes expand
//! the held entries whose rows read zero, and what stays above zero is
//! expanded last. A complete mark leaves the same rows in either order, so
//! the order is read off the expansions themselves ([`record_expansions`]),
//! and the plain depth-first descent ([`hold_nothing`]) is the control.

use super::*;
use crate::cells::PlainCells;
use crate::cycle::queue::release_queue_segments;
use crate::cycle::testing::open_arena;
use crate::refcount::{is_registered_candidate, mutator_flags};

/// Links of the unregistered state behind the registered core.
const STATE: usize = 8;

/// A request beside the state it reads: the root names the request's first
/// member at its first property and the state's registered core at its
/// second; the request's two members, one in-edge each, and the core, held
/// by a keeper as well, are registered candidates, and the state's links keep
/// their creation references, so nothing registers them.
struct RequestBesideTheState {
    root: *mut Object,
    keeper: *mut Object,
    request: [*mut Object; 2],
    core: *mut Object,
    state: Vec<*mut Object>,
}

impl RequestBesideTheState {
    /// # Safety
    /// As `new_constructed`: `arena` is this thread's.
    unsafe fn build(arena: &mut Arena) -> Self {
        let class = node_class("HeldStackNode");
        let root = unsafe { a_held_object(arena, class) };
        let keeper = unsafe { a_held_object(arena, class) };
        let request = [unsafe { a_held_object(arena, class) }, unsafe {
            a_held_object(arena, class)
        }];
        let core = unsafe { a_held_object(arena, class) };
        let state: Vec<*mut Object> = (0..STATE)
            .map(|_| unsafe { a_held_object(arena, class) })
            .collect();
        unsafe {
            store_prop(arena, root, prop_offset(0), request[0]);
            store_prop(arena, root, prop_offset(1), core);
            store_prop(arena, request[0], prop_offset(0), request[1]);
            store_prop(arena, keeper, prop_offset(0), core);
            store_prop(arena, core, prop_offset(0), state[0]);
            for link in state.windows(2) {
                store_prop(arena, link[0], prop_offset(0), link[1]);
            }

            // A decrement that is not the last registers the entity.
            for registered in [request[0], request[1], core] {
                assert!(!ll_release(registered as *mut RcHeader));
                assert!(
                    is_registered_candidate(mutator_flags(registered as *mut RcHeader)),
                    "the fixture's decrement registered the entity"
                );
            }
        }

        Self {
            root,
            keeper,
            request,
            core,
            state,
        }
    }

    fn every_entity(&self) -> Vec<*mut Object> {
        let mut every = vec![
            self.root,
            self.keeper,
            self.request[0],
            self.request[1],
            self.core,
        ];
        every.extend(&self.state);
        every
    }

    /// # Safety
    /// As `store_prop`, with no trace standing over the entities.
    unsafe fn let_go(self, arena: &mut Arena) {
        unsafe {
            for registered in [self.request[0], self.request[1], self.core] {
                ll_retain(registered as *mut RcHeader);
            }
            for holder in self.every_entity() {
                store_prop(arena, holder, prop_offset(0), std::ptr::null_mut());
                store_prop(arena, holder, prop_offset(1), std::ptr::null_mut());
            }
            for entity in self.every_entity() {
                assert!(ll_release(entity as *mut RcHeader));
                ll_object_die(entity);
            }
        }
        release_queue_segments();
    }
}

/// The position of `entity` in `order`, which must name it.
fn expanded_at(order: &[*mut RcHeader], entity: *mut Object) -> usize {
    order
        .iter()
        .position(|&expanded| expanded == entity as *mut RcHeader)
        .expect("the mark expanded every entity of the fixture")
}

/// Puts the plain descent back however the case ends.
struct HoldingRestored;

impl Drop for HoldingRestored {
    fn drop(&mut self) {
        hold_nothing(false);
    }
}

/// The root's cells are visited in property order, so the plain descent pops
/// the core first and walks the whole state before it reaches the request;
/// the held stack holds both registered targets, expands the request's first
/// member in its first pass (one in-edge, already subtracted), the second in
/// the next, and the core, still held from outside, only in the final drain.
#[test]
fn a_requests_registered_interior_is_expanded_before_the_state_beside_it() {
    let _g = test_guard();
    release_queue_segments();
    let mut arena = Arena::new();
    let graph = unsafe { RequestBesideTheState::build(&mut arena) };

    let mut trace = open_arena();
    record_expansions();
    assert_eq!(
        unsafe { mark::<PlainCells>(&mut trace, graph.root as *mut RcHeader) },
        MarkResult::Complete
    );
    let held = take_expansions();
    trace.reset();
    drop(trace);

    let _restored = HoldingRestored;
    hold_nothing(true);
    let mut plain = open_arena();
    record_expansions();
    assert_eq!(
        unsafe { mark::<PlainCells>(&mut plain, graph.root as *mut RcHeader) },
        MarkResult::Complete
    );
    let descended = take_expansions();
    plain.reset();
    drop(plain);
    hold_nothing(false);

    assert_eq!(
        held.len(),
        4 + STATE,
        "every reachable entity expanded once"
    );
    assert!(
        expanded_at(&held, graph.request[1]) < expanded_at(&held, graph.core),
        "the request's interior goes before the core: {held:?}"
    );
    assert!(expanded_at(&held, graph.core) < expanded_at(&held, graph.state[0]));
    assert!(
        expanded_at(&descended, graph.state[STATE - 1]) < expanded_at(&descended, graph.request[0]),
        "the control walks the state before the request, which is the order the held stack changes"
    );

    unsafe { graph.let_go(&mut arena) };
}

/// The two orders leave every row of a complete mark where the other leaves
/// it: each met entity is expanded once either way, and a subtraction does
/// not depend on when it is made.
#[test]
fn a_complete_mark_leaves_the_same_rows_in_either_order() {
    let _g = test_guard();
    release_queue_segments();
    let mut arena = Arena::new();
    let graph = unsafe { RequestBesideTheState::build(&mut arena) };

    let rows = |holding_nothing: bool| {
        hold_nothing(holding_nothing);
        let mut trace = open_arena();
        assert_eq!(
            unsafe { mark::<PlainCells>(&mut trace, graph.root as *mut RcHeader) },
            MarkResult::Complete
        );
        let words: Vec<u32> = [graph.root, graph.request[0], graph.request[1], graph.core]
            .into_iter()
            .chain(graph.state.iter().copied())
            .map(|entity| unsafe { working_count(entity) })
            .collect();
        trace.reset();
        words
    };
    let _restored = HoldingRestored;
    let held = rows(false);
    let descended = rows(true);
    hold_nothing(false);

    assert_eq!(held, descended);
    assert_eq!(
        &held[..4],
        &[1, 0, 0, 1],
        "the root keeps its own count, the request reads zero, the keeper holds the core"
    );
    assert!(
        held[4..].iter().all(|&count| count == 1),
        "each link of the state keeps its creation reference"
    );

    unsafe { graph.let_go(&mut arena) };
}

/// Links of the registered chain below.
const CHAIN: usize = 2_000;

/// A chain of registered entities one in-edge each, hanging off the root, is
/// expanded in one pass: what a pass's expansion holds is read in the same
/// pass, so the count of passes does not grow with the chain. A pass that left
/// each child for the next would make one pass a link, re-reading every entry
/// above zero at each.
#[test]
fn a_registered_chain_is_expanded_in_one_pass_whatever_its_length() {
    let _g = test_guard();
    release_queue_segments();
    let class = node_class("HeldStackChain");
    let mut arena = Arena::new();
    let root = unsafe { a_held_object(&mut arena, class) };
    let mut chain: Vec<*mut Object> = Vec::with_capacity(CHAIN);
    let mut holder = root;
    for _ in 0..CHAIN {
        let link = unsafe { a_held_object(&mut arena, class) };
        unsafe {
            store_prop(&mut arena, holder, prop_offset(0), link);
            assert!(
                !ll_release(link as *mut RcHeader),
                "the holder's edge keeps the link"
            );
        }
        chain.push(link);
        holder = link;
    }

    take_passes();
    let mut trace = open_arena();
    assert_eq!(
        unsafe { mark::<PlainCells>(&mut trace, root as *mut RcHeader) },
        MarkResult::Complete
    );
    let passes = take_passes();
    let zero = chain
        .iter()
        .all(|&link| unsafe { working_count(link) } == 0);
    trace.reset();
    drop(trace);

    assert_eq!(passes, 1);
    assert!(zero, "every link's one in-edge came off its row");

    unsafe {
        for &link in &chain {
            ll_retain(link as *mut RcHeader);
        }
        for holder in std::iter::once(root).chain(chain.iter().copied()) {
            store_prop(&mut arena, holder, prop_offset(0), std::ptr::null_mut());
        }
        for entity in std::iter::once(root).chain(chain) {
            assert!(ll_release(entity as *mut RcHeader));
            ll_object_die(entity);
        }
    }
    release_queue_segments();
}
