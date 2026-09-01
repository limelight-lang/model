//! The collection's working memory: a bump arena over 64 KiB blocks,
//! taken for one collection and returned whole at its end.
//!
//! **Two doors, in this order: the ordinary block pool, then the
//! thread's critical reserve** (`rfc/model/memory/critical-reserve.md`,
//! "The three customers"). The in-line collection is the standard form
//! rather than the emergency one, so most runs begin with no refusal
//! anywhere and a full trace's rows are far beyond any reserve; the
//! critical door is the fallback, and on the pressure path of Y14 it is
//! the first draw, the pool's refusal being what triggered the
//! collection.
//!
//! **A refusal at both doors aborts the collection, and never the
//! process.** That is why the memory is asked for a block at a time
//! through a call that can answer null, rather than reserved as a
//! mapping materialised page by page: a page that fails to materialise
//! reports nothing a caller can catch, and the release profile is built
//! `panic = "abort"` (`rfc/model/gc/rc-cycle.md`, "Where the shadow
//! count lives"; `dev/DECISIONS.md`, "the shadow arena asks the pool
//! first and the critical reserve second, and the virtual reservation
//! goes").
//!
//! # What the arena owes back
//!
//! Every block, at the end of the collection and on the abort path
//! alike, and what the critical door lent goes back to the reserve
//! before the pool sees a block — the retry that follows an abort wants
//! a door that is open.
//!
//! **The shadow-row pointers are nulled earlier than that, and the
//! instant is fixed by the design rather than by convenience.**
//! [`ShadowArena::sweep_touched`] runs at the end of scan, where the
//! trace token is released and where the last touch of any shadow row
//! has already happened. Everything after that store runs untokened, and
//! the slot returns are among it — so a block may reach the pool and be
//! recommissioned while this collection's teardown is still running, and
//! a sweep left until then would write into another collection's header
//! word (`rfc/model/gc/rc-cycle.md`, "Concurrency" and "Death while
//! enrolled"). [`ShadowArena::reset`] sweeps too, and that is the abort
//! path: an abort can only be raised where memory is asked for, which is
//! inside mark and scan, so an aborting collection has not reached the
//! release instant.
//!
//! # What it does not hold
//!
//! A `Vec`, a `HashMap`, or anything else that reaches the global
//! allocator. Both of the arena's own lists live in its own memory: the
//! blocks thread through their headers, and the touched list threads
//! through the row arrays themselves. A collection that grew a `Vec`
//! would allocate through the very door that has already refused, and an
//! allocation failure inside `Vec` aborts the process
//! (`rfc/model/gc/cycle/questions.md`, Y14, "Its working memory must be
//! sized before it is needed").
//!
//! # Enrolment cannot fail after the rows exist
//!
//! A block's rows and its entry in the touched list are **one
//! allocation**: the entry is a 24-byte prologue on the row array
//! (`crate::cycle::shadow`). One refusal point serves both, and it
//! stands before either exists, so the state the sweep exists to undo —
//! a block stamped with rows the abort has given back — cannot be
//! reached. The recorded alternative is a chain of 512-entry segments
//! beside the arrays: it allocates a second time, and that allocation's
//! refusal arrives after the stamp, which is the state above; it also
//! costs 4 KiB at the first touched block against the prologue's 24
//! bytes.
//!
//! A large entity is the one population with no array, its single row
//! being a word of its own block header, and it takes a prologue with no
//! rows behind it for the sake of the sweep. There the refusal is kept
//! harmless by an ordering instead: the row is written only after the
//! enrolment is in hand, so a refused enrolment leaves the row at zero
//! ([`ShadowArena::meet`]).

use crate::cycle::row::{Population, Row};
use crate::cycle::shadow::{self, Colour, RowArray};
#[cfg(test)]
use crate::memory::block_pool::BlockPool;
use crate::memory::block_pool::{BLOCK_PAYLOAD, BlockHeader};
use crate::memory::gc_metadata;

/// What one meeting of an entity answers: its row, or the two reasons
/// there is none.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Met {
    /// The entity's row, met — its colour is not
    /// [`Colour::Untouched`] and its working count is the refcount less
    /// whatever the trace has already subtracted. The caller writes
    /// through it.
    ///
    /// **`first_reach` is the answer to "had this collection seen the
    /// entity before?"**, and it is carried out of the meeting because
    /// the meeting is what destroys it: after the call the row's colour
    /// says met whichever reach this was. The mark's descent turns on
    /// it — an edge into an entity already expanded takes the
    /// decrement and stops, and a trace that expanded again on every
    /// in-edge would not terminate on a ring
    /// (`rfc/model/gc/rc-cycle.md`, "What replaces the walk").
    Row { row: *mut u32, first_reach: bool },
    /// The block cannot place this index, which is a retained block
    /// whose object index no longer names the entity. The caller counts
    /// the edge as an external live reference, the same answer
    /// `row::edge_to` gives an address it cannot place.
    Unplaced,
    /// Both memory doors refused. The caller aborts the collection,
    /// which costs nothing beyond the work already done: the trace
    /// writes into no entity.
    Refused,
}

/// One collection's memory. Built on the collecting thread's stack,
/// spent by that collection, and returned by [`ShadowArena::reset`].
pub(crate) struct ShadowArena {
    /// Blocks held, threaded through `BlockHeader::next`, newest first.
    blocks: *mut BlockHeader,
    /// How many of them came through the critical door, and therefore
    /// how many go back to the reserve rather than to the pool. The
    /// arena does not record *which*: a block is a block, and the count
    /// is what restores the reserve's size.
    from_reserve: usize,
    /// The bump cursor into the newest block, and the bytes left in it.
    cursor: *mut u8,
    left: usize,
    /// Newest array of the touched list, or null while no block has
    /// been touched.
    touched: *mut RowArray,
}

impl ShadowArena {
    /// An arena holding nothing. Allocates no memory: a collection that
    /// finds no candidate pays for no block.
    pub(crate) fn new() -> Self {
        Self {
            blocks: std::ptr::null_mut(),
            from_reserve: 0,
            cursor: std::ptr::null_mut(),
            left: 0,
            touched: std::ptr::null_mut(),
        }
    }

    /// `bytes` of 8-aligned scratch, or **null when both doors have
    /// refused**, which is the caller's signal to abort the collection.
    ///
    /// A request larger than one block payload is refused outright: no
    /// allocation this arena serves comes near it — a smallest-class
    /// block's array is 16 408 bytes, the largest any population asks
    /// for — and a run of blocks would give the abort path a second
    /// shape to return.
    ///
    /// The memory is **not zeroed**. A row array arrives dirty by
    /// design, its met bitmap being what says which rows have meaning
    /// (`rfc/model/gc/rc-cycle.md`, "The rows are not zeroed greedily").
    pub(crate) fn alloc(&mut self, bytes: usize) -> *mut u8 {
        // Rounded through a checked add, because the refusal below is
        // what the caller reads and an overflow would reach it as a
        // small number: with overflow checks it panics on the one path
        // that must abort a collection instead, and without them it
        // wraps to zero and grants a pointer the caller believes is
        // enormous.
        let Some(rounded) = bytes.checked_add(7) else {
            return std::ptr::null_mut();
        };

        let bytes = rounded & !7;
        if bytes > BLOCK_PAYLOAD {
            return std::ptr::null_mut();
        }

        if bytes > self.left && !self.grow() {
            return std::ptr::null_mut();
        }

        let granted = self.cursor;
        self.cursor = unsafe { granted.add(bytes) };
        self.left -= bytes;
        granted
    }

    /// The shadow row of one entity the trace has just reached, **met**:
    /// initialised from `refcount` on this collection's first reach of it
    /// and left as it stands on every later one, so a second edge into
    /// the same entity subtracts from the count rather than restoring it.
    ///
    /// `row` comes from [`row::edge_to`](crate::cycle::row::edge_to) and
    /// `refcount` is the entity's, read by the caller: this module places
    /// rows and knows nothing about entity headers.
    ///
    /// **Change this, change [`met_row`] too:** the two find the same row
    /// in the same two places, and only this one may create it.
    ///
    /// The block's rows are reserved here, at its first touch, and the
    /// block enrolled for the sweep with them — one allocation, so a
    /// refusal cannot land between the two (module doc). What it does not
    /// do is zero the rows: only the touched group is written, and the
    /// group bitmap is what says which groups those are
    /// (`crate::cycle::shadow`).
    ///
    /// **A large entity is safe by an ordering rather than by that
    /// structure**, its row being a block header word that exists from
    /// the block's commissioning: the colour is tested, then the
    /// enrolment allocated, then the colour written, so a refusal leaves
    /// the row at zero and an unenrolled block is also an unwritten one.
    /// An edit that wrote the row before enrolling would leave the next
    /// collection reading this one's count.
    ///
    /// # Safety
    /// `row` must name a live entity of the collected heap, resolved from
    /// its own address by `edge_to`, and its block must stay mapped until
    /// [`sweep_touched`](Self::sweep_touched) runs — which it does, a
    /// trace in flight being what keeps a block from reaching the pool
    /// (`rfc/model/gc/rc-cycle.md`, "Death while enrolled").
    pub(crate) unsafe fn meet(&mut self, row: Row, refcount: u32) -> Met {
        let block = row.block as *mut u8;
        let word = if row.population == Population::Sole {
            // The one population whose row is a block header word rather
            // than an array, so its enrolment has no allocation to ride
            // on and takes a prologue of its own. The row's own colour is
            // what says whether that has happened: nothing but a meeting
            // writes it, and the tail below writes it before returning.
            debug_assert_eq!(
                row.index,
                crate::cycle::row::SOLE_OCCUPANT,
                "a large entity's block holds one row and this names another"
            );
            let word = unsafe { crate::memory::large_entity::shadow_row(block) };
            if shadow::colour(unsafe { *word }) == Colour::Untouched
                && self.enrol(block, 0, Population::Sole).is_null()
            {
                return Met::Refused;
            }

            word
        } else {
            let mut array = unsafe { crate::memory::heap::block_shadow(block) } as *mut RowArray;
            if array.is_null() {
                let Some(slots) = (unsafe { index_space(row) }) else {
                    return Met::Unplaced;
                };

                array = self.enrol(block, slots, row.population);
                if array.is_null() {
                    return Met::Refused;
                }

                // After the enrolment and never before: this store cannot
                // fail and the one above can, so the other order stamps a
                // block the abort would then leave pointing at memory it
                // has given back (module doc).
                unsafe { crate::memory::heap::set_block_shadow(block, array as *mut u8) };
            }

            if row.index >= unsafe { (*array).slots } {
                // A retained block whose object index has been rebuilt
                // under this trace is the only way here, and the trace
                // token forbids it. Conservative rather than fatal all
                // the same: an edge with no row keeps its referent alive.
                debug_assert!(false, "row {} is past the block's index space", row.index);
                return Met::Unplaced;
            }

            unsafe { shadow::meet_group(array, row.index) };
            unsafe { shadow::row(array, row.index) }
        };

        let first_reach = shadow::colour(unsafe { *word }) == Colour::Untouched;
        if first_reach {
            unsafe { word.write(shadow::compose(Colour::Met, refcount)) };
        }

        Met::Row {
            row: word,
            first_reach,
        }
    }

    /// Reserve `slots` rows for `block` and enrol it for the sweep, or
    /// null when both memory doors have refused.
    ///
    /// The array is linked into the touched list here, which is the
    /// enrolment: it is the same memory, so the two cannot come apart.
    /// `slots` is zero for a large entity, whose row is elsewhere and
    /// whose prologue is enrolment alone.
    fn enrol(&mut self, block: *mut u8, slots: u32, population: Population) -> *mut RowArray {
        let array = self.alloc(shadow::bytes_for(slots)) as *mut RowArray;
        if array.is_null() {
            return array;
        }

        unsafe { shadow::init(array, block, slots, population, self.touched) };
        self.touched = array;
        array
    }

    /// End the collection's hold on memory: give every block back, the
    /// reserve first, having swept anything
    /// [`sweep_touched`](Self::sweep_touched) has not.
    ///
    /// This is the whole of the abort path. A collection that gave up
    /// halfway calls it and has left nothing behind — the trace writes
    /// into no entity, so the heap is byte-identical and the only state
    /// to undo is the pointers the sweep nulls.
    ///
    /// Idempotent, and [`Drop`] calls it, so a collection that unwinds
    /// under a profile that unwinds leaks no block. The release profile
    /// aborts instead, so on that build every exit of a collection owes
    /// this call explicitly.
    ///
    /// **Change this, change the worklist too:** a
    /// [`TraceStack`](crate::cycle::stack::TraceStack) that drew
    /// segments from this arena names memory the pool has taken back
    /// from the moment this returns, and its own `reset` is what says
    /// so.
    pub(crate) fn reset(&mut self) {
        self.sweep_touched();

        // What the reserve lent goes back to the reserve, and the rest
        // to the pool. Returning everything through the reserve's door
        // would refill it out of ordinary memory a collection happened
        // to be holding, which is the safepoint's job and not this one;
        // returning everything to the pool would leave the reserve empty
        // for the retry that follows an abort, and the retry is why the
        // ordering exists at all.
        // The arena's own state moves ahead of each hand-over rather
        // than after the loop: `BlockPool::put` takes a mutex, and a
        // thread that unwinds out of a poisoned one leaves `Drop` to run
        // `reset` again — over a list whose head was already returned.
        self.cursor = std::ptr::null_mut();
        self.left = 0;
        while !self.blocks.is_null() {
            let block = self.blocks;
            self.blocks = unsafe { (*block).next };
            if self.from_reserve > 0 {
                self.from_reserve -= 1;
                gc_metadata::release_to_critical(block);
            } else {
                gc_metadata::release(block);
            }
        }

        self.from_reserve = 0;
    }

    /// Null the shadow-row pointer of every block this collection
    /// enrolled, and empty the list.
    ///
    /// **Called at the end of scan**, where the trace token is released:
    /// that is the last instant at which the blocks are guaranteed still
    /// to be this collection's, because the slot returns that follow the
    /// release can hand one to the pool and another collection can
    /// recommission it (module doc). [`reset`](Self::reset) calls it
    /// again, which is the abort path and a second call over an emptied
    /// list.
    ///
    /// The rows themselves need no undoing: mark and scan write into no
    /// entity, so the pointer is the whole of what a collection leaves in
    /// the heap.
    pub(crate) fn sweep_touched(&mut self) {
        let mut array = self.touched;
        // Emptied first: the walk below runs to the end of the chain,
        // and a second call must find nothing rather than repeat it.
        self.touched = std::ptr::null_mut();

        while !array.is_null() {
            let block = unsafe { (*array).block };
            match unsafe { (*array).population } {
                // The large entity's row is the block's own header word,
                // so what a stale one costs is not a wild pointer but a
                // count: the next collection would read the entity as met
                // and subtract from a working count this one left behind.
                Population::Sole => unsafe {
                    crate::memory::large_entity::shadow_row(block)
                        .write(shadow::compose(Colour::Untouched, 0))
                },
                // Listed rather than a wildcard: a fourth population
                // would otherwise be swept as though its rows hung off
                // the collector line, which is a store into a header word
                // that may be another module's.
                Population::Slotted | Population::Retained => unsafe {
                    crate::memory::heap::clear_block_shadow(block)
                },
            }

            array = unsafe { (*array).next };
        }
    }

    /// Take one more block, or answer false when both doors refuse.
    ///
    /// What is left of the previous block is abandoned. A bump that
    /// searched its older blocks for a fit would be a free list, and the
    /// arena's whole life is one collection.
    fn grow(&mut self) -> bool {
        let mut block = gc_metadata::acquire();
        if block.is_null() {
            block = gc_metadata::adopt(crate::memory::critical::draw());
            if block.is_null() {
                return false;
            }

            self.from_reserve += 1;
        }

        unsafe { (&raw mut (*block).next).write(self.blocks) };
        self.blocks = block;
        self.cursor = BlockHeader::payload_start(block);
        self.left = BLOCK_PAYLOAD;
        true
    }

    /// Blocks enrolled for the sweep. Tests only, and the instrument for
    /// a defect nothing else reports: a block enrolled twice is swept
    /// twice, which is the same store again, so only the length of the
    /// chain shows it.
    #[cfg(test)]
    pub(crate) fn touched_blocks(&self) -> usize {
        let mut count = 0;
        let mut array = self.touched;
        while !array.is_null() {
            count += 1;
            array = unsafe { (*array).next };
        }

        count
    }

    /// Blocks this arena holds. Tests only: the number is what a leak
    /// looks like from outside.
    #[cfg(test)]
    pub(crate) fn blocks_held(&self) -> usize {
        let mut count = 0;
        let mut block = self.blocks;
        while !block.is_null() {
            count += 1;
            block = unsafe { (*block).next };
        }
        count
    }
}

/// The shadow row of an entity this collection has **met**, or `None`
/// when it has not: a block the trace never touched, a group it never
/// zeroed, an index past the block's array, or a row still coloured
/// [`Colour::Untouched`].
///
/// The read-only twin of [`ShadowArena::meet`] — same three populations
/// and same two places a row can be — and it neither allocates nor
/// writes, which is what the scan needs: a meeting would initialise the
/// row of an entity the mark never reached from a refcount nothing has
/// subtracted from, and that row would then be condemned or spared on a
/// count the trace never computed.
///
/// # Safety
/// `row` names a live entity of the collected heap, resolved from its
/// own address by `edge_to`, and its block is still this collection's.
pub(crate) unsafe fn met_row(row: Row) -> Option<*mut u32> {
    let block = row.block as *mut u8;
    let word = if row.population == Population::Sole {
        debug_assert_eq!(
            row.index,
            crate::cycle::row::SOLE_OCCUPANT,
            "a large entity's block holds one row and this names another"
        );
        unsafe { crate::memory::large_entity::shadow_row(block) }
    } else {
        let array = unsafe { crate::memory::heap::block_shadow(block) } as *mut RowArray;
        if array.is_null() {
            return None;
        }

        if row.index >= unsafe { (*array).slots } {
            // The state `meet` asserts on, and it is asserted here for
            // the same reason: only a retained block whose object index
            // was rebuilt under this trace reaches it, which the trace
            // token forbids. A silent `None` here would leave the mark
            // aborting loudly on the state and the scan passing over it.
            debug_assert!(false, "row {} is past the block's index space", row.index);
            return None;
        }

        if !unsafe { shadow::group_is_met(array, row.index) } {
            return None;
        }

        unsafe { shadow::row(array, row.index) }
    };

    match shadow::colour(unsafe { *word }) {
        Colour::Untouched => None,
        _ => Some(word),
    }
}

/// How many rows `row`'s block needs, or `None` for a retained block
/// that has no object index — a block held for a payload alone, or one
/// whose reset has not registered it yet.
///
/// The two populations answer from different places, and only one of them
/// takes a lock: an entity block states its size class in its own
/// collector line, while a retained block's index space is the length of
/// an array behind the registry's mutex, which is why it is asked once
/// per block here rather than once per edge.
///
/// # Safety
/// `row`'s block must be commissioned as the population says it is.
unsafe fn index_space(row: Row) -> Option<u32> {
    match row.population {
        Population::Slotted => {
            Some(unsafe { crate::memory::heap::collector_block_slots(row.block as *mut u8) })
        }
        Population::Retained => {
            crate::memory::retained::occupant_count(row.block).map(|count| count as u32)
        }
        // Unreachable: `meet` answers the sole occupant's row from its
        // block header without asking where an array would go.
        Population::Sole => None,
    }
}

impl Drop for ShadowArena {
    /// The net under an unwind. On the contract path the collection has
    /// already called [`reset`](ShadowArena::reset) and this finds
    /// nothing; a test that panics mid-collection would otherwise leave
    /// the pool short for every test after it.
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests;
