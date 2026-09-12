//! The loads S40.3 reads, built once for the two arms that read them
//! (`PLAN.md` S40.3): the census in the test build, and the driver in
//! `benches/` that links the ordinary library under the `bench-loads`
//! feature.
//!
//! # The shape
//!
//! A ring of `members` objects of one size class, linked through their first
//! property, every member registered as a candidate by spending its creation
//! reference through a non-final release; a keeper of another size class
//! holding the first member, so that the ring is live and every collection
//! traces it whole; and, between two consecutive members, `fillers` objects
//! of the members' class that nothing references, which is what places the
//! members apart inside their blocks without giving the trace anything more
//! to reach. A second edge, when asked for, links each member to the member
//! after its successor through the second property, which doubles the edges
//! over the same vertices.
//!
//! The retained shape is the same ring built in the request arena and moved
//! by the arena's reset into retained blocks, whose index space is the
//! survivor list rather than the slot stride; there the keeper is a GC-heap
//! object and the only registration, as `cycle::density`'s retained arm has
//! it, and the members are reached through it.
//!
//! # What a load is not
//!
//! A workload. The liveness, the placement and the edge count are inputs;
//! what the crate chooses, and the only thing a reading over one of these
//! measures, is where the heap placed the members across blocks and what the
//! collector's own arithmetic then cost over them.
//!
//! # Teardown is a collection
//!
//! The keeper lets go and the next collection reads the ring as garbage and
//! tears it down, which is the production path and the only one this module
//! has: the test-only clearing of the candidate bit is not available to the
//! driver. The fillers die by their own release.

use crate::class::{Class, ClassBuilder};
use crate::memory::arena::Arena;
use crate::memory::barrier::ref_store;
use crate::memory::block_pool::BLOCK_PAYLOAD;
use crate::memory::context::LLContext;
use crate::object::{Object, ll_object_constructed, ll_object_die, ll_object_new};
use crate::refcount::{MemoryCategory, RcHeader, ll_release, ll_retain};
use crate::value::{Tag, Value};

/// Where the members are built, which decides which population's rows the
/// trace reads them through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Population {
    /// The GC heap: an entity block's slots.
    Ordinary,
    /// The request arena, reset into retained blocks before the first
    /// collection.
    Retained,
}

/// One load's construction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Load {
    /// The members' size class, in bytes; one of the heap's size classes.
    pub class_bytes: usize,
    /// Members in the ring.
    pub members: usize,
    /// Unreferenced objects of the members' class allocated between two
    /// consecutive members. Zero packs the ring; one less than the block's
    /// slot count puts one member in every block.
    pub fillers: usize,
    pub population: Population,
    /// Whether each member also holds the member after its successor.
    pub second_edge: bool,
}

/// What [`build`] handed back: the objects, so that the reading can ask
/// about them and the teardown can let them go.
pub struct Built {
    pub arena: Arena,
    pub keeper: *mut Object,
    pub members: Vec<*mut Object>,
    fillers: Vec<*mut Object>,
}

/// Slots a block of `class_bytes` holds.
pub const fn slots_per_block(class_bytes: usize) -> usize {
    BLOCK_PAYLOAD / class_bytes
}

/// The keeper's size class: 48 bytes, which is none of the design's four,
/// so the keeper never takes a slot of a block the members are placed in.
pub const KEEPER_CLASS_BYTES: usize = 48;

/// Reference-carrying properties a class of `bytes` holds: sixteen for the
/// header and the class word, sixteen per property.
const fn props_for(bytes: usize) -> usize {
    (bytes - 16) / 16
}

/// The offset of a class's `index`-th declared property.
const fn prop_offset(index: u32) -> u32 {
    16 + 16 * index
}

/// A class of `props` reference-carrying properties, named after the load
/// so that two loads never share one.
fn a_class(name: &str, props: usize) -> *const Class {
    let mut builder = ClassBuilder::new(name);
    let names: Vec<String> = (0..props).map(|i| format!("p{i}")).collect();
    for name in &names {
        builder = builder.prop(name, true);
    }

    builder.build()
}

/// An object of `class` in `category`, constructed the way generated code
/// constructs one.
///
/// # Safety
/// `arena` is this thread's and `class` is built.
unsafe fn an_object(
    arena: *mut Arena,
    class: *const Class,
    category: MemoryCategory,
) -> *mut Object {
    let mut context = LLContext { arena };
    let object = unsafe { ll_object_new(&mut context, class, category) };
    assert!(!object.is_null(), "the allocator served the load");
    assert!(
        unsafe { ll_object_constructed(&mut context, object) },
        "the destructor registration was served"
    );
    object
}

/// Store `value` into `holder`'s property at `offset` through the barrier,
/// as generated code would; null lets the property's entity go.
///
/// # Safety
/// `holder` is a live object of a class declaring the property, `value` is
/// null or live, and `arena` is this thread's.
pub unsafe fn store(arena: *mut Arena, holder: *mut Object, offset: u32, value: *mut Object) {
    unsafe {
        let slot = Object::prop_at(holder, offset);
        let old = slot.read().entity_or_null();
        let new = if value.is_null() {
            Value::null()
        } else {
            Value::entity(Tag::Object, value as *mut RcHeader)
        };
        assert!(ref_store(arena, holder as *mut RcHeader, slot, old, new));
    }
}

/// Build `load` on this thread's heap: the ring, its keeper and its fillers,
/// the members registered, and for the retained population the reset done.
///
/// # Safety
/// The caller runs on a thread the runtime registered, with no other load
/// standing on it.
pub unsafe fn build(load: Load) -> Built {
    assert!(
        !load.second_edge || props_for(load.class_bytes) >= 2,
        "a second edge needs a second property"
    );
    let member_class = a_class(
        &format!(
            "Load{}c{}x{}{:?}{}",
            load.class_bytes,
            load.members,
            load.fillers,
            load.population,
            if load.second_edge { "e2" } else { "" }
        ),
        props_for(load.class_bytes),
    );
    let keeper_class = a_class(
        &format!(
            "LoadKeeper{}c{}x{}{:?}",
            load.class_bytes, load.members, load.fillers, load.population
        ),
        props_for(KEEPER_CLASS_BYTES),
    );
    let category = match load.population {
        Population::Ordinary => MemoryCategory::GcHeap,
        Population::Retained => MemoryCategory::RequestArena,
    };

    let mut arena = Arena::new();
    let arena_ptr: *mut Arena = &mut arena;
    let keeper = unsafe { an_object(arena_ptr, keeper_class, MemoryCategory::GcHeap) };

    let mut members = Vec::with_capacity(load.members);
    let mut fillers = Vec::with_capacity(load.members * load.fillers);
    for position in 0..load.members {
        members.push(unsafe { an_object(arena_ptr, member_class, category) });
        if position + 1 < load.members {
            for _ in 0..load.fillers {
                fillers.push(unsafe { an_object(arena_ptr, member_class, category) });
            }
        }
    }

    for (position, &member) in members.iter().enumerate() {
        let next = members[(position + 1) % load.members];
        unsafe { store(arena_ptr, member, prop_offset(0), next) };
        if load.second_edge {
            let after_next = members[(position + 2) % load.members];
            unsafe { store(arena_ptr, member, prop_offset(1), after_next) };
        }
    }

    unsafe { store(arena_ptr, keeper, prop_offset(0), members[0]) };

    match load.population {
        Population::Ordinary => {
            // The creation references go: every member is held by the ring
            // and registered by the decrement.
            for &member in &members {
                assert!(
                    !unsafe { ll_release(member as *mut RcHeader) },
                    "the ring's edge stands, so the release is not the last"
                );
            }
        }
        Population::Retained => {
            // The reset moves the ring into retained blocks through the
            // keeper's edge, and the fillers die with the arena. The keeper
            // is the one registration, taken by a non-final release under a
            // retain.
            unsafe { crate::promote::arena_reset_full(arena_ptr) };
            fillers.clear();
            unsafe {
                ll_retain(keeper as *mut RcHeader);
                assert!(
                    !ll_release(keeper as *mut RcHeader),
                    "the retain stands, so this release is not the last"
                );
            }
        }
    }

    Built {
        arena,
        keeper,
        members,
        fillers,
    }
}

/// Let the ring go: the keeper's edge is nulled, which registers the first
/// member if it was not, so that the next collection traces the ring from
/// it, reads it as garbage and tears it down; the keeper and the fillers die
/// here.
///
/// A retained ring is torn down only while it is not mature: after
/// `TRAVERSAL_AGE_THRESHOLD` commits its members carry the stamp and none is
/// a candidate, so the trace from the first member stops at the second and
/// the ring waits for the turnover (`crate::cycle::mark`, "The mature live
/// core is not descended into").
///
/// # Safety
/// `built` came from [`build`] on this thread and no collection is running.
pub unsafe fn release_ring(built: &mut Built) {
    let arena: *mut Arena = &mut built.arena;
    unsafe {
        store(arena, built.keeper, prop_offset(0), std::ptr::null_mut());
        // The keeper's creation reference is its last one: released here,
        // its death releases nothing, the edge being null already.
        assert!(ll_release(built.keeper as *mut RcHeader));
        ll_object_die(built.keeper);
    }

    for filler in built.fillers.drain(..) {
        unsafe {
            assert!(ll_release(filler as *mut RcHeader));
            ll_object_die(filler);
        }
    }
}
