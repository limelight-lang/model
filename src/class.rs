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
//! lists (`ptr_runs` / `box_runs`) the GC and teardown stride, and the
//! tail byte block carrying the init bitmap (A5).
//!
//! **Dispatch tables are pure code-pointer arrays** — no headers inside
//! any table, so the tail is a homogeneous train:
//! `[Class][vtbl][itable A][itable B]…` in one allocation, every table
//! found by link-time pointer/offset (`rfc/model/classes.md`, "Pure
//! pointer tables, one trailing train").
//!
//! Deliberately absent (generated-code territory or later layers):
//! inline caches, property hooks, Ghost/Proxy shims, `__call`,
//! dynamic properties.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::memory::immortal::immortal_alloc;
use crate::string::LLString;

pub const CLASS_FINAL: u32 = 1 << 0;
pub const CLASS_ABSTRACT: u32 = 1 << 1;
pub const CLASS_INTERFACE: u32 = 1 << 2;
/// The class declares (or inherits) a `__destruct` with side effects.
pub const CLASS_HAS_DESTRUCTOR: u32 = 1 << 3;
/// Instances are interpolated-string templates
/// (`crate::template::Template`): the body is a shape pointer followed by
/// as many `Value`s as that shape declares, so the counted children are
/// found through it and not through this class's runs, which are empty.
/// One class serves every interpolation site, which is why the count
/// belongs to the instance.
pub const CLASS_TEMPLATE: u32 = 1 << 4;

/// This class owns counted cells outside its own body, and
/// [`field@Class::outside`] points at the group of behaviours that reach
/// them. The predicate is a flag rather than a null test so that a class
/// without such cells loads nothing extra: `flags` is already in a
/// register wherever this is asked (`dev/DECISIONS.md`, "a class with
/// cells outside itself carries one flag and one group of five").
pub const CLASS_OUTSIDE_CELLS: u32 = 1 << 5;

/// No `__destruct`.
pub const NO_DESTRUCT_SLOT: u32 = u32::MAX;

/// The property is not bitmap-tracked ([`PropSlot::init_bit`]).
pub const NO_INIT_BIT: u32 = u32::MAX;

/// The machine representation of a declared property's type — what the
/// slot physically *is*, chosen at link time (`rfc/model/classes.md`,
/// "Slot kinds"). Traced-ness and stride both follow from the kind, so
/// there is no separate per-slot "is a reference" flag.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotKind {
    /// Raw `i64` / `f64` (an `int` / `float` property): 8 bytes, never
    /// traced.
    Scalar = 0,
    /// A bare 8-byte pointer to a counted entity — a declared class type,
    /// `string` or `array`. For the non-nullable declaration `NULL` means
    /// uninitialized; a nullable `?T` rides the same niche with `NULL` as
    /// PHP null, and tracks uninitialized in the init bitmap. Traced as a
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
/// link time. Physical order groups the runs and so differs from
/// declaration order, which `serialize()`/`foreach`/reflection observe;
/// `declaration_index` preserves it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PropSlot {
    pub name: *const LLString,
    pub offset: u32,
    pub kind: SlotKind,
    pub declaration_index: u32,
    /// Absolute bit position of the slot's init-bitmap bit in the byte
    /// block, or [`NO_INIT_BIT`]. Only raw slots with no marker of their
    /// own carry one — a defaultless `?T` pointer (`NULL` is PHP null
    /// there) or a defaultless scalar/bool (`rfc/model/values.md`,
    /// "Uninitialized properties"); clear = uninitialized.
    pub init_bit: u32,
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

/// One class descriptor: these fixed fields, then an **inline trailing
/// vtable** of `vtbl_len` code pointers with the itables behind it, in
/// one immortal allocation ([`ClassBuilder::build`]) whose address is
/// stable for the process lifetime. Every count and table below covers
/// the inherited chain as well, so no lookup here walks
/// [`field@Class::parent`].
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
    /// The [`crate::walk::OutsideCells`] group when
    /// [`CLASS_OUTSIDE_CELLS`] is set, null otherwise. Erased for the
    /// same reason `dispose` is: the group names crate-private types and
    /// this struct is public.
    pub outside: *const (),
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
    /// The group of behaviours for cells this class owns outside its own
    /// body, or `None` for every class that has none — which is every
    /// class the compiler emits today.
    ///
    /// # Safety
    /// `cls` points at a linked class descriptor.
    #[inline]
    pub(crate) unsafe fn outside_cells<'a>(
        cls: *const Class,
    ) -> Option<&'a crate::walk::OutsideCells> {
        if unsafe { (*cls).flags } & CLASS_OUTSIDE_CELLS == 0 {
            return None;
        }

        Some(unsafe { &*((*cls).outside as *const crate::walk::OutsideCells) })
    }

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

    /// [`field@Self::ptr_runs`] as a slice — the GC and teardown consumer
    /// contract for bare-pointer slots.
    #[inline]
    pub fn ptr_runs(&self) -> &[Run] {
        unsafe { std::slice::from_raw_parts(self.ptr_runs, self.ptr_run_count as usize) }
    }

    /// [`field@Self::box_runs`] as a slice — the same contract for
    /// `mixed`/untyped Box slots.
    #[inline]
    pub fn box_runs(&self) -> &[Run] {
        unsafe { std::slice::from_raw_parts(self.box_runs, self.box_run_count as usize) }
    }

    /// [`field@Self::undef_runs`] as a slice — the factory's undef-stamp
    /// list, a sub-range of [`Self::box_runs`].
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
    /// `(name, kind, tracked_uninitialized)` — the declaration criterion
    /// of `rfc/model/values.md`: declared without an initializer AND
    /// needing explicit tracking. Boxed → the factory's undef stamp;
    /// Scalar/Bool, and Pointer for a nullable `?T` → an init-bitmap
    /// bit. A defaultless non-nullable pointer stays untracked: `NULL`
    /// itself is its marker.
    props: Vec<(*const LLString, SlotKind, bool)>,
    methods: Vec<(*const LLString, *const ())>,
    interfaces: Vec<(u32, Vec<u32>)>,
    /// `None` until this class declares one, so that `build` can tell
    /// "declared nothing" from "declared the default" and inherit.
    dispose: Option<*const ()>,
    outside: *const (),
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
            dispose: None,
            outside: std::ptr::null(),
        }
    }

    /// Install a specialized `dispose` (`crate::object::DisposeFn`),
    /// modeling the per-class teardown the compiler would generate. Not
    /// inherited: a subclass that declares none gets
    /// [`crate::object::ll_default_dispose`], which strides the runs it
    /// inherited rather than only the parent's.
    ///
    /// **A specialized dispose on a class with outside cells owes the
    /// group's `free`**, which the default one makes for it.
    pub fn dispose(mut self, code: *const ()) -> Self {
        self.dispose = Some(code);
        self
    }

    /// Install the group of behaviours for counted cells this class owns
    /// outside its own body ([`crate::walk::OutsideCells`]), which also
    /// raises [`CLASS_OUTSIDE_CELLS`]. A subclass inherits it.
    ///
    /// The group is taken whole. A class carrying some of its members and
    /// not others fails silently in both directions, which is why there
    /// is one pointer here rather than one per behaviour.
    ///
    /// The first caller is `limelight-lang/io`'s coroutine, outside this
    /// crate; the tests here declare one of their own.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "the first caller is another crate")
    )]
    pub(crate) fn outside_cells(mut self, group: &'static crate::walk::OutsideCells) -> Self {
        self.outside = group as *const crate::walk::OutsideCells as *const ();
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

    /// The template class ([`CLASS_TEMPLATE`]). Declares no properties:
    /// a template's values are counted by its shape, not by this class.
    pub fn template(mut self) -> Self {
        self.flags |= CLASS_TEMPLATE;
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

    /// Declare a nullable pointer property (`?Foo`/`?string`/`?array`)
    /// **without a default**: the same bare-pointer slot (`NULL` is PHP
    /// null, the niche), so its uninitialized state is an init-bitmap
    /// bit. The defaulted form is [`Self::prop_pointer`]-shaped and
    /// untracked.
    pub fn prop_nullable_pointer_without_default(self, name: &str) -> Self {
        self.prop_with(name, SlotKind::Pointer, true)
    }

    /// Declare an `int`/`float` property **without a default**: `0` is a
    /// real value, so uninitialized is an init-bitmap bit.
    pub fn prop_scalar_without_default(self, name: &str) -> Self {
        self.prop_with(name, SlotKind::Scalar, true)
    }

    /// Declare a `bool` property **without a default**: same rule as the
    /// scalar, `false` is a real value.
    pub fn prop_bool_without_default(self, name: &str) -> Self {
        self.prop_with(name, SlotKind::Bool, true)
    }

    fn prop_of(self, name: &str, kind: SlotKind) -> Self {
        self.prop_with(name, kind, false)
    }

    fn prop_with(mut self, name: &str, kind: SlotKind, tracked_uninitialized: bool) -> Self {
        self.props
            .push((crate::intern::intern_str(name), kind, tracked_uninitialized));
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
        // A template's body is its shape's, not this class's, and the
        // walkers that branch on the flag return before they read a run —
        // so a property here would be traced by nobody
        // (`crate::template`).
        debug_assert!(
            self.flags & CLASS_TEMPLATE == 0 || self.props.is_empty(),
            "a template class cannot carry properties"
        );
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
        // declaration order — then the byte block (the init bitmap),
        // starting at the parent's `layout_end`, so an own field may land
        // in the parent's tail padding (JDK-15 rule). Hole-filling and
        // bool bit-packing are deferred optimizations (A7 / rfc backlog).
        // A rootless class starts at 16, past the object header
        // (`crate::object::Object`: `RcHeader` + class pointer).
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
        let mut own_pointers: Vec<(*const LLString, u32, bool)> = Vec::new();
        let mut own_boxes: Vec<(*const LLString, u32)> = Vec::new();
        let mut own_undef_boxes: Vec<(*const LLString, u32)> = Vec::new();
        let mut own_rest: Vec<(*const LLString, SlotKind, u32, bool)> = Vec::new();
        for (i, &(name, kind, tracked)) in self.props.iter().enumerate() {
            let declaration_index = parent_prop_count + i as u32;
            match kind {
                SlotKind::Pointer => own_pointers.push((name, declaration_index, tracked)),
                SlotKind::Boxed if tracked => own_undef_boxes.push((name, declaration_index)),
                SlotKind::Boxed => own_boxes.push((name, declaration_index)),
                _ => own_rest.push((name, kind, declaration_index, tracked)),
            }
        }

        let align_up = |cursor: u32, align: u32| (cursor + align - 1) & !(align - 1);
        let mut cursor = parent_layout_end;
        // Bitmap-tracked own slots, as (index into `props`,
        // declaration_index): bits are handed out after the layout, when
        // the byte block's offset is known.
        let mut bitmap_slots: Vec<(usize, u32)> = Vec::new();

        // Counted pointers: one contiguous run, stride 8.
        if !own_pointers.is_empty() {
            cursor = align_up(cursor, 8);
            ptr_runs.push(Run {
                offset: cursor,
                count: own_pointers.len() as u32,
            });

            for &(name, declaration_index, tracked) in &own_pointers {
                if tracked {
                    bitmap_slots.push((props.len(), declaration_index));
                }

                props.push(PropSlot {
                    name,
                    offset: cursor,
                    kind: SlotKind::Pointer,
                    declaration_index,
                    init_bit: NO_INIT_BIT,
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
                    init_bit: NO_INIT_BIT,
                });

                cursor += 16;
            }
        }

        // The rest, in declaration order: scalars and bools, never traced.
        for &(name, kind, declaration_index, tracked) in &own_rest {
            cursor = align_up(cursor, kind.align());
            if tracked {
                bitmap_slots.push((props.len(), declaration_index));
            }

            props.push(PropSlot {
                name,
                offset: cursor,
                kind,
                declaration_index,
                init_bit: NO_INIT_BIT,
            });

            cursor += kind.size();
        }

        // The byte block (`rfc/model/classes.md`): this class's init
        // bitmap, one bit per bitmap-tracked own slot in declaration
        // order, appended at the layout tail. `init_bit` is an absolute
        // bit position from the object base; a subclass block is its own,
        // so parent bits never move. The factory's zero-fill clears the
        // block — every tracked slot starts uninitialized for free.
        if !bitmap_slots.is_empty() {
            bitmap_slots.sort_by_key(|&(_, declaration_index)| declaration_index);
            for (bit, &(prop_index, _)) in bitmap_slots.iter().enumerate() {
                props[prop_index].init_bit = cursor * 8 + bit as u32;
            }

            cursor += bitmap_slots.len().div_ceil(8) as u32;
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

        // **The group is inherited and `dispose` is not**, and the
        // asymmetry is the point. A specialized dispose releases the
        // slots of the class that declared it, and a subclass has more:
        // handing it down would release the parent's children and leak
        // every slot the subclass added. What a subclass needs is
        // `ll_default_dispose`, which strides the full runs it inherited
        // and frees the group's storage as its last act — so inheriting
        // the group is what makes not inheriting the dispose safe
        // (`dev/DECISIONS.md`, "a class with cells outside itself carries
        // one flag and one group of five").
        let dispose = self
            .dispose
            .unwrap_or(crate::object::ll_default_dispose as *const ());
        let outside = if self.outside.is_null() {
            parent.map_or(std::ptr::null(), |p| p.outside)
        } else {
            self.outside
        };
        let flags = self.flags
            | if outside.is_null() {
                0
            } else {
                CLASS_OUTSIDE_CELLS
            };

        unsafe {
            cls.write(Class {
                flags,
                object_size,
                layout_end,
                interface_id: if flags & CLASS_INTERFACE != 0 {
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
                dispose,
                outside,
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
mod tests;
