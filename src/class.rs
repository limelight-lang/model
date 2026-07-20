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
//! `instanceof`, property layout with fixed 16-byte Box slots.
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

/// Property slot flags.
pub const PROP_REFCOUNTED: u32 = 1 << 0;

/// A declared property: fixed offset from the object base, computed at
/// link time. Slots are 16-byte Boxes in phase 1 (unboxed slots are a
/// compiler contract layered on later).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PropSlot {
    pub name: *const LLString,
    pub offset: u32,
    pub flags: u32,
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
pub struct IfaceEntry {
    pub iface_id: u32,
    pub method_count: u32,
    pub itable: *const *const (),
    /// interface slot → class vtable slot.
    pub slot_map: *const u32,
}

#[repr(C)]
pub struct Class {
    pub flags: u32,
    /// Instance allocation size: 16-byte header+class, then prop slots.
    pub object_size: u32,
    /// Identity for interface dispatch; meaningful when
    /// `CLASS_INTERFACE` is set.
    pub iface_id: u32,
    /// Cohen display length; own depth = `display_len - 1`.
    pub display_len: u32,
    pub prop_count: u32,
    pub method_count: u32,
    pub iface_count: u32,
    /// Vtable slot of `__destruct`, or [`NO_DESTRUCT_SLOT`].
    pub destruct_slot: u32,
    pub vtbl_len: u32,
    _pad: u32,
    pub parent: *const Class,
    pub name: *const LLString,
    /// Ancestors root→self, indexed by depth: `instanceof` is one load
    /// + compare (Cohen display, `rfc/model/lowering.md`).
    pub display: *const *const Class,
    pub props: *const PropSlot,
    pub methods: *const MethodEntry,
    pub interfaces: *const IfaceEntry,
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

    /// Offsets of property slots that may hold counted references —
    /// the contract GC tracing and phase-2 teardown consume.
    pub fn refcounted_slots(&self) -> impl Iterator<Item = u32> + '_ {
        self.props()
            .iter()
            .filter(|p| p.flags & PROP_REFCOUNTED != 0)
            .map(|p| p.offset)
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
    pub fn find_iface(&self, iface_id: u32) -> Option<&IfaceEntry> {
        let entries =
            unsafe { std::slice::from_raw_parts(self.interfaces, self.iface_count as usize) };
        entries.iter().find(|e| e.iface_id == iface_id)
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
pub unsafe extern "C" fn ll_find_itable(cls: *const Class, iface_id: u32) -> *const *const () {
    match unsafe { (*cls).find_iface(iface_id) } {
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
    props: Vec<(*const LLString, u32)>,
    methods: Vec<(*const LLString, *const ())>,
    interfaces: Vec<(u32, Vec<u32>)>,
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
        }
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

    /// Declare a property. `refcounted` marks slots that can hold
    /// counted references (traced by GC, released at drop).
    pub fn prop(mut self, name: &str, refcounted: bool) -> Self {
        let flags = if refcounted { PROP_REFCOUNTED } else { 0 };
        self.props.push((crate::intern::intern_str(name), flags));
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
    pub fn implement(mut self, iface: &Class, slot_map: Vec<u32>) -> Self {
        self.interfaces.push((iface.iface_id, slot_map));
        self
    }

    pub fn build(&mut self) -> *const Class {
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

        // Properties: parent slots first (offsets stable), own appended.
        let mut props: Vec<PropSlot> = parent.map_or(Vec::new(), |p| {
            p.props()
                .iter()
                .map(|s| PropSlot {
                    name: s.name,
                    offset: s.offset,
                    flags: s.flags,
                })
                .collect()
        });
        for &(name, flags) in &self.props {
            let offset = 16 + props.len() as u32 * 16;
            props.push(PropSlot {
                name,
                offset,
                flags,
            });
        }
        let object_size = 16 + props.len() as u32 * 16;

        // Interfaces: parent's are re-linked against OUR vtable (an
        // override must flow into the inherited itable), then our own.
        let mut iface_decls: Vec<(u32, Vec<u32>)> = parent.map_or(Vec::new(), |p| {
            unsafe { std::slice::from_raw_parts(p.interfaces, p.iface_count as usize) }
                .iter()
                .map(|e| {
                    let map =
                        unsafe { std::slice::from_raw_parts(e.slot_map, e.method_count as usize) };
                    (e.iface_id, map.to_vec())
                })
                .collect()
        });
        iface_decls.append(&mut self.interfaces);

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

        // The dispatch train: one trailing allocation carries the vtbl
        // and every itable — [Class][vtbl][itable A][itable B]…. All of
        // them are pure code-pointer arrays (metadata lives beside the
        // tables, never inside), so the tail is a plain concatenation.
        // Slot maps are cold link-time data and stay off the train.
        let ptr = size_of::<*const ()>();
        let itables_len: usize = iface_decls.iter().map(|(_, m)| m.len()).sum();
        let total = size_of::<Class>() + (vtbl.len() + itables_len) * ptr;
        let cls = immortal_alloc(total) as *mut Class;

        let iface_entries: Vec<IfaceEntry> = {
            let mut cursor = unsafe { (cls as *mut u8).add(size_of::<Class>() + vtbl.len() * ptr) }
                as *mut *const ();
            iface_decls
                .iter()
                .map(|(id, map)| {
                    let itable = cursor as *const *const ();
                    for &s in map {
                        unsafe {
                            cursor.write(vtbl[s as usize]);
                            cursor = cursor.add(1);
                        }
                    }
                    IfaceEntry {
                        iface_id: *id,
                        method_count: map.len() as u32,
                        itable,
                        slot_map: alloc_array(map),
                    }
                })
                .collect()
        };
        let ifaces_mem = alloc_array(&iface_entries);
        unsafe {
            cls.write(Class {
                flags: self.flags,
                object_size,
                iface_id: if self.flags & CLASS_INTERFACE != 0 {
                    NEXT_IFACE_ID.fetch_add(1, Ordering::Relaxed)
                } else {
                    0
                },
                display_len: display.len() as u32 + 1,
                prop_count: props.len() as u32,
                method_count: method_entries.len() as u32,
                iface_count: iface_entries.len() as u32,
                destruct_slot,
                vtbl_len: vtbl.len() as u32,
                _pad: 0,
                parent: self.parent,
                name: self.name,
                display: std::ptr::null(), // set below (self-referential)
                props: props_mem,
                methods: methods_mem,
                interfaces: ifaces_mem,
            });
            display.push(cls);
            (*cls).display = alloc_array(&display);

            let vtbl_dst = cls.add(1) as *mut *const ();
            std::ptr::copy_nonoverlapping(vtbl.as_ptr(), vtbl_dst, vtbl.len());
        }
        cls
    }
}

/// Copy a slice into immortal metadata memory.
fn alloc_array<T: Copy>(items: &[T]) -> *const T {
    if items.is_empty() {
        return std::ptr::NonNull::dangling().as_ptr();
    }
    let mem = immortal_alloc(std::mem::size_of_val(items)) as *mut T;
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

    #[test]
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
        let animal = unsafe { &*base() };
        let dog = unsafe {
            &*ClassBuilder::new("Dog")
                .parent(animal)
                .prop("breed", true)
                .build()
        };

        assert_eq!(animal.find_prop(intern_str("name")).unwrap().offset, 16);
        assert_eq!(animal.find_prop(intern_str("age")).unwrap().offset, 32);
        assert_eq!(dog.find_prop(intern_str("name")).unwrap().offset, 16);
        assert_eq!(dog.find_prop(intern_str("breed")).unwrap().offset, 48);
        assert_eq!(dog.object_size, 64);
        assert_eq!(
            dog.refcounted_slots().collect::<Vec<_>>(),
            vec![16, 48],
            "age is unboxed-scalar-shaped, not traced"
        );
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

    #[test]
    fn inherited_itable_sees_the_override() {
        let _g = crate::memory::block_pool::test_guard();
        let feedable = ClassBuilder::interface("Feedable");

        let animal = ClassBuilder::new("Animal")
            .method("eat", m2 as *const ())
            .implement(unsafe { &*feedable }, vec![0]) // iface slot 0 → vtbl slot 0 (eat)
            .build();
        let dog = ClassBuilder::new("Dog")
            .parent(animal)
            .method("eat", m2_override as *const ())
            .build();

        let id = unsafe { (*feedable).iface_id };
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

    #[test]
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

        let (id1, id2) = unsafe { ((*i1).iface_id, (*i2).iface_id) };
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
