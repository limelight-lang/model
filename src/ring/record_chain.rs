//! A chain of ring blocks whose four words stand in a mutator's record: the
//! form the collector's chain of roots it read live takes
//! (`crate::cycle::chain`; `dev/design/the-collector-keeps-the-live-roots.md`).
//!
//! The blocks are the ring's, so a chain splices into R with no copy
//! ([`Writer::splice_after_tail`]) once [`RecordChain::take_compacted`] has
//! packed it. Inside a block the entries stand between the reader's `front`,
//! which [`RecordChain::commit`] advances past what a batch took, and the
//! writer's `tail`, which [`RecordChain::push`] advances. An entry of zero is
//! a tombstone: a root the death check took out of the chain in place, which
//! every reading skips and which [`RecordChain::len`] does not count.
//!
//! **One holder at a time.** Every method but [`RecordChain::len`] and
//! [`RecordChain::oldest_stamp`] is the token holder's: the collector under
//! its grant, or the mutator under its own claim, and the token's hand-over
//! orders one holder's stores before the next one's loads. The two readings
//! are atomic loads the collector's round makes under its reading hold with
//! no token, and they read no block.

use super::*;

/// The words of one chain: its first and last blocks and the entries it
/// holds, tombstones not counted.
pub(crate) struct RecordChain {
    first: AtomicPtr<BlockHeader>,
    last: AtomicPtr<BlockHeader>,
    entries: AtomicUsize,
    /// The first block's stamp, kept here so that the round reads it without
    /// reading a block; `u64::MAX` for a chain with no block.
    oldest: std::sync::atomic::AtomicU64,
}

/// Blocks detached from the front of a chain, in their order, with the
/// entries they hold.
pub(crate) struct Detached {
    first: *mut BlockHeader,
    last: *mut BlockHeader,
    entries: usize,
}

/// What a peek copied: the entries, and the positions it spanned in the
/// chain, tombstones included, which the commit advances by.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChainPeek {
    pub(crate) copied: usize,
    positions: usize,
}

/// A death check's answer for one entry.
pub(crate) enum Checked {
    /// The entry stays.
    Keep,
    /// The entry is taken out of the chain: tombstoned in place.
    Take,
    /// The entry stays and the check ends on it, the cursor kept on the
    /// entry, so the next check reads it first.
    KeepAndStop,
}

impl RecordChain {
    pub(crate) const fn empty() -> Self {
        Self {
            first: AtomicPtr::new(std::ptr::null_mut()),
            last: AtomicPtr::new(std::ptr::null_mut()),
            entries: AtomicUsize::new(0),
            oldest: std::sync::atomic::AtomicU64::new(u64::MAX),
        }
    }

    /// Entries the chain holds, tombstones not counted: a load anyone may
    /// make, exact only under the token.
    pub(crate) fn len(&self) -> usize {
        self.entries.load(Ordering::Relaxed)
    }

    /// The first block's stamp, or `u64::MAX` for a chain with no block: a
    /// load anyone may make.
    pub(crate) fn oldest_stamp(&self) -> u64 {
        self.oldest.load(Ordering::Relaxed)
    }

    /// Whether the chain holds a block, tombstones only or not.
    ///
    /// # Safety
    /// The caller holds the token.
    pub(crate) unsafe fn has_a_block(&self) -> bool {
        !self.first.load(Ordering::Relaxed).is_null()
    }

    fn set_first(&self, first: *mut BlockHeader) {
        self.first.store(first, Ordering::Relaxed);
        let stamp = if first.is_null() {
            u64::MAX
        } else {
            unsafe { *(*ring(first)).link.stamp.get() }
        };
        self.oldest.store(stamp, Ordering::Relaxed);
    }

    /// Append `entry`, stamping a fresh block with `stamp`; the block comes
    /// from `fresh`, and null from it is [`NoBlock`] with nothing written.
    ///
    /// # Safety
    /// The caller holds the token.
    pub(crate) unsafe fn push(
        &self,
        entry: usize,
        stamp: u64,
        fresh: impl FnOnce() -> *mut BlockHeader,
    ) -> Result<(), NoBlock> {
        debug_assert_ne!(entry, 0, "a zero entry is a tombstone");
        let last = self.last.load(Ordering::Relaxed);
        if !last.is_null() {
            let l = ring(last);
            let tail = unsafe { (*l).writer.tail.load(Ordering::Relaxed) };
            if tail < BLOCK_ENTRIES {
                unsafe {
                    *(*l).slots[tail].get() = entry;
                    (*l).writer.tail.store(tail + 1, Ordering::Relaxed);
                    *(*l).reader.local_tail.get() = tail + 1;
                }
                self.entries.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
        }

        let block = fresh();
        if block.is_null() {
            return Err(NoBlock);
        }

        unsafe { init_block(block) };
        let b = ring(block);
        unsafe {
            *(*b).link.stamp.get() = stamp;
            *(*b).link.checked.get() = 0;
            *(*b).slots[0].get() = entry;
            (*b).writer.tail.store(1, Ordering::Relaxed);
            *(*b).reader.local_tail.get() = 1;
        }
        if last.is_null() {
            self.set_first(block);
        } else {
            unsafe { (*ring(last)).link.next.store(block, Ordering::Relaxed) };
        }
        self.last.store(block, Ordering::Relaxed);
        self.entries.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Detach the leading blocks whose stamp `due` answers true for, in
    /// their order. `stop` is asked before each block, and a true answer
    /// detaches what was read before it.
    ///
    /// # Safety
    /// The caller holds the token.
    pub(crate) unsafe fn detach_while(
        &self,
        due: impl Fn(u64) -> bool,
        mut stop: impl FnMut() -> bool,
    ) -> Option<Detached> {
        let first = self.first.load(Ordering::Relaxed);
        let mut last = std::ptr::null_mut();
        let mut entries = 0;
        let mut block = first;
        while !block.is_null() && due(unsafe { *(*ring(block)).link.stamp.get() }) && !stop() {
            entries += unsafe { live_entries(block) };
            last = block;
            block = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
        }

        if last.is_null() {
            return None;
        }

        unsafe {
            (*ring(last))
                .link
                .next
                .store(std::ptr::null_mut(), Ordering::Relaxed);
        }
        self.set_first(block);
        if block.is_null() {
            self.last.store(std::ptr::null_mut(), Ordering::Relaxed);
        }
        self.entries.fetch_sub(entries, Ordering::Relaxed);
        Some(Detached {
            first,
            last,
            entries,
        })
    }

    /// Hand every leading block that holds no entry but tombstones to
    /// `give_back`. `stop` is asked before each block, and a true answer
    /// leaves the rest.
    ///
    /// # Safety
    /// The caller holds the token.
    pub(crate) unsafe fn give_back_leading_empty_blocks(
        &self,
        mut give_back: impl FnMut(*mut BlockHeader),
        mut stop: impl FnMut() -> bool,
    ) {
        let mut block = self.first.load(Ordering::Relaxed);
        while !block.is_null() && !stop() && unsafe { live_entries(block) } == 0 {
            let next = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
            self.set_first(next);
            if next.is_null() {
                self.last.store(std::ptr::null_mut(), Ordering::Relaxed);
            }
            give_back(block);
            block = next;
        }
    }

    /// Append `detached` after the last block.
    ///
    /// # Safety
    /// The caller holds the token.
    pub(crate) unsafe fn append(&self, detached: Detached) {
        let last = self.last.load(Ordering::Relaxed);
        if last.is_null() {
            self.set_first(detached.first);
        } else {
            unsafe {
                (*ring(last))
                    .link
                    .next
                    .store(detached.first, Ordering::Relaxed)
            };
        }
        self.last.store(detached.last, Ordering::Relaxed);
        self.entries.fetch_add(detached.entries, Ordering::Relaxed);
    }

    /// Copy up to `out.len()` entries from the front into `out`, oldest
    /// first, tombstones skipped; nothing leaves the chain before
    /// [`commit`](Self::commit).
    ///
    /// # Safety
    /// The caller holds the token.
    pub(crate) unsafe fn peek(&self, out: &mut [usize]) -> ChainPeek {
        let mut peek = ChainPeek::default();
        let mut block = self.first.load(Ordering::Relaxed);
        while !block.is_null() && peek.copied < out.len() {
            let b = ring(block);
            let (front, tail) = unsafe {
                (
                    (*b).reader.front.load(Ordering::Relaxed),
                    (*b).writer.tail.load(Ordering::Relaxed),
                )
            };
            let mut index = front;
            while index < tail && peek.copied < out.len() {
                let entry = unsafe { *(*b).slots[index].get() };
                if entry != 0 {
                    out[peek.copied] = entry;
                    peek.copied += 1;
                }
                index += 1;
                peek.positions += 1;
            }
            block = unsafe { (*b).link.next.load(Ordering::Relaxed) };
        }

        peek
    }

    /// Advance the front past what `peek` spanned, handing every block it
    /// consumed whole to `give_back`.
    ///
    /// # Safety
    /// The caller holds the token, and `peek` is this chain's last peek, with
    /// no commit and no detach since.
    pub(crate) unsafe fn commit(
        &self,
        peek: ChainPeek,
        mut give_back: impl FnMut(*mut BlockHeader),
    ) {
        let mut positions = peek.positions;
        let mut block = self.first.load(Ordering::Relaxed);
        while positions > 0 {
            debug_assert!(!block.is_null(), "a commit past the chain");
            let b = ring(block);
            let (front, tail) = unsafe {
                (
                    (*b).reader.front.load(Ordering::Relaxed),
                    (*b).writer.tail.load(Ordering::Relaxed),
                )
            };
            let step = positions.min(tail - front);
            unsafe { (*b).reader.front.store(front + step, Ordering::Relaxed) };
            positions -= step;
            if front + step < tail {
                break;
            }

            let next = unsafe { (*b).link.next.load(Ordering::Relaxed) };
            self.set_first(next);
            if next.is_null() {
                self.last.store(std::ptr::null_mut(), Ordering::Relaxed);
            }
            give_back(block);
            block = next;
        }
        self.entries.fetch_sub(peek.copied, Ordering::Relaxed);
    }

    /// Read up to `budget` entries past the check's cursor, block by block
    /// from the first, and answer each by `visit`: an entry taken is
    /// tombstoned, and [`Checked::KeepAndStop`] ends the check with the
    /// cursor on the entry it answered. A lap that finds every block read to
    /// its tail starts the cursors again from each block's front. Answers the
    /// entries read; `stop` is asked before each, and a true answer ends the
    /// check where it stands, the cursor kept.
    ///
    /// # Safety
    /// The caller holds the token.
    pub(crate) unsafe fn check(
        &self,
        budget: usize,
        mut stop: impl FnMut() -> bool,
        mut visit: impl FnMut(usize) -> Checked,
    ) -> usize {
        let mut read = 0;
        let mut lapped = false;
        loop {
            let mut block = self.first.load(Ordering::Relaxed);
            let mut met_one = false;
            while !block.is_null() {
                let b = ring(block);
                let tail = unsafe { (*b).writer.tail.load(Ordering::Relaxed) };
                let front = unsafe { (*b).reader.front.load(Ordering::Relaxed) };
                let checked = unsafe { &mut *(*b).link.checked.get() };
                *checked = (*checked).max(front);
                while *checked < tail {
                    met_one = true;
                    if read == budget || stop() {
                        return read;
                    }

                    let slot = unsafe { &mut *(*b).slots[*checked].get() };
                    if *slot != 0 {
                        read += 1;
                        match visit(*slot) {
                            Checked::Keep => {}
                            Checked::Take => {
                                *slot = 0;
                                self.entries.fetch_sub(1, Ordering::Relaxed);
                            }
                            Checked::KeepAndStop => return read,
                        }
                    }
                    *checked += 1;
                }
                block = unsafe { (*b).link.next.load(Ordering::Relaxed) };
            }

            if met_one || lapped {
                return read;
            }

            // Every block read to its tail: the lap ends here, and the next
            // starts from each block's front.
            lapped = true;
            let mut block = self.first.load(Ordering::Relaxed);
            while !block.is_null() {
                let b = ring(block);
                unsafe { *(*b).link.checked.get() = 0 };
                block = unsafe { (*b).link.next.load(Ordering::Relaxed) };
            }
        }
    }

    /// The chain's blocks packed for a splice into R — each block's entries
    /// moved to its start, tombstones and consumed positions dropped, a block
    /// left with none handed to `give_back` — first and last, leaving the
    /// chain empty; `None` for a chain with no entry.
    ///
    /// # Safety
    /// The caller holds the token.
    pub(crate) unsafe fn take_compacted(
        &self,
        mut give_back: impl FnMut(*mut BlockHeader),
    ) -> Option<(*mut BlockHeader, *mut BlockHeader)> {
        let mut first: *mut BlockHeader = std::ptr::null_mut();
        let mut last: *mut BlockHeader = std::ptr::null_mut();
        let mut block = self.first.load(Ordering::Relaxed);
        while !block.is_null() {
            let b = ring(block);
            let next = unsafe { (*b).link.next.load(Ordering::Relaxed) };
            let (front, tail) = unsafe {
                (
                    (*b).reader.front.load(Ordering::Relaxed),
                    (*b).writer.tail.load(Ordering::Relaxed),
                )
            };
            let mut write = 0;
            for read in front..tail {
                let entry = unsafe { *(*b).slots[read].get() };
                if entry != 0 {
                    unsafe { *(*b).slots[write].get() = entry };
                    write += 1;
                }
            }

            if write == 0 {
                give_back(block);
            } else {
                unsafe {
                    (*b).reader.front.store(0, Ordering::Relaxed);
                    *(*b).reader.local_tail.get() = write;
                    (*b).writer.tail.store(write, Ordering::Relaxed);
                    *(*b).writer.local_front.get() = 0;
                    (*b).link
                        .next
                        .store(std::ptr::null_mut(), Ordering::Relaxed);
                }
                if last.is_null() {
                    first = block;
                } else {
                    unsafe { (*ring(last)).link.next.store(block, Ordering::Relaxed) };
                }
                last = block;
            }
            block = next;
        }

        self.set_first(std::ptr::null_mut());
        self.last.store(std::ptr::null_mut(), Ordering::Relaxed);
        self.entries.store(0, Ordering::Relaxed);
        (!first.is_null()).then_some((first, last))
    }

    /// Call `visit` on every entry, oldest first, tombstones skipped.
    ///
    /// # Safety
    /// The caller holds the token.
    #[cfg(test)]
    pub(crate) unsafe fn walk(&self, mut visit: impl FnMut(usize)) {
        let mut block = self.first.load(Ordering::Relaxed);
        while !block.is_null() {
            let b = ring(block);
            let (front, tail) = unsafe {
                (
                    (*b).reader.front.load(Ordering::Relaxed),
                    (*b).writer.tail.load(Ordering::Relaxed),
                )
            };
            for index in front..tail {
                let entry = unsafe { *(*b).slots[index].get() };
                if entry != 0 {
                    visit(entry);
                }
            }
            block = unsafe { (*b).link.next.load(Ordering::Relaxed) };
        }
    }

    /// Blocks in the chain.
    ///
    /// # Safety
    /// The caller holds the token.
    #[cfg(test)]
    pub(crate) unsafe fn block_count(&self) -> usize {
        let mut count = 0;
        let mut block = self.first.load(Ordering::Relaxed);
        while !block.is_null() {
            count += 1;
            block = unsafe { (*ring(block)).link.next.load(Ordering::Relaxed) };
        }
        count
    }
}

/// Entries of `block` between its front and its tail that are not
/// tombstones.
///
/// # Safety
/// The caller holds the token of the chain `block` is in.
unsafe fn live_entries(block: *mut BlockHeader) -> usize {
    let b = ring(block);
    let (front, tail) = unsafe {
        (
            (*b).reader.front.load(Ordering::Relaxed),
            (*b).writer.tail.load(Ordering::Relaxed),
        )
    };
    (front..tail)
        .filter(|&index| unsafe { *(*b).slots[index].get() } != 0)
        .count()
}

#[cfg(test)]
mod tests;
