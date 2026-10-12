//! `web-heap-arrays` (`dev/design/the-web-loads.md`, "`web-heap-arrays`,
//! S68's array-bearing load"): `web-heap`'s request with part of its tree,
//! its ORM collections and a result set held by vectors, and its sessions by
//! string-keyed hashes.
//!
//! **The shape.** The tree, its sizes, its fan-out and its edges are drawn as
//! `web-heap`'s, from the same streams; what is new is drawn from
//! [`Purpose::Arrays`]. A tree object holds its children through a vector in
//! its first child slot with [`ARRAY_HOLDER_PERCENT`]; an ORM collection's
//! head holds a vector of its entities in slot 1, in place of the chain of
//! segments; one tree object a request holds the result set, a vector of the
//! next [`RESULT_ROWS`] tree objects in build order. A session's head holds a
//! hash of its other objects under string keys, and an integer under `t`.
//!
//! **The writes.** Every element write moves a creation reference in, by
//! `array::testing::push` and `testing::raw_insert`, or stores a scalar: no
//! write overwrites or removes a counted element, so no element write makes a
//! decrement and none registers a root. These are the crate's test-only
//! writes to the representation, not the element layer's `write_through`,
//! which a program's stores take; both kinds' builds run the same path, and under
//! `gc-window` the representation's mutable view tags the array as the holder
//! (`array::entity::as_vector_mut`, `as_table_mut`). Each write is asserted:
//! a refused one would lose its reference.
//!
//! **What stays below a block payload.** A vector's body is sixteen bytes an
//! element at a capacity doubling from eight, so one past [`VECTOR_MOST`]
//! elements takes a body past `BLOCK_PAYLOAD`, an OS-direct run whose array
//! the collector's frees refuse (`cycle::collector_frees::eligible`); a hash
//! of [`SESSION_ENTRIES_MOST`] entries takes 40,960 bytes. Only the large
//! result set of `web-heap-arrays-large` passes the first.

use super::*;
use crate::array::table::{InsertKind, InsertOutcome, Key};
use crate::memory::barrier::write_value_slot;
use crate::memory::block_pool::BLOCK_PAYLOAD;
use crate::string::{LLString, ll_string_new};
use crate::value::{Tag, Value};

/// The bytes of an array's entity, the fourth of the classes the loads
/// count.
pub(crate) const ARRAY_BYTES: usize = size_of::<LLArray>();

/// The share of tree objects that hold their children through a vector [A]:
/// set so that arrays are a tenth or more of the garbage made, the load's
/// validity test (`dev/design/the-web-loads.md`, "What the rig reports").
pub(super) const ARRAY_HOLDER_PERCENT: f64 = 50.0;

/// The rows of a result set: Laravel's default page, `Model::$perPage` [F].
pub(super) const RESULT_ROWS: usize = 15;

/// The rows of a large result set [A]: a capacity of 4,096, the first whose
/// body passes `BLOCK_PAYLOAD`.
pub(super) const LARGE_RESULT_ROWS: usize = 4_000;

/// The most elements a vector holds with its body inside a block payload.
pub(super) const VECTOR_MOST: usize = 2_048;

const _: () = assert!(VECTOR_MOST * size_of::<Value>() <= BLOCK_PAYLOAD);
const _: () = assert!(2 * VECTOR_MOST * size_of::<Value>() > BLOCK_PAYLOAD);
const _: () = assert!(LARGE_RESULT_ROWS > VECTOR_MOST);

/// The most entries a session's hash takes before its writes become scalar
/// stores: 2,048 slots of four bytes and 1,024 entries of 32, inside a block
/// payload.
pub(super) const SESSION_ENTRIES_MOST: usize = 1_024;

const _: () = assert!(2 * SESSION_ENTRIES_MOST * 4 + SESSION_ENTRIES_MOST * 32 <= BLOCK_PAYLOAD);

/// The share of session writes that append an object [A]; the rest store a
/// scalar.
pub(super) const SESSION_APPEND_PERCENT: f64 = 50.0;

/// Of the touches that end a session's life or append to it, the share that
/// ends it: one touch in a hundred replaces a session and 0.3 × 0.5 of them
/// append, so a session the setup builds takes a geometric count of prior
/// appends of mean 15.
const PRIOR_APPENDS_END: f64 = 0.01 / (0.01 + 0.3 * 0.5);

/// A slow request's first wait, uniform between these [A]: past the window's
/// young cut of 100 ms (`dev/DECISIONS.md`, "the window's offer reads R as
/// its roots older than 100 ms").
pub(super) const SLOW_WAIT: [Duration; 2] =
    [Duration::from_millis(150), Duration::from_millis(300)];

/// The shares of requests a `web-heap-arrays` load draws large result sets
/// and slow waits at, in percent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ArrayLoad {
    pub(crate) large_result_percent: u8,
    pub(crate) slow_percent: u8,
}

impl ArrayLoad {
    /// `web-heap-arrays`, the gated load: neither.
    pub(crate) const GATED: Self = Self {
        large_result_percent: 0,
        slow_percent: 0,
    };
    /// `web-heap-arrays-large` [A].
    pub(crate) const LARGE: Self = Self {
        large_result_percent: 5,
        slow_percent: 0,
    };
    /// `web-heap-arrays-slow` [A].
    pub(crate) const SLOW: Self = Self {
        large_result_percent: 0,
        slow_percent: 2,
    };
}

/// What an array of a plan holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArrayKind {
    /// A tree object's children.
    Children,
    /// An ORM collection's entities.
    Collection,
    /// The result set's rows.
    ResultSet,
}

/// An array of a plan: the object and the slot that hold it, and what it
/// holds. It is born with its holder; a child whose parent is that slot is
/// appended to it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ArrayPlacement {
    pub(crate) holder: u32,
    pub(crate) slot: u8,
    pub(crate) kind: ArrayKind,
}

/// The context and the payload tree of `count` objects with the collections
/// `orm`, in build order, the arrays that hold part of them, and whether the
/// result set is a large one, as `place_the_objects` places `web-heap`'s: the
/// same draws from `shape`, in the same order, and the arrays' own from
/// `arrays`. The segments a collection would take go to the tree.
pub(super) fn place_the_objects(
    shape: &mut Draws,
    arrays: &mut Draws,
    core: &Zipf,
    orm: &[usize],
    count: usize,
    load: ArrayLoad,
) -> (Vec<Placement>, Vec<ArrayPlacement>, bool) {
    let mut objects = Vec::with_capacity(count);
    let mut placed = Vec::new();
    for _ in 0..CONTEXT {
        objects.push(Placement {
            size_index: draw_a_size_index(shape),
            parent: None,
            extra: ExtraEdge::None,
            born_at: 0.0,
        });
    }

    let mut free_slots = std::collections::VecDeque::new();
    for (index, object) in objects.iter().enumerate() {
        // Slot 0 links the ring.
        for slot in 1..slots(object.size_index).min(FAN_OUT + 1) {
            free_slots.push_back((index as u32, slot as u8));
        }
    }

    let tree = count - CONTEXT - orm.iter().map(|&entities| 1 + entities).sum::<usize>();
    let mut heads_at: Vec<usize> = orm.iter().map(|_| shape.below(tree.max(1))).collect();
    heads_at.sort_unstable();
    let mut heads_at = heads_at.into_iter().zip(orm.iter().copied()).peekable();
    let result_set = draw_the_result_set(arrays, tree, load);
    let (mut rows_left, mut rows_holder) = (0, (0, 0));
    for position in 0..tree {
        while let Some(&(at, entities)) = heads_at.peek()
            && at == position
        {
            place_a_collection(shape, &mut objects, &mut free_slots, &mut placed, entities);
            heads_at.next();
        }

        let size_index = draw_a_size_index(shape);
        let index = objects.len() as u32;
        if rows_left > 0 {
            rows_left -= 1;
            // A large result set's rows are leaves, so that its body seeds a
            // set of little more than them.
            let extra = match result_set {
                Some(ResultSet { large: true, .. }) => ExtraEdge::None,
                _ => draw_an_extra_edge(shape, core),
            };
            objects.push(Placement {
                size_index,
                parent: Some(rows_holder),
                extra,
                born_at: 0.0,
            });
            if !matches!(result_set, Some(ResultSet { large: true, .. })) {
                open_the_slots(
                    arrays,
                    &mut free_slots,
                    &mut placed,
                    index,
                    size_index,
                    extra,
                );
            }

            continue;
        }

        let extra = draw_an_extra_edge(shape, core);
        let parent = free_slots
            .pop_front()
            .expect("every tree object adds a free slot at least");
        objects.push(Placement {
            size_index,
            parent: Some(parent),
            extra,
            born_at: 0.0,
        });
        let first = usize::from(extra != ExtraEdge::None);
        match result_set {
            Some(ResultSet { at, rows, .. }) if at == position => {
                placed.push(ArrayPlacement {
                    holder: index,
                    slot: first as u8,
                    kind: ArrayKind::ResultSet,
                });
                (rows_left, rows_holder) = (rows, (index, first as u8));
                for slot in first + 1..slots(size_index).min(first + FAN_OUT) {
                    free_slots.push_back((index, slot as u8));
                }
            }
            _ => open_the_slots(
                arrays,
                &mut free_slots,
                &mut placed,
                index,
                size_index,
                extra,
            ),
        }
    }

    for (_, entities) in heads_at {
        place_a_collection(shape, &mut objects, &mut free_slots, &mut placed, entities);
    }

    let large = matches!(result_set, Some(ResultSet { large: true, .. }));
    (objects, placed, large)
}

/// Where a request's result set stands: the tree position of the object
/// holding it, its rows, and whether it is a large one.
#[derive(Clone, Copy)]
struct ResultSet {
    at: usize,
    rows: usize,
    large: bool,
}

/// The result set of a tree of `tree` objects: large with `load`'s share
/// where the tree has room for its rows, its holder then drawn uniformly
/// among the positions that leave them room; none in a tree of fewer than
/// two objects.
fn draw_the_result_set(arrays: &mut Draws, tree: usize, load: ArrayLoad) -> Option<ResultSet> {
    let large = arrays.percent(f64::from(load.large_result_percent)) && tree > LARGE_RESULT_ROWS;
    if tree < 2 {
        return None;
    }

    let rows = if large {
        LARGE_RESULT_ROWS
    } else {
        RESULT_ROWS.min(tree - 1)
    };
    Some(ResultSet {
        at: arrays.below(tree - rows),
        rows,
        large,
    })
}

/// Open the child slots of tree object `index`, born of `size_index` with
/// `extra`: with [`ARRAY_HOLDER_PERCENT`] each of its children is appended to
/// a vector in its first child slot, else each takes a slot of its own.
fn open_the_slots(
    arrays: &mut Draws,
    free_slots: &mut std::collections::VecDeque<(u32, u8)>,
    placed: &mut Vec<ArrayPlacement>,
    index: u32,
    size_index: u8,
    extra: ExtraEdge,
) {
    let first = usize::from(extra != ExtraEdge::None);
    let child_slots = first..slots(size_index).min(first + FAN_OUT);
    if !arrays.percent(ARRAY_HOLDER_PERCENT) {
        for slot in child_slots {
            free_slots.push_back((index, slot as u8));
        }

        return;
    }

    placed.push(ArrayPlacement {
        holder: index,
        slot: first as u8,
        kind: ArrayKind::Children,
    });
    for _ in child_slots {
        free_slots.push_back((index, first as u8));
    }
}

/// A collection of `entities`: its head, of the largest size, in the tree's
/// next free slot, holding a vector in its slot 1 to which each entity, of a
/// drawn size with its edge back to the head in its slot 0, is appended. The
/// collection is exempt from the fan-out and adds no free slot to the tree.
fn place_a_collection(
    shape: &mut Draws,
    objects: &mut Vec<Placement>,
    free_slots: &mut std::collections::VecDeque<(u32, u8)>,
    placed: &mut Vec<ArrayPlacement>,
    entities: usize,
) {
    assert!(
        entities <= VECTOR_MOST,
        "a collection's body stays in a block"
    );
    let head = objects.len() as u32;
    objects.push(Placement {
        size_index: LARGEST,
        parent: Some(free_slots.pop_front().expect("a free slot for the head")),
        extra: ExtraEdge::None,
        born_at: 0.0,
    });
    placed.push(ArrayPlacement {
        holder: head,
        slot: 1,
        kind: ArrayKind::Collection,
    });
    for _ in 0..entities {
        objects.push(Placement {
            size_index: draw_a_size_index(shape),
            parent: Some((head, 1)),
            extra: ExtraEdge::BackTo(head),
            born_at: 0.0,
        });
    }
}

/// The rest of a `web-heap-arrays` plan, from `arrays`: the session's write,
/// [`SESSION_WRITE_PERCENT`] of requests, an appended object or with the
/// rest a scalar; and with `load`'s share a slow first wait.
pub(super) fn draw_the_rest(arrays: &mut Draws, plan: &mut Plan, load: ArrayLoad) {
    if arrays.percent(SESSION_WRITE_PERCENT) {
        if arrays.percent(SESSION_APPEND_PERCENT) {
            plan.session_write = Some(draw_a_size_index(arrays));
        } else {
            plan.session_scalar = true;
        }
    }

    plan.slow = arrays.percent(f64::from(load.slow_percent));
    if plan.slow {
        plan.waits[0] = SLOW_WAIT[0] + (SLOW_WAIT[1] - SLOW_WAIT[0]).mul_f64(arrays.unit());
    }
}

/// Put `child`, just built, into `holder`'s slot `slot`: appended to the
/// vector the slot holds, or with none its creation reference handed to the
/// empty slot by `move_prop`.
///
/// # Safety
/// `holder` and `child` are live GC-heap objects of this thread, and
/// `child`'s creation reference is the caller's and is spent here.
pub(super) unsafe fn link(holder: *mut Object, slot: u32, child: *mut Object) {
    let held = unsafe { *Object::prop_at(holder, prop_offset(slot)) };
    if held.tag() != Tag::Array {
        unsafe { move_prop(holder, prop_offset(slot), child) };
        return;
    }

    let vector = held.entity_ptr() as *mut LLArray;
    let appended = unsafe {
        crate::array::testing::push(vector, Value::entity(Tag::Object, child as *mut RcHeader))
    };
    assert!(appended, "the vector took its element");
}

/// Move a fresh array's creation reference into `holder`'s empty slot `slot`,
/// as `move_prop` moves an object's.
///
/// # Safety
/// `holder` is a live GC-heap object of this thread, `array` a fresh one.
unsafe fn hang(holder: *mut Object, slot: u32, array: *mut LLArray) {
    unsafe {
        let at = Object::prop_at(holder, prop_offset(slot));
        assert!(entity_checked(&*at).is_null(), "the array's slot is empty");
        write_value_slot(at, Value::entity(Tag::Array, array as *mut RcHeader));
    }
}

/// The bytes granted to `array`'s body, zero for none.
///
/// # Safety
/// `array` is a live array of this thread.
pub(super) unsafe fn body_bytes(array: *mut LLArray) -> usize {
    unsafe { crate::array::entity::body_of(array) }.map_or(0, |(_, capacity)| capacity)
}

/// The bytes of the body of the hash a session's `head` holds in its slot 0,
/// zero where that slot holds no array: a `web-heap` session or a value.
///
/// # Safety
/// `head` is a live object of this thread.
pub(super) unsafe fn session_body_bytes(head: *mut Object) -> usize {
    let held = unsafe { *Object::prop_at(head, prop_offset(0)) };
    if held.tag() != Tag::Array {
        return 0;
    }

    unsafe { body_bytes(held.entity_ptr() as *mut LLArray) }
}

/// The strings a mutator's session hashes are keyed by, each holding its
/// creation reference: `t` first, then `s1` to `s1023`, so that an entry's
/// key is the string at its position.
///
/// # Safety
/// `context` is the calling mutator's, at a point where it may allocate.
pub(super) unsafe fn make_the_keys(context: &mut LLContext) -> Vec<*mut LLString> {
    (0..SESSION_ENTRIES_MOST)
        .map(|index| {
            let name = if index == 0 {
                "t".to_owned()
            } else {
                format!("s{index}")
            };
            let key = unsafe { ll_string_new(context, MemoryCategory::GcHeap, name.as_bytes()) };
            assert!(!key.is_null(), "the heap served the key");
            key
        })
        .collect()
}

/// Insert `value` under `key`, a key `hash` does not hold, the key's
/// reference retained for the table.
///
/// # Safety
/// `hash` is a live hash of this thread, `key` one of [`make_the_keys`], and
/// `value`'s reference, if it has one, is the caller's and is spent here.
unsafe fn insert(hash: *mut LLArray, key: *mut LLString, value: Value) {
    unsafe { ll_retain(key as *mut RcHeader) };
    let outcome = unsafe {
        crate::array::testing::raw_insert(hash, InsertKind::Admission, Key::Str(key), value)
    };
    assert!(
        matches!(outcome, InsertOutcome::Added),
        "the hash took a new key"
    );
}

/// Store the integer `stamp` under `key`, which `hash` holds with an integer.
///
/// # Safety
/// As [`insert`], and `hash` holds `key`.
unsafe fn store_a_scalar(hash: *mut LLArray, key: *mut LLString, stamp: i64) {
    let outcome = unsafe {
        crate::array::testing::raw_insert(
            hash,
            InsertKind::Admission,
            Key::Str(key),
            Value::int(stamp),
        )
    };
    assert!(
        matches!(outcome, InsertOutcome::Replaced(old) if old.is_int()),
        "the key held an integer"
    );
}

/// Build a session of [`SESSION_OBJECTS`] of drawn sizes: its head holding
/// in slot 0 a hash of the integer 0 under `keys[0]` and of the other objects
/// under the keys after it, each holding the head in its slot 0, and in slot
/// 1 `core`. Answers the head, holding its creation reference, and the bytes
/// by class.
///
/// # Safety
/// `arena` is the calling mutator's, at a point where it may allocate;
/// `keys` are [`make_the_keys`]' of this thread and `core` a live object of
/// its heap.
pub(super) unsafe fn build_a_session(
    arena: *mut Arena,
    classes: &WebClasses,
    draws: &mut Draws,
    keys: &[*mut LLString],
    core: *mut Object,
) -> (*mut Object, [u32; CLASSES]) {
    let mut context = LLContext { arena };
    let mut bytes = [0; CLASSES];
    let mut new_object = |draws: &mut Draws| {
        let size_index = draw_a_size_index(draws) as usize;
        bytes[size_index] += SIZES[size_index] as u32;
        unsafe { new_constructed(&mut context, classes.0[size_index], MemoryCategory::GcHeap) }
    };
    let head = new_object(draws);
    let members: Vec<*mut Object> = (1..SESSION_OBJECTS).map(|_| new_object(draws)).collect();
    let hash = unsafe { crate::array::testing::hash_array(MemoryCategory::GcHeap) };
    assert!(!hash.is_null(), "the heap served the session's hash");
    bytes[ARRAY_CLASS] += ARRAY_BYTES as u32;
    unsafe {
        hang(head, 0, hash);
        insert(hash, keys[0], Value::int(0));
        for (&member, &key) in members.iter().zip(&keys[1..]) {
            insert(
                hash,
                key,
                Value::entity(Tag::Object, member as *mut RcHeader),
            );
            store_prop(arena, member, prop_offset(0), head);
        }

        store_prop(arena, head, prop_offset(1), core);
    }

    (head, bytes)
}

/// The hash a session's `head` holds.
fn hash_of(head: *mut Object) -> *mut LLArray {
    let held = unsafe { *Object::prop_at(head, prop_offset(0)) };
    assert_eq!(held.tag(), Tag::Array, "a session's head holds its hash");
    held.entity_ptr() as *mut LLArray
}

impl LongLived {
    /// Write the session in directory slot `slot`: with `append`, a new
    /// object of that size index under the hash's next key, while the hash
    /// holds fewer than [`SESSION_ENTRIES_MOST`] entries; else, or with
    /// `scalar`, the request count stored under `t`. Answers the bytes born,
    /// which the session's bytes and the state's take before any replacement
    /// can end them.
    ///
    /// # Safety
    /// `arena` is the calling mutator's, at a point where it may allocate;
    /// the state is of `web-heap-arrays`.
    pub(super) unsafe fn write_the_session(
        &mut self,
        arena: *mut Arena,
        slot: u32,
        append: Option<u8>,
        scalar: bool,
    ) -> [usize; CLASSES] {
        let mut born = [0; CLASSES];
        let hash = hash_of(self.sessions.head(slot));
        let entries = unsafe { crate::array::testing::table(hash) }.len();
        match append {
            Some(size_index) if entries < SESSION_ENTRIES_MOST => {
                let size_index = size_index as usize;
                let mut context = LLContext { arena };
                let object = unsafe {
                    new_constructed(
                        &mut context,
                        self.classes.0[size_index],
                        MemoryCategory::GcHeap,
                    )
                };
                unsafe {
                    insert(
                        hash,
                        self.keys[entries],
                        Value::entity(Tag::Object, object as *mut RcHeader),
                    )
                };
                born[size_index] = SIZES[size_index];
                self.sessions.bytes[slot as usize][size_index] += SIZES[size_index] as u32;
                self.held[size_index] += SIZES[size_index];
                self.counts.session_writes += 1;
            }
            Some(_) => unsafe { store_a_scalar(hash, self.keys[0], self.requests as i64) },
            None if scalar => unsafe { store_a_scalar(hash, self.keys[0], self.requests as i64) },
            None => {}
        }

        born
    }

    /// The bytes of the bodies of the sessions' hashes, the state's live
    /// array bodies; zero outside `web-heap-arrays`.
    pub(crate) fn live_body_bytes(&self) -> usize {
        (0..self.sessions.bytes.len() as u32)
            .map(|slot| unsafe { session_body_bytes(self.sessions.head(slot)) })
            .sum()
    }

    /// Give each session the appends it took before the cell: a geometric
    /// count of mean 15 ([`PRIOR_APPENDS_END`]), each of a size drawn from
    /// `setup`.
    ///
    /// # Safety
    /// As [`LongLived::new`].
    pub(super) unsafe fn write_the_prior_appends(&mut self, arena: *mut Arena, setup: &mut Draws) {
        for slot in 0..self.sessions.bytes.len() as u32 {
            for _ in SESSION_OBJECTS..SESSION_ENTRIES_MOST {
                if setup.unit() < PRIOR_APPENDS_END {
                    break;
                }

                let size_index = draw_a_size_index(setup);
                let _ = unsafe { self.write_the_session(arena, slot, Some(size_index), false) };
            }
        }
    }
}

impl Request {
    /// Hang the arrays [`Plan::arrays`] places on the object born last, each
    /// a fresh vector in its slot. Answers how many.
    ///
    /// # Safety
    /// As [`Request::advance`], straight after a birth.
    pub(super) unsafe fn hang_the_arrays(&mut self) -> usize {
        let holder = self.objects.len() as u32 - 1;
        let mut hung = 0;
        while let Some(&placed) = self.plan.arrays.get(self.arrays.len())
            && placed.holder == holder
        {
            let vector = unsafe { crate::array::entity::ll_array_new(MemoryCategory::GcHeap) };
            assert!(!vector.is_null(), "the heap served the array");
            unsafe {
                hang(
                    self.objects[holder as usize],
                    u32::from(placed.slot),
                    vector,
                )
            };
            self.arrays.push(vector);
            hung += 1;
        }

        hung
    }

    /// End a `web-heap-arrays` request: the session written as its plan
    /// says, at the end as PHP saves a session, its array bodies counted, and
    /// [`Request::end`].
    ///
    /// # Safety
    /// As [`Request::finish`].
    pub(super) unsafe fn end_with_arrays(self, build: &mut RequestBuild) -> Ended {
        let born = unsafe {
            build.long_lived.write_the_session(
                build.arena,
                self.plan.session,
                self.plan.session_write,
                self.plan.session_scalar,
            )
        };
        let bodies = self
            .arrays
            .iter()
            .map(|&array| unsafe { body_bytes(array) })
            .sum();
        let mut ended = unsafe { self.end() };
        ended.born = born;
        ended.bodies = bodies;
        ended
    }

    /// The arrays born so far.
    pub(super) fn arrays(&self) -> &[*mut LLArray] {
        &self.arrays
    }
}

/// The objects of `plan` whose parent is the slot `placed` stands in: the
/// array's elements, in build order.
fn elements_of(plan: &Plan, placed: ArrayPlacement) -> Vec<usize> {
    (0..plan.objects.len())
        .filter(|&index| plan.objects[index].parent == Some((placed.holder, placed.slot)))
        .collect()
}

/// A small state of `web-heap-arrays` in `load`, its classes named after
/// `name`.
fn fixture(name: &str, load: ArrayLoad) -> Fixture {
    Fixture::in_variant(
        name,
        LongLivedShape {
            core: 50,
            values: 10,
            sessions: 2,
        },
        Variant::HeapArrays(load),
    )
}

/// The plan keeps `web-heap`'s count and places its arrays as specified:
/// every collection one vector of its entities in its head's slot 1, one
/// result set of [`RESULT_ROWS`] rows, half the tree objects that open
/// slots holding their children through a vector, none longer than
/// [`VECTOR_MOST`]; and a large result set's rows are leaves.
#[test]
fn an_arrays_plan_places_its_arrays() {
    let targets = Targets::new(
        &LongLivedShape::specified(40_000),
        Variant::HeapArrays(ArrayLoad::GATED),
    );
    let mut streams = Streams::new(1, 1);
    let (mut holders, mut openers) = (0, 0);
    for _ in 0..50 {
        let plan = Plan::with_count(&mut streams, &targets, 2_000);
        assert_eq!(plan.objects.len(), 2_000);
        assert!(!plan.large_result_set);
        let kinds = |kind| plan.arrays.iter().filter(move |placed| placed.kind == kind);
        assert_eq!(kinds(ArrayKind::Collection).count(), plan.orm.len());
        for (placed, &entities) in kinds(ArrayKind::Collection).zip(&plan.orm) {
            assert_eq!(placed.slot, 1);
            assert_eq!(elements_of(&plan, *placed).len(), entities);
        }

        let result_sets: Vec<&ArrayPlacement> = kinds(ArrayKind::ResultSet).collect();
        assert_eq!(result_sets.len(), 1);
        assert_eq!(elements_of(&plan, *result_sets[0]).len(), RESULT_ROWS);
        for placed in &plan.arrays {
            assert!(elements_of(&plan, *placed).len() <= VECTOR_MOST);
        }

        holders += kinds(ArrayKind::Children).count();
        // Every tree object but the result set's holder draws whether it
        // holds an array, the rows of an ordinary result set included; a
        // collection's head and entities do not.
        let heads: Vec<u32> = kinds(ArrayKind::Collection)
            .map(|placed| placed.holder)
            .collect();
        openers += plan
            .objects
            .iter()
            .enumerate()
            .skip(CONTEXT)
            .filter(|&(index, object)| {
                let index = index as u32;
                !matches!(object.extra, ExtraEdge::BackTo(_))
                    && !heads.contains(&index)
                    && result_sets[0].holder != index
            })
            .count();
        assert!(plan.arrays.is_sorted_by_key(|placed| placed.holder));
    }

    let share = holders as f64 * 100.0 / openers as f64;
    assert!(
        (share - ARRAY_HOLDER_PERCENT).abs() < 2.0,
        "holders {share} %"
    );

    let targets = Targets::new(
        &LongLivedShape::specified(40_000),
        Variant::HeapArrays(ArrayLoad {
            large_result_percent: 100,
            slow_percent: 100,
        }),
    );
    let plan = Plan::with_count(&mut streams, &targets, 10_000);
    assert!(plan.large_result_set && plan.slow);
    assert!((SLOW_WAIT[0]..=SLOW_WAIT[1]).contains(&plan.waits[0]));
    let result_set = *plan
        .arrays
        .iter()
        .find(|placed| placed.kind == ArrayKind::ResultSet)
        .expect("a result set");
    let rows = elements_of(&plan, result_set);
    assert_eq!(rows.len(), LARGE_RESULT_ROWS);
    for &row in &rows {
        assert_eq!(plan.objects[row].extra, ExtraEdge::None);
        assert!(
            plan.objects.iter().all(|object| object
                .parent
                .is_none_or(|(parent, _)| parent as usize != row)),
            "row {row} is a leaf"
        );
    }
}

/// A request built along an arrays plan holds what the plan names: each
/// array in its holder's slot with its elements in build order, the plan's
/// bytes by class born and held, an array's 112 bytes among them; and its
/// end makes all of it garbage, which a collection frees whole, the garbage
/// read back to zero.
#[test]
fn a_request_with_arrays_builds_its_plan_and_its_garbage_is_exact() {
    let _g = test_guard();
    let mut fixture = fixture("ArraysExact", ArrayLoad::GATED);
    let mut plan = fixture.plan(7, 2_000);
    plan.session_write = None;
    plan.session_scalar = false;
    let bytes = plan.bytes();
    assert_eq!(bytes[ARRAY_CLASS], plan.arrays.len() * ARRAY_BYTES);
    let mut garbage = Garbage::new(Instant::now(), held_by_size());
    let mut build = fixture.build();
    let (request, advanced) = unsafe { build_whole(&mut build, plan) };
    assert_eq!(advanced.born, bytes);
    garbage.born(advanced.born);
    garbage.read(Instant::now(), held_by_size());
    assert_eq!(garbage.current(), [0; CLASSES], "the heap holds the plan");

    let plan = request.plan().clone();
    for (&array, &placed) in request.arrays().iter().zip(&plan.arrays) {
        let held = unsafe {
            *Object::prop_at(
                request.object(placed.holder as usize),
                prop_offset(placed.slot.into()),
            )
        };
        assert_eq!(held.entity_ptr() as *mut LLArray, array);
        let elements = elements_of(&plan, placed);
        assert_eq!(
            unsafe { crate::array::testing::used(array) },
            elements.len()
        );
        for (position, &element) in elements.iter().enumerate() {
            let at = unsafe { crate::array::testing::at(array, position) }.expect("an element");
            assert_eq!(at.entity_ptr() as *mut Object, request.object(element));
        }
    }

    let ended = unsafe { request.finish(&mut build) };
    assert_eq!(ended.bytes, bytes);
    assert_eq!(ended.born, [0; CLASSES]);
    assert!(ended.bodies > 0, "the vectors' bodies");
    garbage.ended(ended.bytes);
    unsafe { crate::gc::ll_gc_collect_cycles() };
    garbage.read(Instant::now(), held_by_size());
    assert_eq!(
        garbage.current(),
        [0; CLASSES],
        "the collection freed the request"
    );
    fixture.let_go();
}

/// A session write appends an object under the hash's next key, born into
/// the session's bytes and the state's at the end, or stores the request
/// count under `t`, which holds no counted element and bears nothing.
#[test]
fn a_session_write_appends_or_stores_a_scalar() {
    let _g = test_guard();
    let mut fixture = fixture("ArraysSession", ArrayLoad::GATED);
    for append in [true, false] {
        let mut plan = fixture.plan(3, 100);
        (plan.session_write, plan.session_scalar) = if append {
            (Some(1), false)
        } else {
            (None, true)
        };
        let slot = plan.session;
        let hash = hash_of(fixture.long_lived.sessions.head(slot));
        let entries = unsafe { crate::array::testing::table(hash) }.len();
        let (session_bytes, held) = (
            fixture.long_lived.sessions.bytes[slot as usize],
            fixture.long_lived.held(),
        );
        let mut build = fixture.build();
        let (request, _) = unsafe { build_whole(&mut build, plan) };
        let ended = unsafe { request.finish(&mut build) };
        let grown = usize::from(append);
        assert_eq!(
            unsafe { crate::array::testing::table(hash) }.len(),
            entries + grown
        );
        assert_eq!(ended.born, [0, grown * SIZES[1], 0, 0]);
        assert_eq!(
            fixture.long_lived.sessions.bytes[slot as usize][1],
            session_bytes[1] + (grown * SIZES[1]) as u32
        );
        assert_eq!(fixture.long_lived.held()[1], held[1] + grown * SIZES[1]);
        let stamp =
            unsafe { crate::array::testing::get(hash, Key::Str(fixture.long_lived.keys[0])) }
                .expect("a session holds `t`");
        assert_eq!(
            stamp.as_int(),
            if append {
                0
            } else {
                fixture.long_lived.requests as i64
            }
        );
        unsafe { crate::gc::ll_gc_collect_cycles() };
    }

    fixture.let_go();
}

/// The setup gives each session a geometric count of prior appends of mean
/// 15: over 400 sessions, the hashes' entries less the session's own 20
/// average within three of their standard errors.
#[test]
fn the_sessions_take_their_prior_appends() {
    let _g = test_guard();
    let fixture = Fixture::in_variant(
        "ArraysPrior",
        LongLivedShape {
            core: 50,
            values: 10,
            sessions: 400,
        },
        Variant::HeapArrays(ArrayLoad::GATED),
    );
    let sessions = &fixture.long_lived.sessions;
    let appends: usize = (0..400u32)
        .map(|slot| {
            unsafe { crate::array::testing::table(hash_of(sessions.head(slot))) }.len()
                - SESSION_OBJECTS
        })
        .sum();
    let mean = appends as f64 / 400.0;
    assert!((mean - 15.0).abs() < 2.5, "{mean} prior appends");
    assert_eq!(fixture.long_lived.counts.session_writes, appends);
    fixture.let_go();
}

/// A large result set's body passes the block payload and is counted
/// OS-direct, granted at the build and given back once a collection frees
/// it, its bytes among the request's bodies.
#[test]
fn a_large_result_sets_body_is_counted_os_direct() {
    let _g = test_guard();
    let mut fixture = fixture(
        "ArraysLarge",
        ArrayLoad {
            large_result_percent: 100,
            slow_percent: 0,
        },
    );
    let plan = fixture.plan(5, 10_000);
    assert!(plan.large_result_set);
    let [(_, os_granted), (_, os_given_back)] = crate::memory::routing::body_bytes::read();
    let mut build = fixture.build();
    let (request, _) = unsafe { build_whole(&mut build, plan) };
    let large = request
        .plan()
        .arrays
        .iter()
        .position(|placed| placed.kind == ArrayKind::ResultSet)
        .map(|index| request.arrays()[index])
        .expect("a result set");
    let body = unsafe { body_bytes(large) };
    assert!(body > BLOCK_PAYLOAD, "a body of {body} bytes");
    let [(_, granted_after), _] = crate::memory::routing::body_bytes::read();
    assert_eq!(granted_after - os_granted, body);
    let ended = unsafe { request.finish(&mut build) };
    assert!(ended.bodies >= body);
    unsafe { crate::gc::ll_gc_collect_cycles() };
    let [_, (_, given_back_after)] = crate::memory::routing::body_bytes::read();
    assert_eq!(
        given_back_after - os_given_back,
        body,
        "the owner freed the body"
    );
    fixture.let_go();
}
