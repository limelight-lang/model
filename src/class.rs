//! Class descriptors and link-time construction.
//!
//! Layout per `rfc/model/classes.md` / `rfc/model/lowering.md`: one
//! descriptor per class with an **inline trailing vtable**, allocated
//! in immortal metadata memory at link time; its address is stable for
//! the process lifetime (the foundation for inline caches). Linking a
//! class at runtime is legitimate runtime machinery — autoloading and
//! the JIT link classes while the program runs — but every *decision*
//! (what to link, which dispatch path a call site uses) stays with the
//! compiler.
//!
//! Implemented here: slot-stable vtable inheritance, method table for
//! the slow path (interned name → slot), per-interface itables with
//! slot maps (rebuilt against the subclass's own vtable, so overrides
//! flow into inherited interfaces), Cohen display for O(1)
//! `instanceof`, and property layout with machine-typed slots (scalar /
//! bare pointer / Box, grouped into three runs) plus the two typed trace
//! lists (`ptr_runs` / `box_runs`) the GC and teardown stride.
//!
//! **Dispatch tables are pure code-pointer arrays** — the invariant:
//! no headers inside any table (C++-style offset-to-top/RTTI prefixes
//! are unnecessary because objects point at the descriptor, not at a
//! table; the descriptor *is* the vtbl's header). That makes the tail
//! a homogeneous train: `[Class][vtbl][itable A][itable B]…` in one
//! allocation, every table found by link-time pointer/offset.
//!
//! Deliberately absent (generated-code territory or later layers):
//! inline caches, property hooks, Ghost/Proxy shims, `__call`,
//! dynamic properties.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::intern::LLString;
use crate::memory::immortal::immortal_alloc;

pub const CLASS_FINAL: u32 = 1 << 0;
pub const CLASS_ABSTRACT: u32 = 1 << 1;
pub const CLASS_INTERFACE: u32 = 1 << 2;
/// The class declares (or inherits) a `__destruct` with side effects.
pub const CLASS_HAS_DESTRUCTOR: u32 = 1 << 3;

/// No `__destruct`.
pub const NO_DESTRUCT_SLOT: u32 = u32::MAX;

/// The machine representation of a declared property's type — what the
/// slot physically *is*, chosen at link time (`rfc/model/classes.md`,
/// "Slot kinds"). Traced-ness and stride both follow from the kind, so
/// there is no separate per-slot "is a reference" flag any more.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotKind {
    /// Raw `i64` / `f64` (an `int` / `float` property): 8 bytes, never
    /// traced.
    Scalar = 0,
    /// A bare 8-byte pointer to a counted entity — a declared class type,
    /// `string` or `array`. `NULL` means uninitialized. Traced as a
    /// pointer run (stride 8, skip `NULL`).
    Pointer = 1,
    /// A 16-byte [`crate::value::Value`] Box — the one boxed slot form,
    /// for an untyped / `mixed` property (nullable scalars join it later).
    /// Traced as a Box run (stride 16, skip when the refcounted flag is
    /// clear).
    Boxed = 2,
    /// A `bool` byte (1 byte), never traced. Bit-packing into a byte block
    /// is a deferred optimization (`rfc` backlog, A7).
    Bool = 3,
}

impl SlotKind {
    /// Bytes the slot occupies.
    #[inline]
    pub const fn size(self) -> u32 {
        match self {
            SlotKind::Scalar | SlotKind::Pointer => 8,
            SlotKind::Boxed => 16,
            SlotKind::Bool => 1,
        }
    }

    /// Alignment the slot requires.
    #[inline]
    pub const fn align(self) -> u32 {
        match self {
            SlotKind::Bool => 1,
            _ => 8,
        }
    }

    /// Whether the GC traces this slot: pointer and Box slots hold counted
    /// references, scalars and bools never do.
    #[inline]
    pub const fn is_traced(self) -> bool {
        matches!(self, SlotKind::Pointer | SlotKind::Boxed)
    }
}

/// A contiguous stretch of same-kind traced slots in the object body:
/// `count` slots starting at byte `offset`. Pointer runs stride 8, Box
/// runs stride 16 — the GC trace map (`rfc/model/classes.md`, "Slot
/// order"). A `Run` compresses `count` slots into one 8-byte pair.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    pub offset: u32,
    pub count: u32,
}

/// A declared property: its fixed offset from the object base, its
/// machine slot kind, and its declaration-order index — all computed at
/// link time. Physical order groups the runs, so it no longer matches
/// declaration order, which `serialize()`/`foreach`/reflection still
/// observe; `declaration_index` preserves it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PropSlot {
    pub name: *const LLString,
    pub offset: u32,
    pub kind: SlotKind,
    pub declaration_index: u32,
}

/// Slow-path method table entry: interned name → vtable slot. Hot
/// paths never come here (direct calls, vtable, itable, ICs).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MethodEntry {
    pub name: *const LLString,
    pub slot: u32,
    _pad: u32,
}

/// One implemented interface: the COM-model itable (code pointers,
/// slots fixed by interface declaration order) plus the slot map it
/// was built from, kept so a subclass can rebuild the itable against
/// its own vtable — overrides then flow into inherited interfaces.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct InterfaceEntry {
    pub interface_id: u32,
    pub method_count: u32,
    pub itable: *const *const (),
    /// interface slot → class vtable slot.
    pub slot_map: *const u32,
}

#[repr(C)]
pub struct Class {
    pub flags: u32,
    /// Instance allocation size: `layout_end` rounded up to 8.
    pub object_size: u32,
    /// First free byte, unrounded: where a subclass resumes laying out
    /// its own properties, so a subclass field may sit in this class's
    /// tail padding (`rfc/model/classes.md`, JDK-15 rule).
    pub layout_end: u32,
    /// Identity for interface dispatch; meaningful when
    /// `CLASS_INTERFACE` is set.
    pub interface_id: u32,
    /// Cohen display length; own depth = `display_len - 1`.
    pub display_len: u32,
    pub prop_count: u32,
    pub method_count: u32,
    pub interface_count: u32,
    /// Vtable slot of `__destruct`, or [`NO_DESTRUCT_SLOT`].
    pub destruct_slot: u32,
    pub vtbl_len: u32,
    pub ptr_run_count: u32,
    pub box_run_count: u32,
    pub undef_run_count: u32,
    pub parent: *const Class,
    pub name: *const LLString,
    /// Ancestors root→self, indexed by depth: `instanceof` is one load
    /// + compare (Cohen display, `rfc/model/lowering.md`).
    pub display: *const *const Class,
    pub props: *const PropSlot,
    pub methods: *const MethodEntry,
    pub interfaces: *const InterfaceEntry,
    /// The internal destructor `dispose(obj)` (`crate::object::DisposeFn`):
    /// runs `__destruct` and releases counted fields, one indirect call
    /// into per-class code. Holds [`crate::object::ll_default_dispose`]
    /// until the compiler generates a specialized one.
    pub dispose: *const (),
    /// Counted-pointer trace runs (stride 8, skip `NULL`).
    pub ptr_runs: *const Run,
    /// Box trace runs (stride 16, skip when the refcounted flag is clear).
    pub box_runs: *const Run,
    /// Box slots declared **without** a default (stride 16): the factory
    /// stamps [`crate::value::VALUE_UNDEF`] over these after the
    /// zero-fill, since an all-zero Box is `null`, not undefined
    /// (`rfc/model/values.md`, Construction). Always a sub-range of a
    /// box run; construction is the only consumer — the GC and teardown
    /// never read it.
    pub undef_runs: *const Run,
    // vtbl: [*const (); vtbl_len] — inline trailing array.
}

static NEXT_IFACE_ID: AtomicU32 = AtomicU32::new(1);

impl Class {
    /// The inline trailing vtable.
    ///
    /// Takes a raw pointer rather than `&self` on purpose: the vtable is
    /// allocated *past* `size_of::<Class>()`, and a `&Class` only carries
    /// provenance over the fixed fields, so reaching the trailing array
    /// through it is outside that reference — UB under Stacked/Tree
    /// Borrows, which Miri reports (audit `class.rs:115`). The result
    /// borrows freely because class descriptors are immortal.
    ///
    /// # Safety
    /// `cls` must point to a linked class descriptor, allocated with its
    /// trailing vtable.
    #[inline]
    pub unsafe fn vtbl<'a>(cls: *const Class) -> &'a [*const ()] {
        unsafe {
            let base = (cls as *const u8).add(size_of::<Class>()) as *const *const ();
            std::slice::from_raw_parts(base, (*cls).vtbl_len as usize)
        }
    }

    #[inline]
    pub fn props(&self) -> &[PropSlot] {
        unsafe { std::slice::from_raw_parts(self.props, self.prop_count as usize) }
    }

    /// Counted-pointer trace runs (stride 8, skip `NULL`) — the GC and
    /// teardown consumer contract for bare-pointer slots.
    #[inline]
    pub fn ptr_runs(&self) -> &[Run] {
        unsafe { std::slice::from_raw_parts(self.ptr_runs, self.ptr_run_count as usize) }
    }

    /// Box trace runs (stride 16, skip when the refcounted flag is clear)
    /// — the same contract for `mixed`/untyped Box slots.
    #[inline]
    pub fn box_runs(&self) -> &[Run] {
        unsafe { std::slice::from_raw_parts(self.box_runs, self.box_run_count as usize) }
    }

    /// Box slots declared without a default (stride 16) — the factory's
    /// undef-stamp list, a sub-range of [`Self::box_runs`].
    #[inline]
    pub fn undef_runs(&self) -> &[Run] {
        unsafe { std::slice::from_raw_parts(self.undef_runs, self.undef_run_count as usize) }
    }

    /// Slow-path lookup: interned name → vtable slot. Name equality is
    /// pointer equality (`rfc/model/classes.md` Interned Names).
    pub fn find_method(&self, name: *const LLString) -> Option<u32> {
        let methods =
            unsafe { std::slice::from_raw_parts(self.methods, self.method_count as usize) };
        methods.iter().find(|m| m.name == name).map(|m| m.slot)
    }

    pub fn find_prop(&self, name: *const LLString) -> Option<&PropSlot> {
        self.props().iter().find(|p| p.name == name)
    }

    /// `instanceof` a class: Cohen display, one load + compare.
    #[inline]
    pub fn instance_of_class(&self, target: &Class) -> bool {
        let depth = target.display_len;
        if depth == 0 || depth > self.display_len {
            return false;
        }
        let entry = unsafe { *self.display.add(depth as usize - 1) };
        std::ptr::eq(entry, target)
    }

    /// Interface lookup: linear scan of a short array sorted by id
    /// (classes implement few interfaces; cache locality beats a
    /// hashtable). The analog of COM's QueryInterface.
    pub fn find_interface(&self, interface_id: u32) -> Option<&InterfaceEntry> {
        let entries =
            unsafe { std::slice::from_raw_parts(self.interfaces, self.interface_count as usize) };
        entries.iter().find(|e| e.interface_id == interface_id)
    }

    #[inline]
    pub fn has_destructor(&self) -> bool {
        self.flags & CLASS_HAS_DESTRUCTOR != 0
    }
}

/// Itable lookup for interface-typed call sites; the sorted-array
/// `find` of `rfc/model/classes.md`, IC-cached by generated code.
///
/// # Safety
/// `cls` must point to a linked class descriptor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ll_find_itable(cls: *const Class, interface_id: u32) -> *const *const () {
    match unsafe { (*cls).find_interface(interface_id) } {
        Some(e) => e.itable,
        None => std::ptr::null(),
    }
}

// --- Link-time construction ------------------------------------------------

/// Builds and links one class descriptor. This mirrors what the
/// compiler's class-linking phase performs when a class is loaded.
pub struct ClassBuilder {
    name: *const LLString,
    parent: *const Class,
    flags: u32,
    /// `(name, kind, starts_undef)` — `starts_undef` is the declaration
    /// criterion of `rfc/model/values.md`: a Boxed property with no
    /// default starts uninitialized. Meaningful only for `Boxed` until
    /// the init bitmap (A5 commit 2) tracks the raw kinds.
    props: Vec<(*const LLString, SlotKind, bool)>,
    methods: Vec<(*const LLString, *const ())>,
    interfaces: Vec<(u32, Vec<u32>)>,
    dispose: *const (),
}

impl ClassBuilder {
    pub fn new(name: &str) -> Self {
        ClassBuilder {
            name: crate::intern::intern_str(name),
            parent: std::ptr::null(),
            flags: 0,
            props: Vec::new(),
            methods: Vec::new(),
            interfaces: Vec::new(),
            // The generic teardown stand-in until a compiler generates a
            // specialized `dispose` for this class.
            dispose: crate::object::ll_default_dispose as *const (),
        }
    }

    /// Install a specialized `dispose` (`crate::object::DisposeFn`),
    /// modeling the per-class teardown the compiler would generate. The
    /// default is [`crate::object::ll_default_dispose`].
    pub fn dispose(mut self, code: *const ()) -> Self {
        self.dispose = code;
        self
    }

    /// Declare an interface (no instances; carries an id and fixes
    /// method slot indices by declaration order).
    pub fn interface(name: &str) -> *const Class {
        let mut b = Self::new(name);
        b.flags |= CLASS_INTERFACE;
        b.build()
    }

    pub fn parent(mut self, parent: *const Class) -> Self {
        self.parent = parent;
        self
    }

    pub fn final_class(mut self) -> Self {
        self.flags |= CLASS_FINAL;
        self
    }

    /// Declare a property, in the boolean compatibility shim over the two
    /// common kinds: `true` → a [`SlotKind::Boxed`] slot **with a
    /// default** (starts `null` from the zero-fill, never undef-tracked),
    /// `false` → a [`SlotKind::Scalar`] slot. Bare-pointer, `bool` and
    /// defaultless-Box slots have their own declarations.
    pub fn prop(self, name: &str, refcounted: bool) -> Self {
        let kind = if refcounted {
            SlotKind::Boxed
        } else {
            SlotKind::Scalar
        };
        self.prop_of(name, kind)
    }

    /// Declare a bare-pointer property (a declared class type / `string` /
    /// `array`): 8 bytes, traced as a pointer run, `NULL` = uninitialized.
    pub fn prop_pointer(self, name: &str) -> Self {
        self.prop_of(name, SlotKind::Pointer)
    }

    /// Declare a `bool` property: 1 byte, never traced.
    pub fn prop_bool(self, name: &str) -> Self {
        self.prop_of(name, SlotKind::Bool)
    }

    /// Declare a `mixed`/untyped property **without a default**: a Boxed
    /// slot that starts uninitialized — the factory stamps its
    /// [`crate::value::VALUE_UNDEF`] flag via the class's `undef_runs`.
    /// [`Self::prop`]'s Boxed form models the defaulted declaration,
    /// which starts `null` (the zero-fill) and is never tracked.
    pub fn prop_boxed_without_default(self, name: &str) -> Self {
        self.prop_with(name, SlotKind::Boxed, true)
    }

    fn prop_of(self, name: &str, kind: SlotKind) -> Self {
        self.prop_with(name, kind, false)
    }

    fn prop_with(mut self, name: &str, kind: SlotKind, starts_undef: bool) -> Self {
        self.props
            .push((crate::intern::intern_str(name), kind, starts_undef));
        self
    }

    /// Declare or override a method. Slot assignment per
    /// `rfc/model/classes.md`: same name as an inherited method →
    /// override into the same slot; new name → appended slot.
    pub fn method(mut self, name: &str, code: *const ()) -> Self {
        self.methods.push((crate::intern::intern_str(name), code));
        self
    }

    /// Declare `__destruct` (a method with a flagged slot).
    pub fn destructor(mut self, code: *const ()) -> Self {
        self.flags |= CLASS_HAS_DESTRUCTOR;
        self.methods
            .push((crate::intern::intern_str("__destruct"), code));
        self
    }

    /// Implement an interface: `slot_map[i]` is this class's vtable
    /// slot serving the interface's slot `i`.
    pub fn implement(mut self, interface: &Class, slot_map: Vec<u32>) -> Self {
        self.interfaces.push((interface.interface_id, slot_map));
        self
    }

    /// **Null when the immortal region is exhausted.** Class metadata is
    /// allocated there, and autoload can run mid-request, so a refusal is
    /// reported to the frame that asked for the class rather than taken
    /// as fatal. Whatever was already carved out stays carved: the
    /// immortal region is never reclaimed by design, and losing a few
    /// bytes on a failing load costs less than a rollback path that has
    /// no memory to run in.
    pub fn build(&mut self) -> *const Class {
        // The interned names come from `intern`, which reports the same
        // way. A null here means one of them was refused earlier, and a
        // class with a nameless property is not worth building.
        if self.name.is_null()
            || self.props.iter().any(|(n, _, _)| n.is_null())
            || self.methods.iter().any(|(n, _)| n.is_null())
        {
            return std::ptr::null();
        }

        let parent = if self.parent.is_null() {
            None
        } else {
            Some(unsafe { &*self.parent })
        };

        // Vtable: parent's layout unchanged, overrides in place, new
        // methods appended. Method table mirrors it for the slow path.
        let mut vtbl: Vec<*const ()> = if self.parent.is_null() {
            Vec::new()
        } else {
            unsafe { Class::vtbl(self.parent) }.to_vec()
        };
        let mut method_entries: Vec<(*const LLString, u32)> = parent.map_or(Vec::new(), |p| {
            unsafe { std::slice::from_raw_parts(p.methods, p.method_count as usize) }
                .iter()
                .map(|m| (m.name, m.slot))
                .collect()
        });
        let mut destruct_slot = parent.map_or(NO_DESTRUCT_SLOT, |p| p.destruct_slot);
        if parent.is_some_and(|p| p.has_destructor()) {
            self.flags |= CLASS_HAS_DESTRUCTOR;
        }

        let destruct_name = crate::intern::intern_str("__destruct");
        for &(name, code) in &self.methods {
            let slot = match method_entries.iter().find(|(n, _)| *n == name) {
                Some(&(_, slot)) => {
                    vtbl[slot as usize] = code; // override, same slot
                    slot
                }
                None => {
                    vtbl.push(code);
                    let slot = (vtbl.len() - 1) as u32;
                    method_entries.push((name, slot));
                    slot
                }
            };
            if name == destruct_name {
                destruct_slot = slot;
            }
        }

        // Property layout (`rfc/model/classes.md`, "Slot order"). Parent
        // slots keep their offsets; this class lays its own out in three
        // runs — counted pointers, then Boxes, then the rest in
        // declaration order — starting at the parent's `layout_end`, so an
        // own field may land in the parent's tail padding (JDK-15 rule).
        // Hole-filling and bit-packing are deferred optimizations (A5/A7).
        let parent_layout_end = parent.map_or(16, |p| p.layout_end);
        let parent_prop_count = parent.map_or(0, |p| p.prop_count);

        let mut props: Vec<PropSlot> = parent.map_or(Vec::new(), |p| p.props().to_vec());
        // A subclass appends at most one run per kind for its own slots.
        let mut ptr_runs: Vec<Run> = parent.map_or(Vec::new(), |p| p.ptr_runs().to_vec());
        let mut box_runs: Vec<Run> = parent.map_or(Vec::new(), |p| p.box_runs().to_vec());
        let mut undef_runs: Vec<Run> = parent.map_or(Vec::new(), |p| p.undef_runs().to_vec());

        // Classify own props, in declaration order, into the three runs.
        // Defaultless Boxes go behind the defaulted ones so the factory's
        // undef stamp is one contiguous sub-run at the box run's tail —
        // physical order is free to differ (`declaration_index` keeps the
        // observable one).
        let mut own_pointers: Vec<(*const LLString, u32)> = Vec::new();
        let mut own_boxes: Vec<(*const LLString, u32)> = Vec::new();
        let mut own_undef_boxes: Vec<(*const LLString, u32)> = Vec::new();
        let mut own_rest: Vec<(*const LLString, SlotKind, u32)> = Vec::new();
        for (i, &(name, kind, starts_undef)) in self.props.iter().enumerate() {
            let declaration_index = parent_prop_count + i as u32;
            match kind {
                SlotKind::Pointer => own_pointers.push((name, declaration_index)),
                SlotKind::Boxed if starts_undef => {
                    own_undef_boxes.push((name, declaration_index))
                }
                SlotKind::Boxed => own_boxes.push((name, declaration_index)),
                _ => own_rest.push((name, kind, declaration_index)),
            }
        }

        let align_up = |cursor: u32, align: u32| (cursor + align - 1) & !(align - 1);
        let mut cursor = parent_layout_end;

        // Counted pointers: one contiguous run, stride 8.
        if !own_pointers.is_empty() {
            cursor = align_up(cursor, 8);
            ptr_runs.push(Run {
                offset: cursor,
                count: own_pointers.len() as u32,
            });
            for &(name, declaration_index) in &own_pointers {
                props.push(PropSlot {
                    name,
                    offset: cursor,
                    kind: SlotKind::Pointer,
                    declaration_index,
                });
                cursor += 8;
            }
        }
        // Boxes: one contiguous run, stride 16; the defaultless tail is
        // additionally the factory's undef run.
        if !own_boxes.is_empty() || !own_undef_boxes.is_empty() {
            cursor = align_up(cursor, 8);
            box_runs.push(Run {
                offset: cursor,
                count: (own_boxes.len() + own_undef_boxes.len()) as u32,
            });
            if !own_undef_boxes.is_empty() {
                undef_runs.push(Run {
                    offset: cursor + own_boxes.len() as u32 * 16,
                    count: own_undef_boxes.len() as u32,
                });
            }
            for &(name, declaration_index) in own_boxes.iter().chain(&own_undef_boxes) {
                props.push(PropSlot {
                    name,
                    offset: cursor,
                    kind: SlotKind::Boxed,
                    declaration_index,
                });
                cursor += 16;
            }
        }
        // The rest, in declaration order: scalars and bools, never traced.
        for &(name, kind, declaration_index) in &own_rest {
            cursor = align_up(cursor, kind.align());
            props.push(PropSlot {
                name,
                offset: cursor,
                kind,
                declaration_index,
            });
            cursor += kind.size();
        }

        let layout_end = cursor;
        let object_size = align_up(cursor, 8);

        // Interfaces: parent's are re-linked against OUR vtable (an
        // override must flow into the inherited itable), then our own.
        let mut interface_declarations: Vec<(u32, Vec<u32>)> = parent.map_or(Vec::new(), |p| {
            unsafe { std::slice::from_raw_parts(p.interfaces, p.interface_count as usize) }
                .iter()
                .map(|e| {
                    let map =
                        unsafe { std::slice::from_raw_parts(e.slot_map, e.method_count as usize) };
                    (e.interface_id, map.to_vec())
                })
                .collect()
        });
        interface_declarations.append(&mut self.interfaces);

        // Display: parent's chain + self.
        let mut display: Vec<*const Class> = parent.map_or(Vec::new(), |p| unsafe {
            std::slice::from_raw_parts(p.display, p.display_len as usize).to_vec()
        });

        // Materialize everything in immortal metadata memory.
        let props_mem = alloc_array(&props);
        let methods_mem = alloc_array(
            &method_entries
                .iter()
                .map(|&(name, slot)| MethodEntry {
                    name,
                    slot,
                    _pad: 0,
                })
                .collect::<Vec<_>>(),
        );
        let ptr_runs_mem = alloc_array(&ptr_runs);
        let box_runs_mem = alloc_array(&box_runs);
        let undef_runs_mem = alloc_array(&undef_runs);
        if props_mem.is_null()
            || methods_mem.is_null()
            || ptr_runs_mem.is_null()
            || box_runs_mem.is_null()
            || undef_runs_mem.is_null()
        {
            return std::ptr::null();
        }

        // The dispatch train: one trailing allocation carries the vtbl
        // and every itable — [Class][vtbl][itable A][itable B]…. All of
        // them are pure code-pointer arrays (metadata lives beside the
        // tables, never inside), so the tail is a plain concatenation.
        // Slot maps are cold link-time data and stay off the train.
        let ptr = size_of::<*const ()>();
        let itables_len: usize = interface_declarations.iter().map(|(_, m)| m.len()).sum();
        let total = size_of::<Class>() + (vtbl.len() + itables_len) * ptr;
        let cls = immortal_alloc(total) as *mut Class;
        if cls.is_null() {
            return std::ptr::null();
        }

        let interface_entries: Vec<InterfaceEntry> = {
            let mut cursor = unsafe { (cls as *mut u8).add(size_of::<Class>() + vtbl.len() * ptr) }
                as *mut *const ();
            interface_declarations
                .iter()
                .map(|(id, map)| {
                    let itable = cursor as *const *const ();
                    for &s in map {
                        unsafe {
                            cursor.write(vtbl[s as usize]);
                            cursor = cursor.add(1);
                        }
                    }
                    InterfaceEntry {
                        interface_id: *id,
                        method_count: map.len() as u32,
                        itable,
                        slot_map: alloc_array(map),
                    }
                })
                .collect()
        };
        let interfaces_mem = alloc_array(&interface_entries);
        if interfaces_mem.is_null() || interface_entries.iter().any(|e| e.slot_map.is_null()) {
            return std::ptr::null();
        }
        unsafe {
            cls.write(Class {
                flags: self.flags,
                object_size,
                layout_end,
                interface_id: if self.flags & CLASS_INTERFACE != 0 {
                    NEXT_IFACE_ID.fetch_add(1, Ordering::Relaxed)
                } else {
                    0
                },
                display_len: display.len() as u32 + 1,
                prop_count: props.len() as u32,
                method_count: method_entries.len() as u32,
                interface_count: interface_entries.len() as u32,
                destruct_slot,
                vtbl_len: vtbl.len() as u32,
                ptr_run_count: ptr_runs.len() as u32,
                box_run_count: box_runs.len() as u32,
                undef_run_count: undef_runs.len() as u32,
                parent: self.parent,
                name: self.name,
                display: std::ptr::null(), // set below (self-referential)
                props: props_mem,
                methods: methods_mem,
                interfaces: interfaces_mem,
                dispose: self.dispose,
                ptr_runs: ptr_runs_mem,
                box_runs: box_runs_mem,
                undef_runs: undef_runs_mem,
            });
            display.push(cls);
            (*cls).display = alloc_array(&display);
            if (*cls).display.is_null() {
                return std::ptr::null();
            }

            let vtbl_dst = cls.add(1) as *mut *const ();
            std::ptr::copy_nonoverlapping(vtbl.as_ptr(), vtbl_dst, vtbl.len());
        }
        cls
    }
}

/// Copy a slice into immortal metadata memory. Null if the region
/// cannot grow; an empty slice needs no memory and yields a dangling
/// non-null pointer, which nothing ever dereferences.
fn alloc_array<T: Copy>(items: &[T]) -> *const T {
    if items.is_empty() {
        return std::ptr::NonNull::dangling().as_ptr();
    }
    let mem = immortal_alloc(std::mem::size_of_val(items)) as *mut T;
    if mem.is_null() {
        return std::ptr::null();
    }
    unsafe { std::ptr::copy_nonoverlapping(items.as_ptr(), mem, items.len()) };
    mem
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::intern_str;

    extern "C" fn m1() {}
    extern "C" fn m2() {}
    extern "C" fn m2_override() {}
    extern "C" fn m3() {}

    fn base() -> *const Class {
        ClassBuilder::new("Animal")
            .prop("name", true)
            .prop("age", false)
            .method("speak", m1 as *const ())
            .method("eat", m2 as *const ())
            .build()
    }

    /// Not runnable under Miri: it compares vtable entries against the
    /// functions they should hold, and Miri does not give a function a
    /// single address — `m2 as *const ()` yields a different pointer at
    /// the builder's cast site than at the assertion's. The dispatch
    /// tables round-trip exactly (verified by reading back what was
    /// written); it is function *identity* Miri cannot model. On real
    /// targets a function has one address, so this runs normally under
    /// `cargo test` and the contract is unweakened.
    #[test]
    #[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
    fn slots_are_stable_and_overrides_land_in_place() {
        let _g = crate::memory::block_pool::test_guard();
        let animal = base();
        let dog = ClassBuilder::new("Dog")
            .parent(animal)
            .method("eat", m2_override as *const ())
            .method("fetch", m3 as *const ())
            .build();

        let (animal_ptr, dog_ptr) = (animal, dog);
        let (animal, dog) = unsafe { (&*animal, &*dog) };
        let eat = intern_str("eat");
        let speak = intern_str("speak");

        assert_eq!(animal.find_method(eat), dog.find_method(eat), "slot stable");
        assert_eq!(animal.find_method(speak), dog.find_method(speak));

        let slot = dog.find_method(eat).unwrap() as usize;
        assert_eq!(unsafe { Class::vtbl(animal_ptr) }[slot], m2 as *const ());
        assert_eq!(
            unsafe { Class::vtbl(dog_ptr) }[slot],
            m2_override as *const (),
            "override in place"
        );
        assert_eq!(dog.vtbl_len, animal.vtbl_len + 1, "fetch appended");
    }

    #[test]
    fn property_offsets_inherit_and_append() {
        let _g = crate::memory::block_pool::test_guard();
        // `parent` takes the raw descriptor pointer: handing it a `&Class`
        // would narrow provenance to the fixed fields, and `build` reads
        // the parent's trailing vtable.
        let animal_ptr = base();
        let dog_ptr = ClassBuilder::new("Dog")
            .parent(animal_ptr)
            .prop("breed", true)
            .build();
        let (animal, dog) = unsafe { (&*animal_ptr, &*dog_ptr) };

        // name is a Boxed slot (the `refcounted` shim), age a Scalar: the
        // Box run is placed first, the scalar after it. object_size is the
        // exact byte count (40), not the old uniform 16-per-slot.
        assert_eq!(animal.find_prop(intern_str("name")).unwrap().offset, 16);
        assert_eq!(animal.find_prop(intern_str("age")).unwrap().offset, 32);
        assert_eq!(animal.layout_end, 40);
        assert_eq!(animal.object_size, 40);

        // Inherited offsets are unchanged; Dog's own box slot resumes at
        // the parent's layout_end (40).
        assert_eq!(dog.find_prop(intern_str("name")).unwrap().offset, 16);
        assert_eq!(dog.find_prop(intern_str("age")).unwrap().offset, 32);
        assert_eq!(dog.find_prop(intern_str("breed")).unwrap().offset, 40);
        assert_eq!(dog.object_size, 56);

        // The trace map: two Box runs (parent's name, then Dog's breed),
        // no pointer runs. The scalar `age` is in neither — never traced.
        assert_eq!(
            dog.box_runs(),
            &[Run { offset: 16, count: 1 }, Run { offset: 40, count: 1 }],
            "age is scalar-shaped, not traced"
        );
        assert!(dog.ptr_runs().is_empty());
    }

    /// A defaultless Box slot is regrouped behind the defaulted ones so
    /// the factory's undef stamp is one contiguous sub-run at the box
    /// run's tail, and a subclass appends its own undef run beside the
    /// inherited one — the same shape as the trace runs.
    #[test]
    fn defaultless_boxes_group_at_the_box_runs_tail_and_inherit() {
        let _g = crate::memory::block_pool::test_guard();
        let base_ptr = ClassBuilder::new("UndefBase")
            .prop_boxed_without_default("bare") // declared first,
            .prop("defaulted", true) // laid out last: defaulted@16, bare@32
            .build();
        let sub_ptr = ClassBuilder::new("UndefSub")
            .parent(base_ptr)
            .prop_boxed_without_default("own_bare")
            .build();
        let (base, sub) = unsafe { (&*base_ptr, &*sub_ptr) };

        assert_eq!(base.find_prop(intern_str("defaulted")).unwrap().offset, 16);
        assert_eq!(base.find_prop(intern_str("bare")).unwrap().offset, 32);
        assert_eq!(base.box_runs(), &[Run { offset: 16, count: 2 }]);
        assert_eq!(
            base.undef_runs(),
            &[Run { offset: 32, count: 1 }],
            "the defaultless tail of the box run"
        );

        // Declaration order survives the regrouping.
        assert_eq!(base.find_prop(intern_str("bare")).unwrap().declaration_index, 0);
        assert_eq!(base.find_prop(intern_str("defaulted")).unwrap().declaration_index, 1);

        // The subclass inherits the parent's undef run and appends its own.
        assert_eq!(sub.find_prop(intern_str("own_bare")).unwrap().offset, 48);
        assert_eq!(
            sub.box_runs(),
            &[Run { offset: 16, count: 2 }, Run { offset: 48, count: 1 }]
        );
        assert_eq!(
            sub.undef_runs(),
            &[Run { offset: 32, count: 1 }, Run { offset: 48, count: 1 }]
        );
    }

    /// The full slot-kind spread: a bare pointer (traced, stride-8 run), a
    /// Box (traced, stride-16 run), a scalar and a bool (never traced),
    /// each at the offset its kind and the run grouping dictate. Also pins
    /// declaration order surviving the physical regrouping, and the exact
    /// `layout_end` / `object_size` split that a subclass builds on.
    #[test]
    fn slot_kinds_lay_out_in_three_runs() {
        let _g = crate::memory::block_pool::test_guard();
        let cls_ptr = ClassBuilder::new("Mixed")
            .prop_pointer("next") // Pointer, 8
            .prop("data", true) // Boxed, 16
            .prop("id", false) // Scalar, 8
            .prop_bool("ok") // Bool, 1
            .build();
        let cls = unsafe { &*cls_ptr };

        // Pointers first, then Boxes, then the rest in declaration order.
        assert_eq!(cls.find_prop(intern_str("next")).unwrap().offset, 16);
        assert_eq!(cls.find_prop(intern_str("data")).unwrap().offset, 24);
        assert_eq!(cls.find_prop(intern_str("id")).unwrap().offset, 40);
        assert_eq!(cls.find_prop(intern_str("ok")).unwrap().offset, 48);

        // One pointer run and one Box run; the scalar and the bool are in
        // neither.
        assert_eq!(cls.ptr_runs(), &[Run { offset: 16, count: 1 }]);
        assert_eq!(cls.box_runs(), &[Run { offset: 24, count: 1 }]);

        // Slot kinds are recorded per property.
        assert_eq!(cls.find_prop(intern_str("next")).unwrap().kind, SlotKind::Pointer);
        assert_eq!(cls.find_prop(intern_str("id")).unwrap().kind, SlotKind::Scalar);
        assert_eq!(cls.find_prop(intern_str("ok")).unwrap().kind, SlotKind::Bool);

        // Declaration order is preserved though physical order regrouped.
        assert_eq!(cls.find_prop(intern_str("next")).unwrap().declaration_index, 0);
        assert_eq!(cls.find_prop(intern_str("data")).unwrap().declaration_index, 1);
        assert_eq!(cls.find_prop(intern_str("id")).unwrap().declaration_index, 2);
        assert_eq!(cls.find_prop(intern_str("ok")).unwrap().declaration_index, 3);

        // The bool ends the layout mid-word: layout_end is unrounded (49),
        // object_size is it rounded up to 8 (56), leaving 7 bytes of tail.
        assert_eq!(cls.layout_end, 49);
        assert_eq!(cls.object_size, 56);
    }

    /// A subclass field falls into the parent's tail padding (JDK-15 rule):
    /// `Mixed` ends at layout_end 49 with object_size 56, so a 1-byte bool
    /// added by a subclass lands at 49 and the object does not grow.
    #[test]
    fn subclass_reuses_the_parents_tail_padding() {
        let _g = crate::memory::block_pool::test_guard();
        let parent = ClassBuilder::new("Mixed2")
            .prop_pointer("next")
            .prop("data", true)
            .prop("id", false)
            .prop_bool("ok")
            .build();
        let sub = ClassBuilder::new("Sub")
            .parent(parent)
            .prop_bool("flag")
            .build();
        let sub = unsafe { &*sub };

        // flag sits inside the parent's [49, 56) padding, so object_size
        // stays 56 — the field cost nothing in allocation.
        assert_eq!(sub.find_prop(intern_str("flag")).unwrap().offset, 49);
        assert_eq!(sub.layout_end, 50);
        assert_eq!(sub.object_size, 56, "own field fit in the parent's tail padding");
    }

    #[test]
    fn cohen_display_answers_instanceof_in_o1() {
        let _g = crate::memory::block_pool::test_guard();
        let a = base();
        let b = ClassBuilder::new("Dog").parent(a).build();
        let c = ClassBuilder::new("Puppy").parent(b).build();
        let other = ClassBuilder::new("Rock").build();

        let (ca, cb, cc, co) = unsafe { (&*a, &*b, &*c, &*other) };
        assert!(cc.instance_of_class(ca));
        assert!(cc.instance_of_class(cb));
        assert!(cc.instance_of_class(cc));
        assert!(cb.instance_of_class(ca));
        assert!(!ca.instance_of_class(cb), "parent is not a child");
        assert!(!co.instance_of_class(ca));
        assert_eq!(cc.display_len, 3);
    }

    /// Miri-ignored for the same reason as
    /// [`slots_are_stable_and_overrides_land_in_place`]: function identity.
    #[test]
    #[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
    fn inherited_itable_sees_the_override() {
        let _g = crate::memory::block_pool::test_guard();
        let feedable = ClassBuilder::interface("Feedable");

        let animal = ClassBuilder::new("Animal")
            .method("eat", m2 as *const ())
            .implement(unsafe { &*feedable }, vec![0]) // interface slot 0 → vtbl slot 0 (eat)
            .build();
        let dog = ClassBuilder::new("Dog")
            .parent(animal)
            .method("eat", m2_override as *const ())
            .build();

        let id = unsafe { (*feedable).interface_id };
        let animal_it = unsafe { ll_find_itable(animal, id) };
        let dog_it = unsafe { ll_find_itable(dog, id) };
        assert!(!animal_it.is_null() && !dog_it.is_null());
        unsafe {
            assert_eq!(*animal_it, m2 as *const ());
            assert_eq!(
                *dog_it, m2_override as *const (),
                "inherited itable must be re-linked against the subclass vtable"
            );
        }

        let missing = unsafe { ll_find_itable(animal, 9999) };
        assert!(missing.is_null());
    }

    /// Miri-ignored for the same reason as
    /// [`slots_are_stable_and_overrides_land_in_place`]: function identity.
    #[test]
    #[cfg_attr(miri, ignore = "Miri gives one function several addresses")]
    fn itables_ride_the_descriptor_tail() {
        let _g = crate::memory::block_pool::test_guard();
        let i1 = ClassBuilder::interface("A");
        let i2 = ClassBuilder::interface("B");

        let cls = ClassBuilder::new("Train")
            .method("x", m1 as *const ())
            .method("y", m2 as *const ())
            .implement(unsafe { &*i1 }, vec![0, 1])
            .implement(unsafe { &*i2 }, vec![1])
            .build();

        let c = unsafe { &*cls };
        let tail_start = cls as usize + size_of::<Class>();
        let vtbl_end = tail_start + c.vtbl_len as usize * 8;
        let train_end = vtbl_end + 3 * 8; // 2 + 1 itable entries

        let (id1, id2) = unsafe { ((*i1).interface_id, (*i2).interface_id) };
        let t1 = unsafe { ll_find_itable(cls, id1) } as usize;
        let t2 = unsafe { ll_find_itable(cls, id2) } as usize;
        assert!(
            (vtbl_end..train_end).contains(&t1) && (vtbl_end..train_end).contains(&t2),
            "itables must live in the descriptor's own tail"
        );
        unsafe {
            assert_eq!(*(t1 as *const *const ()), m1 as *const ());
            assert_eq!(*(t2 as *const *const ()), m2 as *const ());
        }
    }

    #[test]
    fn destructor_slot_is_tracked_through_inheritance() {
        let _g = crate::memory::block_pool::test_guard();
        let animal = ClassBuilder::new("Animal")
            .destructor(m1 as *const ())
            .build();
        let dog = ClassBuilder::new("Dog").parent(animal).build();

        let (ca, cd) = unsafe { (&*animal, &*dog) };
        assert!(ca.has_destructor());
        assert!(cd.has_destructor(), "destructor presence inherits");
        assert_eq!(ca.destruct_slot, cd.destruct_slot);
        assert_ne!(ca.destruct_slot, NO_DESTRUCT_SLOT);
    }
}
