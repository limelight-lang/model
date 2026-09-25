//! The collector thread's birth and its join, by the OS entry directly.
//!
//! The pressure path births the collector at the end of the collection on
//! which the manager has just refused, so the birth may meet no allocation
//! whose refusal is an abort — `std::thread::Builder::spawn` builds the
//! thread's name, its handle's shared state and the closure's box on the
//! global heap, and every one of those aborts on refusal
//! (`dev/DECISIONS.md`, "the reset window's memory comes from the manager,
//! and an allocation it cannot get is a refusal"). The thread is made by
//! `pthread_create` on unix and `CreateThread` on windows instead, declared
//! raw as `memory::os` declares `mmap`, with no dependency taken for it.
//!
//! **The slot protocol is this module's and the entries are the arms'.**
//! [`SLOTS`] holds the thread of each slot since its last join, and
//! [`spawn`] locks the slot, joins what stood in it, creates through
//! [`platform::create`] and stores what it made; each arm exports its own
//! [`platform::Thread`], `create`, `join` and `name_this_thread`, and
//! decides nothing about the order.
//!
//! **One stack per slot, mapped once and kept.** On unix the thread runs on
//! a stack the manager maps through `memory::os` the first time the slot is
//! born, [`COLLECTOR_STACK_BYTES`] over a guard of [`STACK_GUARD_BYTES`]
//! turned `PROT_NONE`, and the mapping stays with the slot across the
//! thread's lives: a birth after the first makes one join and one create
//! and no `mmap`, and the process holds at most `MAX_COLLECTORS` stacks. The
//! thread is joinable, and the slot keeps it: the next birth of the slot,
//! and the tests' retire, join it before the stack is reused — a join
//! returns after the kernel has cleared the thread, after every TLS
//! destructor, so the stack is never handed to a thread while another
//! still runs on it. The order is the slot lock's and not the clock's: a
//! birth holds the slot's mutex from the join through the create to the
//! store of the new thread, so a slot that reads `UNBORN` before its
//! parent has stored the thread — the child's word is its own — is joined
//! by the next birth all the same, which waits on the mutex for the
//! store. A join that fails is a refused birth, never a reuse; the failure
//! glibc documents for a joinable thread is `EDEADLK`, a thread joining
//! itself, which no caller here can be, so the branch has no arm.
//! Windows has no user-stack entry, so its thread runs on the stack the OS
//! gives it, and the join is a wait on the handle.
//!
//! **The trampoline ends the life.** Every entry runs [`run_the_life`]:
//! `thread_body` under `catch_unwind`, since a panic out of an `extern`
//! function is an abort; then `ll_thread_exit`, idempotent for the path
//! that already ran it; then the state word to `UNBORN`, so the slot reads
//! unborn only after the runtime exit this thread runs itself. What
//! follows the word is glibc's teardown, the exit guard's own pass of
//! `ll_thread_exit` among its TLS destructors — a pass over a thread with
//! nothing left, taking the pool's and the registry's locks and none a
//! birth holds — and the join that follows waits on that too.
//!
//! Under Miri the std spawn stays, behind `cfg(miri)`: Miri models neither
//! a user-provided stack nor `mprotect`, and what it checks of this crate is
//! the `unsafe` code the thread runs, not how the thread was made. The
//! raw thread's own lines therefore have no Miri coverage by construction
//! (`dev/WORKFLOW.md`, "Known limits").

use std::sync::Mutex;
use std::sync::atomic::Ordering;

use super::{COLLECTORS, MAX_COLLECTORS, UNBORN, thread_body};

/// The collector thread's stack: 2 MiB, std's default for a spawned thread
/// and not a measured figure. The suite passes under it in the debug
/// profile, whose frames are the larger, and the trace's worklist is
/// arena-backed so no frame scales with the graph; a smaller figure is found
/// by lowering this under the worker suite until a run dies on the guard —
/// a bare `SIGSEGV`, since a raw thread has no handler stack to report it
/// from. On windows it is the reservation the OS is asked for.
#[cfg(not(miri))]
pub(crate) const COLLECTOR_STACK_BYTES: usize = 2 * 1024 * 1024;

/// The guard below the stack, `PROT_NONE`: a frame past the stack's low end
/// dies here instead of writing into whatever the OS mapped below. One
/// block's worth rather than one page, so that the guard and the stack
/// above it are page-aligned on a 64 KiB-page kernel as on a 4 KiB one,
/// and `memory::os` maps the pair at that alignment.
#[cfg(all(unix, not(miri)))]
pub(crate) const STACK_GUARD_BYTES: usize = crate::memory::block_pool::BLOCK_SIZE;

/// `map_aligned` takes the guard as its alignment and asks a multiple of it.
#[cfg(all(unix, not(miri)))]
const _: () = assert!(COLLECTOR_STACK_BYTES % STACK_GUARD_BYTES == 0);

/// The collector threads' names by slot, NUL-terminated for the unix arm's
/// `pthread_setname_np`, whose limit is sixteen bytes with the terminator;
/// the Miri arm hands std the name without the terminator, which std
/// refuses.
#[cfg(any(miri, target_os = "linux", target_os = "android"))]
const NAMES: [&str; MAX_COLLECTORS] = [
    "ll-collector\0",
    "ll-collector-1\0",
    "ll-collector-2\0",
    "ll-collector-3\0",
    "ll-collector-4\0",
    "ll-collector-5\0",
    "ll-collector-6\0",
    "ll-collector-7\0",
];

/// The thread of each slot since its last join, `None` for a slot whose
/// thread was joined or never made. The lock is held from a birth's join
/// through its create to its store, so a join asked for meanwhile waits for
/// the thread the birth made rather than reading an empty slot.
static SLOTS: [Mutex<Option<platform::Thread>>; MAX_COLLECTORS] =
    [const { Mutex::new(None) }; MAX_COLLECTORS];

/// Create slot `index`'s thread, joining the one that stood in the slot
/// before; true when the thread is running. False is a refused birth: a
/// stack the operating system would not map, a thread it would not create,
/// or a previous thread that could not be joined, each left for a later
/// birth to try again. The caller holds the slot at `STARTING`.
pub(super) fn spawn(index: usize) -> bool {
    let mut slot = lock(index);
    if !join_under(&mut slot) {
        return false;
    }

    let Some(thread) = platform::create(index) else {
        return false;
    };

    *slot = Some(thread);
    true
}

/// Join every slot's thread that stood since the last join, tests only:
/// the threads are asked to end first ([`super::testing::retire`]), and
/// this waits for each to be gone before the next case births again. Passes
/// until one joins nothing, because a thread joined in one pass may have
/// birthed a slot the pass had already visited; births are forbidden by
/// then, so the pass after the last birth's join is the last.
#[cfg(test)]
pub(crate) fn join_every_slot() {
    while (0..MAX_COLLECTORS).fold(false, |joined, index| join(index) | joined) {}
}

/// Join the thread that stood in slot `index`, if one does, and answer
/// whether one did: true is a thread joined, false an empty slot or a join
/// the operating system refused. Waits on the slot's lock through a birth
/// in flight, so the thread that birth stores is the one joined.
#[cfg(test)]
fn join(index: usize) -> bool {
    let mut slot = lock(index);
    let stood = slot.is_some();
    join_under(&mut slot) && stood
}

/// Join the thread `slot` holds under its lock; true when the slot holds no
/// unjoined thread afterwards, which an empty slot does trivially. A join
/// the operating system refused puts the thread back for the next attempt.
fn join_under(slot: &mut Option<platform::Thread>) -> bool {
    let Some(thread) = slot.take() else {
        return true;
    };

    if let Err(thread) = platform::join(thread) {
        *slot = Some(thread);
        return false;
    }

    true
}

/// Slot `index`'s thread under its lock, a poisoned lock read through: the
/// panic of a case that held it says nothing about the thread inside.
fn lock(index: usize) -> std::sync::MutexGuard<'static, Option<platform::Thread>> {
    SLOTS[index]
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Makes the next birth's stack read as refused by the operating system,
/// tests only: the slot keeps its mapping across lives, so a case about the
/// refusal cannot reach it through `memory::os::fault` once any birth has
/// mapped the slot. It names the stack and nothing else.
#[cfg(all(test, unix, not(miri)))]
pub(crate) static REFUSE_NEXT_STACK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Makes the next `pthread_create` read as refused — `EAGAIN`, the refusal
/// a process out of threads meets — tests only, after the stack was
/// granted. It names the create and nothing else; the guard's `mprotect`
/// has no such arm, its failure being one no test can order.
#[cfg(all(test, unix, not(miri)))]
pub(crate) static REFUSE_NEXT_CREATE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The base of slot `index`'s mapping — the guard's first byte — or zero
/// before its first birth, for a case that reads the mapping's protection
/// back from the OS.
#[cfg(all(test, unix, not(miri)))]
pub(crate) fn stack_base_of(index: usize) -> usize {
    platform::stack_base_of(index)
}

/// One life of the collector of slot `index`, on the thread the entry
/// made: the body under `catch_unwind`, the runtime exit, and the state
/// word to unborn after it.
pub(super) fn run_the_life(index: usize) {
    platform::name_this_thread(index);
    // The word below goes unborn whether the life ended by its loop or by
    // a panic, and the case that raised the panic reads it.
    let _ = std::panic::catch_unwind(|| thread_body(index));
    #[cfg(test)]
    super::testing::note_collector_life_end(index);
    // Idempotent for the loop's own ending, which ran it already; on the
    // unwind this is the exit, and it runs here on the thread rather than in
    // the guard's TLS destructor, so the word below is stored after it.
    crate::memory::heap::ll_thread_exit();
    #[cfg(test)]
    super::testing::note_exits_before_the_word(crate::memory::heap::exit_sequences());
    COLLECTORS[index].state.store(UNBORN, Ordering::Release);
}

#[cfg(all(unix, not(miri)))]
mod platform {
    use std::ffi::{c_char, c_int, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{COLLECTOR_STACK_BYTES, STACK_GUARD_BYTES, run_the_life};

    /// `pthread_t`: an unsigned long on glibc and musl, a pointer on
    /// macOS — one word on every unix this builds for.
    pub(super) type Thread = usize;

    /// An opaque `pthread_attr_t`, oversized: 56 bytes on glibc and musl
    /// x86_64, 64 on macOS, and `pthread_attr_init` writes the whole of the
    /// real size, so a buffer declared too small is a silent overwrite of the
    /// caller's frame on the pressure path. 128 bytes at 16 holds every one.
    #[repr(C, align(16))]
    struct PthreadAttr([u8; 128]);

    const PROT_NONE: c_int = 0;

    unsafe extern "C" {
        fn pthread_attr_init(attr: *mut PthreadAttr) -> c_int;
        fn pthread_attr_destroy(attr: *mut PthreadAttr) -> c_int;
        fn pthread_attr_setstack(
            attr: *mut PthreadAttr,
            stack_address: *mut c_void,
            stack_size: usize,
        ) -> c_int;
        fn pthread_create(
            thread: *mut Thread,
            attr: *const PthreadAttr,
            start: extern "C" fn(*mut c_void) -> *mut c_void,
            argument: *mut c_void,
        ) -> c_int;
        fn pthread_join(thread: Thread, result: *mut *mut c_void) -> c_int;
        fn mprotect(address: *mut c_void, length: usize, protection: c_int) -> c_int;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        fn pthread_self() -> Thread;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        fn pthread_setname_np(thread: Thread, name: *const c_char) -> c_int;
    }

    /// The stack each slot keeps across its lives, zero until the slot's
    /// first birth maps it.
    static STACKS: [AtomicUsize; super::MAX_COLLECTORS] =
        [const { AtomicUsize::new(0) }; super::MAX_COLLECTORS];

    extern "C" fn trampoline(argument: *mut c_void) -> *mut c_void {
        run_the_life(argument as usize);
        std::ptr::null_mut()
    }

    /// The thread of slot `index` on the slot's own stack, or `None` for a
    /// stack the operating system would not map and a thread it would not
    /// create. The caller holds the slot.
    pub(super) fn create(index: usize) -> Option<Thread> {
        let stack = stack_of(index);
        if stack.is_null() {
            return None;
        }

        let mut attributes = PthreadAttr([0; 128]);
        if unsafe { pthread_attr_init(&raw mut attributes) } != 0 {
            return None;
        }

        let set = unsafe {
            pthread_attr_setstack(
                &raw mut attributes,
                stack.add(STACK_GUARD_BYTES).cast(),
                COLLECTOR_STACK_BYTES,
            )
        };
        let mut thread: Thread = 0;
        #[cfg(test)]
        let refused = super::REFUSE_NEXT_CREATE.swap(false, Ordering::Relaxed);
        #[cfg(not(test))]
        let refused = false;
        let created = set == 0
            && !refused
            && unsafe {
                pthread_create(
                    &raw mut thread,
                    &raw const attributes,
                    trampoline,
                    index as *mut c_void,
                )
            } == 0;
        unsafe { pthread_attr_destroy(&raw mut attributes) };
        created.then_some(thread)
    }

    /// Wait for `thread` to be gone, the kernel's word cleared and every TLS
    /// destructor run; `Err` hands the thread back unjoined.
    pub(super) fn join(thread: Thread) -> Result<(), Thread> {
        if unsafe { pthread_join(thread, std::ptr::null_mut()) } == 0 {
            Ok(())
        } else {
            Err(thread)
        }
    }

    #[cfg(test)]
    pub(super) fn stack_base_of(index: usize) -> usize {
        STACKS[index].load(Ordering::Acquire)
    }

    /// The slot's stack, mapped with its guard on the first call and kept:
    /// null when the operating system refuses the mapping.
    fn stack_of(index: usize) -> *mut u8 {
        #[cfg(test)]
        if super::REFUSE_NEXT_STACK.swap(false, Ordering::Relaxed) {
            return std::ptr::null_mut();
        }

        let mapped = STACKS[index].load(Ordering::Acquire);
        if mapped != 0 {
            return mapped as *mut u8;
        }

        let bytes = STACK_GUARD_BYTES + COLLECTOR_STACK_BYTES;
        let base = crate::memory::os::map_aligned(bytes, STACK_GUARD_BYTES);
        if base.is_null() {
            return base;
        }

        if unsafe { mprotect(base.cast(), STACK_GUARD_BYTES, PROT_NONE) } != 0 {
            crate::memory::os::unmap(base, bytes);
            return std::ptr::null_mut();
        }

        // The caller holds the slot at `STARTING`, so no second birth maps
        // beside this one.
        STACKS[index].store(base as usize, Ordering::Release);
        base
    }

    pub(super) fn name_this_thread(index: usize) {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        unsafe {
            pthread_setname_np(pthread_self(), super::NAMES[index].as_ptr().cast());
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let _ = index;
    }
}

#[cfg(all(windows, not(miri)))]
mod platform {
    use std::ffi::c_void;

    use super::{COLLECTOR_STACK_BYTES, run_the_life};

    /// A `HANDLE`, kept as a word so the slot's mutex is `Sync`. The stack
    /// is the OS's: `CreateThread` takes a size and not a caller's memory.
    pub(super) type Thread = usize;

    const STACK_SIZE_PARAM_IS_A_RESERVATION: u32 = 0x0001_0000;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT_OBJECT_0: u32 = 0;

    unsafe extern "system" {
        fn CreateThread(
            attributes: *mut c_void,
            stack_size: usize,
            start: extern "system" fn(*mut c_void) -> u32,
            parameter: *mut c_void,
            creation_flags: u32,
            thread_id: *mut u32,
        ) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    extern "system" fn trampoline(parameter: *mut c_void) -> u32 {
        run_the_life(parameter as usize);
        0
    }

    /// The thread of slot `index` on a stack the OS reserves, or `None` for
    /// one it would not create. The caller holds the slot.
    pub(super) fn create(index: usize) -> Option<Thread> {
        let handle = unsafe {
            CreateThread(
                std::ptr::null_mut(),
                COLLECTOR_STACK_BYTES,
                trampoline,
                index as *mut c_void,
                STACK_SIZE_PARAM_IS_A_RESERVATION,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() {
            return None;
        }

        Some(handle as Thread)
    }

    /// Wait for `thread` to be gone and close its handle; `Err` hands the
    /// handle back unwaited and unclosed.
    pub(super) fn join(thread: Thread) -> Result<(), Thread> {
        let handle = thread as *mut c_void;
        if unsafe { WaitForSingleObject(handle, INFINITE) } != WAIT_OBJECT_0 {
            return Err(thread);
        }

        unsafe { CloseHandle(handle) };
        Ok(())
    }

    /// No name: `SetThreadDescription` takes a wide string, and a name is
    /// what a debugger shows, not what the crate reads.
    pub(super) fn name_this_thread(_index: usize) {}
}

#[cfg(miri)]
mod platform {
    use super::{NAMES, run_the_life};

    pub(super) type Thread = std::thread::JoinHandle<()>;

    /// The thread of slot `index` from std, the instrument's exemption: the
    /// name without its terminator, which std refuses.
    pub(super) fn create(index: usize) -> Option<Thread> {
        std::thread::Builder::new()
            .name(NAMES[index].trim_end_matches('\0').into())
            .spawn(move || run_the_life(index))
            .ok()
    }

    /// Wait for `thread` to be gone. A join fails on nothing here: the
    /// body's panic was caught on the thread.
    pub(super) fn join(thread: Thread) -> Result<(), Thread> {
        let _ = thread.join();
        Ok(())
    }

    /// The name is the spawn's, given at the birth.
    pub(super) fn name_this_thread(_index: usize) {}
}
