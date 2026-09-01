//! Memory straight from the operating system: `mmap` on unix,
//! `VirtualAlloc` on windows.
//!
//! Every layer above this one is built on a promise that a refusal is
//! **reported** — `BlockPool::get` answers null, the caller decides. Rust's
//! own allocator cannot keep that promise: `Vec`, `Box` and
//! `std::alloc::alloc`'s callers route a refusal into `handle_alloc_error`,
//! which aborts the process, and no caller can see it coming. So the pool's
//! own memory comes from here instead, and the pool's path holds no `Vec`
//! at all (`rfc/model/memory/heap-slot-allocation.md`; `PLAN.md` S34.9).
//!
//! It also unblocks installing this manager as Rust's `#[global_allocator]`.
//! A region carved through Rust's allocator re-enters `ll_alloc` under such
//! an install, with an alignment the small path refuses, so every
//! allocation reports null; carving from the operating system has no such
//! edge (`stdapi.rs`).
//!
//! No dependency is taken for it. The crate has none, and the four symbols
//! below are declared here and resolved by the C runtime that std links
//! anyway.

/// Reserve and commit `bytes` of zero-filled memory whose address is a
/// multiple of `align`, or null when the operating system refuses.
///
/// `bytes` must be a multiple of `align`, and `align` a power of two of at
/// least one page. The memory is readable and writable, private to this
/// process, and belongs to the caller until it passes the same pointer and
/// the same `bytes` to [`unmap`].
///
/// **Zero-filled by the operating system**, which is what lets a fresh
/// region be handed out as blocks without a memset.
pub(crate) fn map_aligned(bytes: usize, align: usize) -> *mut u8 {
    debug_assert!(align.is_power_of_two());
    debug_assert_eq!(bytes % align, 0);

    #[cfg(test)]
    if fault::takes_the_refusal() {
        return std::ptr::null_mut();
    }

    imp::map_aligned(bytes, align)
}

/// A refusal on demand, tests only.
///
/// The operating system cannot be made to refuse a mapping to order, and
/// every caller above this module reports exhaustion rather than aborting
/// **only** on the branch that refusal takes — an untested branch there is
/// a guess (`PLAN.md` S34.9).
#[cfg(test)]
pub(crate) mod fault {
    use std::sync::atomic::{AtomicIsize, Ordering};

    /// Mappings still granted before one is refused. Negative means no
    /// refusal is armed, which is the state every test starts in.
    static GRANTS_LEFT: AtomicIsize = AtomicIsize::new(-1);

    /// An arming that disarms itself on the way out of the scope,
    /// including the way out a panic takes.
    ///
    /// An arming a test leaves standing lands on the next test to map
    /// anything, which then fails where the defect is not — the reason
    /// `array/entity/tests/what_a_refused_copy_gives_back.rs` keeps the
    /// same guard for the pool's own refusal flag.
    pub(crate) struct Refusing;

    impl Refusing {
        /// Grant `grants` more mappings, then refuse one.
        pub(crate) fn after(grants: usize) -> Self {
            GRANTS_LEFT.store(grants as isize, Ordering::Relaxed);

            Refusing
        }
    }

    impl Drop for Refusing {
        fn drop(&mut self) {
            GRANTS_LEFT.store(-1, Ordering::Relaxed);
        }
    }

    /// Whether this mapping is the one to refuse. Consumes the arming, so
    /// a test that forgets to disarm still refuses exactly once.
    pub(super) fn takes_the_refusal() -> bool {
        loop {
            let left = GRANTS_LEFT.load(Ordering::Relaxed);
            if left < 0 {
                return false;
            }

            let next = left - 1;
            if GRANTS_LEFT
                .compare_exchange(left, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return next < 0;
            }
        }
    }
}

/// Give back a mapping obtained from [`map_aligned`].
///
/// `ptr` and `bytes` must be the pointer that call returned and the size it
/// was asked for; a partial release is not offered, because the trimming
/// unix needs for alignment has already happened by the time the caller
/// sees the pointer.
pub(crate) fn unmap(ptr: *mut u8, bytes: usize) {
    imp::unmap(ptr, bytes)
}

#[cfg(unix)]
mod imp {
    use std::ffi::c_void;

    const PROT_READ: i32 = 1;
    const PROT_WRITE: i32 = 2;
    const MAP_PRIVATE: i32 = 2;

    // The mapping call's ABI, before its flags: `off_t` is 32-bit on
    // 32-bit unix without large-file support, so the `i64` declared below
    // would put the offset where the callee does not read it. Nothing
    // targets 32-bit here, and a build that starts to stops instead of
    // discovering it as an intermittent refusal.
    #[cfg(not(target_pointer_width = "64"))]
    compile_error!(
        "memory::os declares mmap's offset as i64, which matches off_t only on 64-bit unix"
    );

    // Enumerated rather than defaulted, because a wrong value here is the worst
    // shape of defect this module can produce: `mmap` refuses with `EBADF`, the
    // pool reports exhaustion, and every allocation in the process fails on a
    // machine with all its memory free — a report indistinguishable from real
    // exhaustion. An unported platform stops the build instead, the way the
    // per-process key's own unix-only `/dev/urandom` read does (`PLAN.md`, "The
    // per-process key's Windows randomness source").
    //
    // Linux carries the architecture in the condition as well as the
    // operating system: mips defines the flag as 0x0800, and 0x20 there
    // is `MAP_RENAME`, so a target_os arm alone would let exactly one
    // platform through the gate it exists to close.
    #[cfg(all(
        any(target_os = "linux", target_os = "android"),
        not(any(
            target_arch = "mips",
            target_arch = "mips32r6",
            target_arch = "mips64",
            target_arch = "mips64r6"
        ))
    ))]
    const MAP_ANONYMOUS: i32 = 0x20;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    const MAP_ANONYMOUS: i32 = 0x1000;
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    const MAP_ANONYMOUS: i32 = 0x100;
    #[cfg(not(any(
        all(
            any(target_os = "linux", target_os = "android"),
            not(any(
                target_arch = "mips",
                target_arch = "mips32r6",
                target_arch = "mips64",
                target_arch = "mips64r6"
            ))
        ),
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "solaris",
        target_os = "illumos"
    )))]
    compile_error!(
        "memory::os has no verified MAP_ANONYMOUS for this target; add it rather than guessing"
    );

    const MAP_FAILED: *mut c_void = usize::MAX as *mut c_void;

    unsafe extern "C" {
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
        fn munmap(addr: *mut c_void, len: usize) -> i32;
    }

    /// `mmap` guarantees page alignment and nothing more, so an aligned
    /// span is cut out of an oversized one: ask for `bytes + align`, keep
    /// the aligned span inside it, and hand the head and the tail back.
    /// The two `munmap`s are what keep the waste at zero rather than at
    /// one alignment per region.
    pub(super) fn map_aligned(bytes: usize, align: usize) -> *mut u8 {
        let Some(over) = bytes.checked_add(align) else {
            return std::ptr::null_mut();
        };

        let base = unsafe {
            mmap(
                std::ptr::null_mut(),
                over,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == MAP_FAILED || base.is_null() {
            return std::ptr::null_mut();
        }

        let base = base as usize;
        let aligned = (base + align - 1) & !(align - 1);

        // **Under Miri the oversized mapping is kept whole**, and
        // [`whole`] remembers it so that [`unmap`] can hand back the
        // same span it was given. Trimming means unmapping part of a
        // mapping, which POSIX allows and Miri's `munmap` shim does not
        // model: it reports "incorrect layout on deallocation" and ends
        // the run, and since the first `BlockPool::get` of any test
        // carves a region that put the whole crate out of reach of the
        // one tool that sees its formal-UB class. What the arm costs is
        // stated where the command is (`dev/WORKFLOW.md`, Miri): the
        // mapping is wider than the object at both ends, so an access
        // just past a region or a run lands inside a live allocation
        // instead of outside one.
        #[cfg(miri)]
        whole::remember(aligned, base, over);

        #[cfg(not(miri))]
        {
            let head = aligned - base;
            if head != 0 {
                unsafe { munmap(base as *mut c_void, head) };
            }

            let tail = over - head - bytes;
            if tail != 0 {
                unsafe { munmap((aligned + bytes) as *mut c_void, tail) };
            }
        }

        aligned as *mut u8
    }

    /// The mappings this module made and has not returned, under Miri
    /// only.
    ///
    /// It exists because Miri's `munmap` refuses a partial call: the
    /// trim [`map_aligned`] performs everywhere else cannot run there, so
    /// the aligned span a caller holds is smaller than the mapping it
    /// sits in and only this table knows where that mapping starts.
    /// Keeping it is what lets an unmap stay an unmap under Miri, rather
    /// than becoming a leak the interpreter cannot see past.
    ///
    /// A vector and a linear scan: the population is regions and large
    /// runs, tens of entries, and Miri costs orders of magnitude more per
    /// instruction than the scan does.
    #[cfg(miri)]
    mod whole {
        use std::sync::Mutex;

        /// `(aligned, base, over)` — what was handed out, where the
        /// mapping starts, and how long it is.
        static MAPPINGS: Mutex<Vec<(usize, usize, usize)>> = Mutex::new(Vec::new());

        pub(super) fn remember(aligned: usize, base: usize, over: usize) {
            MAPPINGS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((aligned, base, over));
        }

        /// The mapping an aligned pointer sits in, forgotten as it is
        /// answered. `None` says the caller is unmapping something this
        /// module never handed out — a sub-range, or a pointer from
        /// somewhere else — which is a design error the `unmap` above
        /// turns into a panic under Miri and no other build can see.
        pub(super) fn take(aligned: usize) -> Option<(usize, usize)> {
            let mut mappings = MAPPINGS.lock().unwrap_or_else(|e| e.into_inner());
            let at = mappings.iter().position(|(a, _, _)| *a == aligned)?;
            let (_, base, over) = mappings.swap_remove(at);
            Some((base, over))
        }
    }

    pub(super) fn unmap(ptr: *mut u8, bytes: usize) {
        // Under Miri the span this caller holds is part of the untrimmed
        // mapping [`map_aligned`] made, and `over > bytes` always — so
        // the `munmap` a caller's own figures describe is the partial one
        // the shim refuses. The whole mapping goes back instead, which is
        // the exact-layout deallocation the shim accepts, and that keeps
        // a read of returned memory an error Miri reports — which is what
        // three tests in
        // `promote::tests::the_reset_reads_no_zero_count_member` are, Miri being
        // their whole regression.
        #[cfg(miri)]
        {
            let _ = bytes;
            let (base, over) =
                whole::take(ptr as usize).expect("unmap of a span this module did not hand out");
            unsafe { munmap(base as *mut c_void, over) };
        }

        #[cfg(not(miri))]
        unsafe {
            munmap(ptr as *mut c_void, bytes)
        };
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_READWRITE: u32 = 0x04;

    unsafe extern "system" {
        fn VirtualAlloc(
            addr: *mut c_void,
            size: usize,
            allocation_type: u32,
            protect: u32,
        ) -> *mut c_void;
        fn VirtualFree(addr: *mut c_void, size: usize, free_type: u32) -> i32;
    }

    /// No trimming: `VirtualAlloc` returns an address that is a multiple of
    /// the allocation granularity, 64 KiB on every Windows this runs on,
    /// which is the alignment the pool asks for. A larger request would
    /// need the same reserve-and-trim dance unix does, so it is refused
    /// rather than silently under-aligned.
    pub(super) fn map_aligned(bytes: usize, align: usize) -> *mut u8 {
        if align > 64 * 1024 {
            return std::ptr::null_mut();
        }

        let base = unsafe {
            VirtualAlloc(
                std::ptr::null_mut(),
                bytes,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };

        base as *mut u8
    }

    /// `MEM_RELEASE` frees the whole reservation, so the size argument
    /// must be zero — the parameter is kept for symmetry with the unix
    /// half and for the caller's own bookkeeping.
    pub(super) fn unmap(ptr: *mut u8, _bytes: usize) {
        unsafe { VirtualFree(ptr as *mut c_void, 0, MEM_RELEASE) };
    }
}
