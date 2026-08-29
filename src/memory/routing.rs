//! Where a memory category's bytes come from.
//!
//! The compiler assigns a category to an owner without knowing what kind
//! of entity will live there (`rfc/model/memory/arenas.md`), so "which
//! allocator serves this category" is a question of the memory layer
//! rather than of each factory. It used to be answered by the same
//! `match` written out in eight places — six factories and two
//! out-of-line bodies — and the two body copies had already drifted
//! apart when this module replaced them.
//!
//! Two populations, and they are not interchangeable. An **entity** has
//! an `RcHeader` and goes to `entity_alloc` in the long-lived
//! categories, because the collector reads the first eight bytes of
//! every occupied slot of an entity block as one. A **body** is the
//! bytes an entity owns outside its own slot — a dynamic string's
//! payload, a table's storage — and goes to the buffer arena, whose
//! blocks carry the owner and the stack a foreign free posts to
//! (`rfc/model/memory/buffers.md`). Putting a headerless body in an
//! entity block is the mistake this split exists to prevent.

use crate::memory::Buffer;
use crate::memory::block_pool::BLOCK_PAYLOAD;
use crate::memory::context::{LLContext, resolve_arena};
use crate::memory::immortal::immortal_alloc;
use crate::refcount::MemoryCategory;

/// Uninitialized memory for an **entity** of `category`, `size` bytes.
/// Null when the allocation fails, which every factory turns into a
/// refusal its caller can raise on.
///
/// **A size past what the category's allocator packs into one slot takes
/// a block-aligned allocation of its own** ([`slot_limit`],
/// `memory::large_entity`), so no factory carries a block size of its
/// own and no size a program can reach is refused for being large.
///
/// A category that must not hold a given kind of entity is the caller's
/// to refuse *before* calling — the refusal belongs where its reason is,
/// and `string::ll_string_new_dynamic` is the one that has one.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`].
#[inline]
pub(crate) unsafe fn entity_alloc_in(
    ctx: *mut LLContext,
    category: MemoryCategory,
    size: usize,
) -> *mut u8 {
    match category {
        // The arena's own entity door, which splits at a block payload
        // and logs the run it takes above it. `Arena::alloc` is not that
        // door and keeps refusing: `ll_arena_alloc` reaches it from the C
        // ABI, where an entity and a byte buffer are the same request.
        MemoryCategory::RequestArena => unsafe { (*resolve_arena(ctx)).alloc_entity(size) },
        // Counted entities live in the segregated entity-block population
        // a cycle collector traces (`docs/memory-manager.md`, "Heap: small
        // objects"). LongLived rides along: a trace skips it by category
        // per entity, and the long-lived arena's own reclamation policy is
        // still TBD.
        // Including a size past the largest class: `entity_alloc` makes
        // that split itself, against the same constant [`slot_limit`]
        // reports here. Testing it a second time in this arm would put
        // the number in two places, and `heap::entity_alloc` has other
        // callers that would keep the old answer.
        MemoryCategory::GcHeap | MemoryCategory::LongLived => unsafe {
            crate::memory::heap::entity_alloc(size)
        },
        // No bound of its own: past one block payload `immortal_alloc`
        // stops bump-packing and serves the request from a block-aligned
        // run, which is the same shape `large_entity` builds minus the
        // registry and the parking — an immortal entity is never freed
        // and never walked, so it needs neither.
        MemoryCategory::Immortal => immortal_alloc(size),
    }
}

/// The largest entity slot `category`'s allocator packs **into a block
/// it shares**. A factory asks this rather than a block size: what the
/// bound *is* belongs to the allocator, and it differs by allocator
/// rather than by taste (`dev/DECISIONS.md`, "the entity-slot limit
/// differs by category, and past it nothing is packed").
///
/// Past it an entity is not packed at all — it takes a block-aligned
/// allocation of its own (`memory::large_entity`) — so this is a routing
/// switch rather than a refusal.
///
/// This answers for an entity **slot**, never for a body: an out-of-line
/// body above the same bound is legal and takes the dedicated-run path
/// ([`body_alloc`]).
///
/// The arena's row is enforced twice rather than read here: `Arena::alloc`
/// tests the bound itself, `ll_arena_alloc` reaching it without passing
/// this module, and `Arena::alloc_entity` splits at the same bound.
#[inline]
pub(crate) fn slot_limit(category: MemoryCategory) -> usize {
    match category {
        MemoryCategory::RequestArena | MemoryCategory::Immortal => BLOCK_PAYLOAD,
        MemoryCategory::GcHeap | MemoryCategory::LongLived => crate::memory::heap::MAX_SMALL,
    }
}

/// Memory for an entity's **out-of-line body**, reporting the bytes
/// really granted beside the pointer. A null pointer is a refusal.
///
/// The granted size is what the free below needs: the buffer arena's
/// free is size-carrying and a chunk holds no metadata, so a caller that
/// remembered the requested size instead would hand the difference back
/// to nobody.
///
/// Both arenas split by size — a body over a block payload is a
/// dedicated run — and in both the split belongs to the arena rather
/// than to the caller: a body is sized from a program-visible count, so
/// a caller that made the test itself would be carrying the block size
/// around with it.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`].
#[inline]
pub(crate) unsafe fn body_alloc(
    ctx: *mut LLContext,
    category: MemoryCategory,
    bytes: usize,
) -> (*mut u8, usize) {
    match category {
        MemoryCategory::RequestArena => {
            let p = unsafe { (*resolve_arena(ctx)).alloc_body(bytes) };
            (p, bytes)
        }
        MemoryCategory::GcHeap | MemoryCategory::LongLived => {
            crate::memory::buffer_arena::buffer_alloc_longlived_payload(bytes)
        }
        MemoryCategory::Immortal => (immortal_alloc(bytes), bytes),
    }
}

/// Grow `payload` to hold `min_capacity`, through whichever half of the
/// buffer machinery the category calls for. Both halves extend at their
/// own bump top when the payload is still the last chunk taken from it,
/// and fall back to alloc-new, copy, free-old. False on refusal, with
/// the buffer untouched.
///
/// # Safety
/// `ctx` per [`crate::memory::context::ll_arena_alloc`].
#[inline]
pub(crate) unsafe fn body_ensure(
    ctx: *mut LLContext,
    category: MemoryCategory,
    payload: &mut Buffer,
    min_capacity: usize,
    hint: usize,
) -> bool {
    let grown = match category {
        MemoryCategory::RequestArena => unsafe {
            crate::memory::buffer::buffer_ensure(
                &mut *resolve_arena(ctx),
                payload,
                min_capacity,
                hint,
            )
        },
        MemoryCategory::GcHeap => {
            crate::memory::buffer_arena::buffer_ensure_longlived(payload, min_capacity, hint)
        }

        // Exhaustive rather than a catch-all, and these two refuse. No
        // body in either category grows today — `ll_string_new_dynamic`
        // refuses to build a dynamic string there, and an immortal
        // table's storage is replaced rather than extended — so the arm
        // is unreachable, and the point of writing it out is what a
        // catch-all would do if it ever became reachable: an immortal
        // payload would go to the buffer arena, whose whole machinery
        // answers the question of who frees a chunk, and pin its block
        // for the life of the process; growth there would free the old
        // payload, whose address an immortal reader may have cached
        // forever (`rfc/model/memory/arenas.md`, "Immortal Objects"). A
        // refusal is also what the next category added to the enum
        // should meet here.
        MemoryCategory::LongLived | MemoryCategory::Immortal => {
            debug_assert!(false, "no body in this category grows");
            std::ptr::null_mut()
        }
    };

    !grown.is_null()
}

/// Give back a body from [`body_alloc`]. `capacity` is the **granted**
/// size, not the requested one: the buffer arena's free is size-carrying
/// and a chunk holds no metadata. Null is accepted and does nothing.
///
/// Only the long-lived categories free here. An arena body goes at the
/// reset and an immortal one never goes, so reaching this with either is
/// a no-op rather than a mistake.
///
/// **The category decides, and the block kind cannot replace it**
/// (`dev/DECISIONS.md`, "category routing lives in one module, and the
/// free still needs the category").
///
/// # Safety
/// `(ptr, capacity)` must be exactly one live body from [`body_alloc`]
/// of `category`, or null.
#[inline]
pub(crate) unsafe fn body_free(category: MemoryCategory, ptr: *mut u8, capacity: usize) {
    if ptr.is_null() {
        return;
    }

    match category {
        MemoryCategory::RequestArena | MemoryCategory::Immortal => {}
        // Within this population the block kind *is* the dispatch, and
        // `buffer_free_longlived_payload` makes it: a retained block's
        // bytes are left alone, a chunk parks while a collection reads
        // it, and an OS-direct run is unmapped.
        MemoryCategory::GcHeap | MemoryCategory::LongLived => unsafe {
            crate::memory::buffer_arena::buffer_free_longlived_payload(ptr, capacity)
        },
    }
}

#[cfg(test)]
mod tests;
